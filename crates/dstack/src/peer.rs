// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! `dstack-dev peer` (dstack #305, obs#57): a TEST Dante device for an
//! application's CI. A runner has no Dante network and no grandmaster, so
//! the peer runs on the TEST system clock (optionally skewed,
//! [`crate::clock::skewed`]) on a dummy interface, beside the app's own
//! device:
//!
//! - its TX channels each send a sine ([`tone::tone_at`], one frequency a
//!   channel), for the app to receive;
//! - its RX channels are subscribed to the app's TX channels, and each one's
//!   tone is checked for continuity ([`tone::ToneCheck`]: `tone_gaps`);
//! - a JSON status file, rewritten once a second ([`status_json`]): the
//!   peer's own part and the device's status (`ctl status`: the RX
//!   channels' counters, the pump's `play_at_steps` / `play_at_max_dev_us`).
//!
//! The device is an [`Embedded`] one (an app's own device, its processor
//! [`Tones`]) on the `normal` timing profile. Setup: `docs/ci-peer.md`.

pub mod args;
pub mod tone;

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use dstack_audio_io::{Block, Process};
use tracing::{error, info};

use crate::clock::TestClockPpm;
use crate::daemon::PlaceWait;
use crate::daemon::media::Media;
use crate::device::DeviceConfig;
use crate::embed::Embedded;
use crate::host::{self, ClockSource, HostError, Stopped};
use crate::stream::Map;
use args::{Parsed, PeerArgs};
use tone::{RxTone, ToneCheck};

/// How long the stop waits for the goodbyes.
pub const STOP_WAIT: Duration = Duration::from_secs(5);
/// The pump's block, frames (dlink's).
const BLOCK: u64 = 128;
/// The pump's margin W, ns (dlink's default with TX).
const WORK_MARGIN_NS: u64 = 6_000_000;

/// The peer's device config: `normal` timing, 24-bit, the args' channels,
/// rate, latency, ports and RX subscriptions, the skew as `test_clock_ppm`.
pub fn device_config(a: &PeerArgs) -> Result<DeviceConfig, String> {
    let mut t = toml::Table::new();
    t.insert("name".into(), a.name.clone().into());
    t.insert("manufacturer".into(), "dstack".into());
    t.insert("model".into(), "dstack-peer".into());
    t.insert("sample_rate".into(), i64::from(a.rate).into());
    t.insert("encoding".into(), 24.into());
    t.insert("latency_ns".into(), int(a.latency_ns).into());
    t.insert("block".into(), int(BLOCK).into());
    let margin = WORK_MARGIN_NS * u64::from(a.rate) / 1_000_000_000;
    t.insert("work_margin".into(), int(margin).into());
    t.insert("timing".into(), "normal".into());
    t.insert(
        "state_file".into(),
        a.state.to_string_lossy().into_owned().into(),
    );
    if let Some((arc, cmc, flows)) = a.ports {
        t.insert("arc_port".into(), i64::from(arc).into());
        t.insert("cmc_port".into(), i64::from(cmc).into());
        t.insert("flows_port".into(), i64::from(flows).into());
    }
    t.insert("process_id".into(), i64::from(a.process_id).into());
    if let Some(ppm) = a.test_clock_ppm {
        t.insert("test_clock_ppm".into(), ppm.0.into());
    }
    let rx: Vec<String> = rx_names(a).collect();
    if let Some((device, channels)) = &a.rx_from {
        let subs: Vec<toml::Value> = channels
            .iter()
            .enumerate()
            .map(|(i, ch)| {
                let mut s = toml::Table::new();
                s.insert("rx".into(), int(i as u64 + 1).into());
                s.insert("tx_channel".into(), ch.clone().into());
                s.insert("tx_device".into(), device.clone().into());
                s.into()
            })
            .collect();
        t.insert("subscribe".into(), subs.into());
    }
    t.insert("rx".into(), rx.into());
    let tx: Vec<String> = (1..=a.tones.len()).map(|n| format!("TX {n}")).collect();
    t.insert("tx".into(), tx.into());
    let text = toml::to_string(&t).map_err(|e| e.to_string())?;
    DeviceConfig::from_toml(&text).map_err(|e| e.to_string())
}

