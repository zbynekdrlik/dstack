// SPDX-License-Identifier: MIT OR Apache-2.0

//! The latency menu (pure, every platform): the values a device offers,
//! as Dante Controller lists them, within the range its ARC settings give
//! ([`dstack::arc::control::Settings`]: `latency_min_ns`,
//! `latency_max_ns`), the configured one checked. The tray menu shows
//! them and writes a choice with [`dstack::arc::control::set_latency`];
//! `dtray --set-latency <device> <ms>` writes the same way from the
//! command line, any value in ms inside that range, not only the menu's
//! (dstack #213: [`ns_of_ms`], [`pick`], [`read_back`]; [`crate::set`]).

use dstack::arc::control::Settings;

/// Dante's latency values, ns: 0.25, 0.5, 1, 2, 5 and 10 ms.
pub const DANTE_NS: [u32; 6] = [
    250_000, 500_000, 1_000_000, 2_000_000, 5_000_000, 10_000_000,
];

/// One value of the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub ns: u32,
    /// As the menu shows it (`0.25 ms`).
    pub label: String,
    /// The device's configured latency.
    pub checked: bool,
}

/// `ns` as the menu shows it: milliseconds, no trailing zeros
/// (`0.25 ms`, `1 ms`).
pub fn label(ns: u32) -> String {
    let whole = ns / 1_000_000;
    let frac = ns % 1_000_000;
    if frac == 0 {
        return format!("{whole} ms");
    }
    let digits = format!("{frac:06}");
    format!("{whole}.{} ms", digits.trim_end_matches('0'))
}

/// The menu of a device with `settings`: each of [`DANTE_NS`] from its
/// minimum to its maximum, both included, the configured one checked.
/// None when the device names no range (no value is guessed).
pub fn choices(settings: &Settings) -> Vec<Choice> {
    let (Some(min), Some(max)) = (settings.latency_min_ns, settings.latency_max_ns) else {
        return Vec::new();
    };
    DANTE_NS
        .iter()
        .filter(|&&ns| min <= ns && ns <= max)
        .map(|&ns| Choice {
            ns,
            label: label(ns),
            checked: settings.latency_configured_ns == Some(ns),
        })
        .collect()
}

/// The latency `ms` names in milliseconds, ns, rounded to whole µs (half
/// up at the fourth decimal): ASCII digits, then optionally a point and
/// more digits (`0.333`, `2`, `1.0`; the menu's labels without " ms" name
/// the same values as the menu). `None` for anything else (a sign, a
/// space, a unit, `.5`, `1.`) and for a value past a `u32` of ns.
pub fn ns_of_ms(ms: &str) -> Option<u32> {
    let (whole, frac) = match ms.split_once('.') {
        Some((whole, frac)) => (whole, Some(frac)),
        None => (ms, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || !frac.is_none_or(digits) {
        return None;
    }
    let frac = frac.unwrap_or("").as_bytes();
    let digit = |i: usize| frac.get(i).map_or(0, |&b| u64::from(b - b'0'));
    let part = digit(0) * 100 + digit(1) * 10 + digit(2) + u64::from(digit(3) >= 5);
    let us = whole
        .parse::<u64>()
        .ok()?
        .checked_mul(1000)?
        .checked_add(part)?;
    u32::try_from(us.checked_mul(1000)?).ok()
}

/// The latency `ms` names ([`ns_of_ms`]) when it lies in the range the
/// device states in `settings` (`latency_min_ns` to `latency_max_ns`, both
/// included: any value there, not only Dante's, dstack #213); the reason
/// otherwise: the device names no range, `ms` is no value, or it is
/// outside the range (the error names the range).
pub fn pick(settings: &Settings, ms: &str) -> Result<u32, String> {
    let (Some(min), Some(max)) = (settings.latency_min_ns, settings.latency_max_ns) else {
        return Err("the device names no latency range; nothing written".to_string());
    };
    let ns = ns_of_ms(ms).ok_or_else(|| {
        format!("{ms} is not a latency in ms (such as 0.5 or 0.333); nothing written")
    })?;
    if (min..=max).contains(&ns) {
        Ok(ns)
    } else {
        Err(format!(
            "{} is outside the latency range the device states ({} to {}); nothing written",
            label(ns),
            label(min),
            label(max)
        ))
    }
}

/// The verdict on a write of `asked` from the configured latency the
/// device reads back after it: the line when they are equal, the reason
/// otherwise (a value read back as the menu shows it, or none).
pub fn read_back(asked: u32, read: Option<u32>) -> Result<String, String> {
    match read {
        Some(ns) if ns == asked => Ok(format!(
            "the latency is set to {}; it reads back {}",
            label(asked),
            label(ns)
        )),
        Some(ns) => Err(format!(
            "the latency {} was written, but it reads back {}",
            label(asked),
            label(ns)
        )),
        None => Err(format!(
            "the latency {} was written, but the device reads back no configured latency",
            label(asked)
        )),
    }
}

#[cfg(test)]
mod tests;
