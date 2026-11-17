// SPDX-License-Identifier: MIT OR Apache-2.0

//! The bridge's ASRC seed (dstack #382, dasio#120): the rates the two
//! players' rate fits last locked on, kept in a small file the app names
//! (dasio: one per bridge instance), so the next run starts from them.
//!
//! Without it each run's ASRC starts at 0 ppm and a driver 7.5 ppm off
//! Dante drifts the FIFO's depth about 0.45 ms before the fit locks at
//! 60 s (dasio's measurement on dsg); the target must carry that margin
//! for the first minute. Seeded, the correction starts at the last locked
//! rate and the fit takes over once it locks ([`dlink::bridge::Player::with_seed_ppm`]).
//!
//! The file is a few `key = value` lines (`#` starts a comment):
//!
//! ```text
//! # dstack-asio bridge: the ASRC's last locked rates (dstack #382)
//! dante_rate = 96000
//! driver_rate = 96000
//! to_asio_ppm = 7.512
//! from_asio_ppm = -7.498
//! ```
//!
//! A seed is used only for the same Dante rate and driver rate it was
//! locked at ([`Seed::for_rates`]). An app reads it once at its start
//! ([`Seed::read`], [`super::Bridge::seed_from`]) and writes
//! [`super::Bridge::seed`] when it changed, from its own loop (never the
//! driver's thread: [`Seed::write`] is a file write).

use std::path::Path;

/// The largest seed taken, ppm: the ASRC controller's bound
/// (`dstack_resample::asrc::MAX_PPM`).
pub const MAX_SEED_PPM: f64 = dstack_resample::asrc::MAX_PPM;

/// The two players' last locked rates (module doc).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seed {
    /// The Dante device's rate, Hz.
    pub dante_rate: u32,
    /// The driver's rate the rates were locked at, Hz.
    pub driver_rate: u32,
    /// The to-ASIO player's locked rate, ppm.
    pub to_asio_ppm: Option<f64>,
    /// The from-ASIO player's locked rate, ppm.
    pub from_asio_ppm: Option<f64>,
}

impl Seed {
    /// This seed for a run at `dante_rate` and `driver_rate`: itself when
    /// both are the rates it was locked at, else none.
    pub fn for_rates(self, dante_rate: u32, driver_rate: u32) -> Option<Seed> {
        (self.dante_rate == dante_rate && self.driver_rate == driver_rate).then_some(self)
    }

    /// The file's text (module doc).
    pub fn text(&self) -> String {
        let mut t = format!(
            "# dstack-asio bridge: the ASRC's last locked rates (dstack #382)\n\
             dante_rate = {}\ndriver_rate = {}\n",
            self.dante_rate, self.driver_rate
        );
        for (key, ppm) in [
            ("to_asio_ppm", self.to_asio_ppm),
            ("from_asio_ppm", self.from_asio_ppm),
        ] {
            if let Some(ppm) = ppm {
                t.push_str(&format!("{key} = {ppm}\n"));
            }
        }
        t
    }

    /// A seed from the file's text; why not, when the text is not one
    /// (an unknown key, a missing rate, a rate past [`MAX_SEED_PPM`]).
    pub fn parse(text: &str) -> Result<Seed, String> {
        let mut dante_rate = None;
        let mut driver_rate = None;
        let mut to_asio_ppm = None;
        let mut from_asio_ppm = None;
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(format!("line {}: no `key = value`", n + 1));
            };
            let (key, value) = (key.trim(), value.trim());
            match key {
                "dante_rate" => dante_rate = Some(rate(key, value)?),
                "driver_rate" => driver_rate = Some(rate(key, value)?),
                "to_asio_ppm" => to_asio_ppm = Some(ppm(key, value)?),
                "from_asio_ppm" => from_asio_ppm = Some(ppm(key, value)?),
                _ => return Err(format!("line {}: unknown key `{key}`", n + 1)),
            }
        }
        match (dante_rate, driver_rate) {
            (Some(dante_rate), Some(driver_rate)) => Ok(Seed {
                dante_rate,
                driver_rate,
                to_asio_ppm,
                from_asio_ppm,
            }),
            _ => Err("no dante_rate or driver_rate".to_string()),
        }
    }

    /// The seed in the file at `path`: `None` when there is no file yet;
    /// an error for a file that cannot be read or is no seed (the app logs
    /// it and starts unseeded).
    pub fn read(path: &Path) -> Result<Option<Seed>, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Seed::parse(&text)
                .map(Some)
                .map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Writes the seed to `path`, whole: a temporary file beside it, synced
    /// to disk, then renamed over it (neither a crash nor a power loss
    /// leaves half a file; at worst the last seed stays).
    pub fn write(&self, path: &Path) -> Result<(), String> {
        use std::io::Write;
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = std::path::PathBuf::from(tmp);
        std::fs::File::create(&tmp)
            .and_then(|mut f| {
                f.write_all(self.text().as_bytes())?;
                f.sync_all()
            })
            .and_then(|()| std::fs::rename(&tmp, path))
            .map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// A rate, Hz, 1 or more.
fn rate(key: &str, value: &str) -> Result<u32, String> {
    match value.parse::<u32>() {
        Ok(r) if r > 0 => Ok(r),
        _ => Err(format!("{key}: `{value}` is no rate")),
    }
}

/// A locked rate, ppm, within ±[`MAX_SEED_PPM`].
fn ppm(key: &str, value: &str) -> Result<f64, String> {
    match value.parse::<f64>() {
        Ok(p) if p.abs() <= MAX_SEED_PPM => Ok(p),
        _ => Err(format!(
            "{key}: `{value}` is no rate within ±{MAX_SEED_PPM} ppm"
        )),
    }
}

#[cfg(test)]
mod tests;