fn int(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// The RX channels' names: one per `--rx-from` channel.
fn rx_names(a: &PeerArgs) -> impl Iterator<Item = String> {
    let n = a.rx_from.as_ref().map_or(0, |(_, ch)| ch.len());
    (1..=n).map(|i| format!("RX {i}"))
}

/// What the processor shares with the status.
#[derive(Debug, Default)]
pub struct Shared {
    /// Each RX channel's tone.
    pub rx: Box<[RxTone]>,
    /// Blocks run with a media start.
    pub blocks: AtomicU64,
    /// The play times' rate against the media time, ppm, as `f64` bits
    /// ([`play_at_ppm`]; NaN before two blocks with one).
    pub play_at_ppm: AtomicU64,
}

/// The rate of the media clock against the app's that the blocks' play
/// times show, ppm: the media time from the first block to the last over
/// the play time between them, less one. `None` before they are apart.
pub fn play_at_ppm(first: (u64, i64), last: (u64, i64), rate: u32) -> Option<f64> {
    let media = (last.0 as f64 - first.0 as f64) * 1e9 / f64::from(rate.max(1));
    let played = (last.1 - first.1) as f64;
    (played > 0.0).then(|| (media / played - 1.0) * 1e6)
}

/// The peer's processor: the TX tones out, the RX tones checked.
pub struct Tones {
    tones: Vec<u32>,
    rate: u32,
    checks: Vec<ToneCheck>,
    samples: u64,
    first: Option<(u64, i64)>,
    /// Where the next block starts on the media grid.
    next: Option<u64>,
    shared: Arc<Shared>,
}

impl Tones {
    pub fn new(tones: Vec<u32>, rx: usize, rate: u32) -> Tones {
        let shared = Shared {
            rx: (0..rx).map(|_| RxTone::default()).collect(),
            blocks: AtomicU64::new(0),
            play_at_ppm: AtomicU64::new(f64::NAN.to_bits()),
        };
        Tones {
            tones,
            rate,
            checks: vec![ToneCheck::default(); rx],
            samples: 0,
            first: None,
            next: None,
            shared: Arc::new(shared),
        }
    }

    pub fn shared(&self) -> Arc<Shared> {
        Arc::clone(&self.shared)
    }
}

impl Process for Tones {
    fn process(&mut self, block: &mut Block<'_>) {
        let frames = block.frames();
        for (ch, check) in self.checks.iter_mut().enumerate() {
            check.block(block.input(ch));
        }
        self.samples += frames as u64;
        for (check, out) in self.checks.iter().zip(self.shared.rx.iter()) {
            check.publish(out, self.samples, self.rate);
        }
        let Some(start) = block.start() else {
            return;
        };
        self.shared.blocks.fetch_add(1, Ordering::Relaxed);
        for (ch, &hz) in self.tones.iter().enumerate() {
            for (i, y) in block.output(ch).iter_mut().enumerate() {
                *y = tone::tone_at(start + i as u64, hz, self.rate);
            }
        }
        // A start off the last block's end (a pump restart, a re-anchor)
        // starts the play-time rate afresh.
        if self.next.replace(start + frames as u64) != Some(start) {
            self.first = None;
        }
        if let Some(at) = block.play_at() {
            let first = *self.first.get_or_insert((start, at));
            if let Some(ppm) = play_at_ppm(first, (start, at), self.rate) {
                self.shared
                    .play_at_ppm
                    .store(ppm.to_bits(), Ordering::Relaxed);
            }
        }
    }
}

/// The peer running.
pub struct Peer {
    device: Embedded,
    shared: Arc<Shared>,
    args: PeerArgs,
}

impl Peer {
    /// Starts the peer's device on the TEST system clock, waiting for its
    /// place with no limit (a dummy interface still getting its address).
    pub fn start(args: PeerArgs) -> Result<Peer, HostError> {
        let cfg = device_config(&args).map_err(HostError::Setup)?;
        let tones = Tones::new(args.tones.clone(), cfg.rx.len(), args.rate);
        let shared = tones.shared();
        let map = Map {
            rx: (0..cfg.rx.len()).collect(),
            tx: (0..cfg.tx.len()).collect(),
        };
        let device = Embedded::start_waiting(
            cfg,
            &args.place,
            PlaceWait::Forever,
            map,
            tones,
            ClockSource::System,
            None,
        )?;
        Ok(Peer {
            device,
            shared,
            args,
        })
    }

    pub fn media(&self) -> &Arc<Media> {
        self.device.media()
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    /// The status JSON ([`status_json`]).
    pub fn status(&self) -> String {
        status_json(
            &self.args,
            &self.shared,
            &crate::ctl::status(self.device.media()),
        )
    }

    /// Why the device ended by itself, if it did.
    pub fn ended(&self) -> Option<String> {
        self.device.ended()
    }

    /// Stops the pump and the device, its goodbyes sent.
    pub fn stop(self) -> Stopped {
        self.device.stop(STOP_WAIT)
    }
}

/// The status: `peer` (its name, `test_clock_ppm` or null, `tx`: each
/// channel's `tone_hz`; `rx`: each channel's `from` (device:channel),
/// `locked`, `tone_hz` and `level_dbfs` (the last clean block's; null
/// before a lock), `samples`, `tone_gaps`; `blocks`, `play_at_ppm` (null
/// before two blocks)) and `device` (the device's own status, as `ctl
/// status`; a status that does not parse goes in as its text).
pub fn status_json(a: &PeerArgs, shared: &Shared, device: &str) -> String {
    use serde_json::{Value, json};
    let finite = |x: f64| x.is_finite().then_some(x);
    let tx: Vec<Value> = a
        .tones
        .iter()
        .enumerate()
        .map(|(i, hz)| json!({"channel": format!("TX {}", i + 1), "tone_hz": hz}))
        .collect();
    let rx: Vec<Value> = shared
        .rx
        .iter()
        .zip(rx_names(a))
        .enumerate()
        .map(|(i, (tone, channel))| {
            let v = tone.view();
            let from = a
                .rx_from
                .as_ref()
                .map(|(dev, chs)| format!("{dev}:{}", chs[i]));
            json!({
                "channel": channel,
                "from": from,
                "locked": v.locked,
                "tone_hz": v.locked.then_some(v.hz),
                "level_dbfs": v.locked.then(|| 20.0 * v.amplitude.log10()),
                "samples": v.samples,
                "tone_gaps": v.gaps,
            })
        })
        .collect();
    let device: Value =
        serde_json::from_str(device).unwrap_or_else(|_| Value::String(device.to_string()));
    json!({
        "peer": {
            "name": a.name,
            "version": crate::daemon::VERSION,
            "test_clock_ppm": a.test_clock_ppm.map(|p: TestClockPpm| p.0),
            "tx": tx,
            "rx": rx,
            "blocks": shared.blocks.load(Ordering::Relaxed),
            "play_at_ppm": finite(f64::from_bits(shared.play_at_ppm.load(Ordering::Relaxed))),
        },
        "device": device,
    })
    .to_string()
}

/// Writes `text` to `path` whole: a temporary file beside it, renamed.
pub fn write_status(path: &Path, text: &str) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// `dstack-dev peer <args>`: runs the peer until SIGINT or SIGTERM,
/// writing its status once a second. Exit 0 after a signal and the
/// goodbyes; 2 for the command line or the config; 1 when the device
/// failed or the status cannot be written.
pub fn main(args: &[String]) -> ExitCode {
    let a = match args::parse(args) {
        Ok(Parsed::Help) => {
            println!("{}", args::USAGE);
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Run(a)) => *a,
        Err(why) => {
            eprintln!("dstack-dev peer: {why}\n\n{}", args::USAGE);
            return ExitCode::from(2);
        }
    };
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let status = a.status.clone();
    let peer = match Peer::start(a) {
        Ok(p) => p,
        Err(e) => {
            error!("{e}");
            return ExitCode::from(host::exit_code(&e));
        }
    };
    info!(status = %status.display(), "dstack-dev peer: running; the status once a second");
    let served = run_until_signal(&peer, &status);
    let stopped = peer.stop();
    match (served, stopped) {
        (Ok(()), Stopped::Done) => ExitCode::SUCCESS,
        (Err(why), _) => {
            error!("{why}");
            ExitCode::from(1)
        }
        (Ok(()), other) => {
            error!("dstack-dev peer: the stop: {other:?}");
            ExitCode::from(1)
        }
    }
}

/// Writes the status once a second until SIGINT or SIGTERM (Ok), the
/// device's own end or a status write's failure (Err).
fn run_until_signal(peer: &Peer, status: &Path) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("dstack-dev peer: no runtime: {e}"))?;
    rt.block_on(async {
        // Both signals' listeners are made once, before the loop: a signal
        // that comes while the status is written is kept for the next
        // select, never lost.
        #[cfg(unix)]
        let (mut term, mut int) = {
            use tokio::signal::unix::{SignalKind, signal};
            let listen = |kind: SignalKind, name: &str| {
                signal(kind).map_err(|e| format!("dstack-dev peer: {name}: {e}"))
            };
            (
                listen(SignalKind::terminate(), "SIGTERM")?,
                listen(SignalKind::interrupt(), "SIGINT")?,
            )
        };
        #[cfg(not(unix))]
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(not(unix))]
        tokio::pin!(ctrl_c);
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            #[cfg(unix)]
            let signalled = async {
                tokio::select! {
                    _ = term.recv() => (),
                    _ = int.recv() => (),
                }
            };
            #[cfg(not(unix))]
            let signalled = async {
                let _ = (&mut ctrl_c).await;
            };
            tokio::select! {
                () = signalled => return Ok(()),
                _ = tick.tick() => {
                    if let Some(why) = peer.ended() {
                        return Err(format!("dstack-dev peer: the device ended: {why}"));
                    }
                    write_status(status, &peer.status()).map_err(|e| {
                        format!("dstack-dev peer: the status {}: {e}", status.display())
                    })?;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests;
