// SPDX-License-Identifier: MIT OR Apache-2.0

//! A running dsend: the captured output, its FIFO and player, the dlink
//! device and the output's choice (thin glue, run by hand: it needs a
//! sound card and the network). `main.rs` drives it. Its decisions are the
//! library's, each tested: the output's choice
//! ([`dstack_endpoint::outputs::choose`]), the settings, the processor and
//! its switch ([`dsend::source`]), the status and the app command
//! ([`dsend::status`]).
//!
//! dtray's Output menu sends `app output <n>`: the command hook runs on the
//! device's control task, so it only checks the number and hands it to the
//! main thread, which owns the streams (cpal's streams stay on the thread
//! that made them) and hands the new player to the pump.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};

use dlink::{Link, Map};
use dsend::cli::{self, Options};
use dsend::source::{self, Switch, TX_CHANNELS};
use dsend::status::{self, AppView};
use dstack_endpoint::fifo::Fifo;
use dstack_endpoint::host;
use dstack_endpoint::outputs::{self, Output, Refind};
use dstack_endpoint::player::{Player, Stats, fifo_for};
use dstack_endpoint::settings::{self, Settings};
use dstack_endpoint::status::Snapshot;

use crate::audio;

/// The state file of a run: `--state`, else the user's state folder.
pub fn state_of(o: &Options, name: &str) -> PathBuf {
    host::state_file(o.state.as_deref(), cli::APP, name)
}

/// The device name of a run: `--name`, else `<host>-dsend`.
pub fn name_of(o: &Options) -> String {
    host::device_name(o.name.as_deref(), cli::SUFFIX)
}

/// What the status thread and the command hook read: the outputs, the one
/// captured, its FIFO and rate.
struct Shown {
    names: Vec<String>,
    current: usize,
    fifo: Arc<Fifo>,
    in_rate: u32,
    /// The FIFO's `arrived` at the status's last reading (0 for a new
    /// FIFO), for [`status::capturing`].
    seen: u64,
}

/// A running dsend (module doc).
pub struct Session {
    link: Link,
    switch: Switch,
    stats: Arc<Stats>,
    rate: u32,
    devices: Vec<cpal::Device>,
    outputs: Vec<Output>,
    current: usize,
    capturing: Option<audio::Capturing>,
    /// A stream of the capture reported an error ([`audio::start`]).
    failed: Arc<AtomicBool>,
    /// The FIFO's `arrived` at the last wake, and the wakes since it grew.
    arrived: u64,
    stalled: u32,
    shown: Arc<Mutex<Shown>>,
    settings_file: PathBuf,
}

/// What the main thread is asked.
pub enum Event {
    /// Ctrl+C.
    Stop,
    /// dtray's Output menu: capture output `n`.
    Output(usize),
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A started capture of one output and the player that reads it.
struct Opened {
    capturing: audio::Capturing,
    player: Player,
    fifo: Arc<Fifo>,
    in_rate: u32,
    channels: u16,
}

/// Starts the capture of `device` and makes its player for the network's
/// `rate`, counting on in `stats`.
fn open(
    device: &cpal::Device,
    rate: u32,
    stats: &Arc<Stats>,
    failed: &Arc<AtomicBool>,
) -> Result<Opened, String> {
    let config = audio::config_of(device)?;
    let in_rate = config.sample_rate();
    let channels = config.channels();
    let fifo = Arc::new(fifo_for(usize::from(channels), in_rate));
    let player = Player::new(Arc::clone(&fifo), in_rate, rate, Arc::clone(stats))
        .map_err(|e| format!("the rate conversion {in_rate} to {rate} Hz: {e}"))?;
    let capturing = audio::start(device, &config, Arc::clone(&fifo), failed)?;
    tracing::info!(
        in_rate,
        channels,
        format = %config.sample_format(),
        "dsend: capturing the output (its mix format) with a silent keep-alive stream"
    );
    Ok(Opened {
        capturing,
        player,
        fifo,
        in_rate,
        channels,
    })
}

impl Session {
    /// Picks the output (`--output`, the saved one, the default), starts
    /// its capture and the device. dtray's output choices come to `events`.
    pub fn start(o: &Options, events: Sender<Event>) -> Result<Session, String> {
        let name = name_of(o);
        let state = state_of(o, &name);
        if let Some(dir) = state.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("the state folder {}: {e}", dir.display()))?;
        }
        let settings_file = settings::path_for(&state);
        let saved = settings::load(&settings_file).unwrap_or_else(|e| {
            tracing::warn!("dsend: {e}; starting without it");
            Settings::default()
        });
        let (devices, outputs) = audio::outputs()?;
        let (current, note) =
            outputs::choose(&outputs, o.output.as_deref(), saved.output.as_deref())?;
        if let Some(note) = note {
            tracing::warn!("dsend: {note}");
        }
        let place = match &o.place {
            Some(p) => p.clone(),
            None => {
                let p = host::routed_ipv4()?;
                tracing::info!(
                    place = ?p,
                    "dsend: no --ip or --iface, so the default route's address (name the Dante NIC with --ip)"
                );
                p
            }
        };
        let stats: Arc<Stats> = Arc::default();
        let failed = Arc::new(AtomicBool::new(false));
        let first = open(&devices[current], o.rate, &stats, &failed)?;
        tracing::info!(output = %outputs[current].name, "dsend: the output captured");
        let (processor, switch) = source::source(first.player);
        let link = Link::start(
            &cli::link_config(o, &name, state.clone()),
            &place,
            Map {
                rx: vec![],
                tx: (0..TX_CHANNELS).collect(),
            },
            processor,
        )
        .map_err(|e| format!("the Dante device {name} did not start: {e}"))?;
        tracing::info!(name, state = %state.display(), "dsend: the Dante device runs");
        let shown = Arc::new(Mutex::new(Shown {
            names: outputs.iter().map(|o| o.name.clone()).collect(),
            current,
            fifo: first.fifo,
            in_rate: first.in_rate,
            seen: 0,
        }));
        let session = Session {
            link,
            switch,
            stats,
            rate: o.rate,
            devices,
            outputs,
            current,
            capturing: Some(first.capturing),
            failed,
            arrived: 0,
            stalled: 0,
            shown,
            settings_file,
        };
        session.hooks(events);
        Ok(session)
    }

    /// Captures output `index` of the list: its capture starts, the pump
    /// takes its player at the next block, then the old capture stops. On
    /// an error the old capture goes on.
    fn capture(&mut self, index: usize) -> Result<(), String> {
        let device = self
            .devices
            .get(index)
            .ok_or_else(|| format!("no output {index}"))?;
        let opened = open(device, self.rate, &self.stats, &self.failed)?;
        // Refused (the pump has not taken the last one, or ended): the new
        // capture stops as `opened` drops.
        self.switch.send(opened.player)?;
        self.capturing = Some(opened.capturing);
        self.failed.store(false, Ordering::Relaxed);
        (self.arrived, self.stalled) = (0, 0);
        self.current = index;
        {
            let mut s = lock(&self.shown);
            s.current = index;
            s.fifo = opened.fifo;
            s.in_rate = opened.in_rate;
            s.seen = 0;
        }
        tracing::info!(
            output = %self.outputs[index].name,
            in_rate = opened.in_rate,
            channels = opened.channels,
            "dsend: now capturing this output"
        );
        Ok(())
    }

    /// Once a wake: frees the players the pump replaced (on this thread,
    /// not the pump's), and opens the captured output again when a stream
    /// failed or the capture stalled ([`status::reopen_due`]).
    pub fn watch(&mut self) {
        self.switch.collect();
        let now = lock(&self.shown).fifo.arrived();
        self.stalled = if now > self.arrived {
            0
        } else {
            self.stalled.saturating_add(1)
        };
        self.arrived = now;
        let failed = self.failed.swap(false, Ordering::Relaxed);
        if status::reopen_due(failed, self.stalled) {
            tracing::warn!(
                output = %self.outputs[self.current].name,
                failed,
                stalled_s = self.stalled,
                "dsend: the capture stopped: opening the output again"
            );
            if let Err(e) = self.capture(self.current) {
                tracing::warn!("dsend: the output did not open again: {e}");
            }
        }
    }

    /// dtray's choice: captures output `index` and remembers it.
    pub fn choose_output(&mut self, index: usize) -> Result<(), String> {
        self.capture(index)?;
        let s = Settings {
            output: Some(self.outputs[index].name.clone()),
        };
        settings::save(&self.settings_file, &s)
    }

    /// The app's hooks: its status for dtray ([`status::app_status`]) and
    /// its command (`output <n> [<name>]`, [`status::parse_command`]),
    /// handed to `events`.
    fn hooks(&self, events: Sender<Event>) {
        let (stats, shown) = (Arc::clone(&self.stats), Arc::clone(&self.shown));
        let seen = AtomicU64::new(0);
        self.link.set_app_status(move || {
            let mut s = lock(&shown);
            let snap = Snapshot::read(&s.fifo, &stats, s.in_rate);
            let before = seen.swap(snap.underruns, Ordering::Relaxed);
            let now = s.fifo.arrived();
            let capturing = status::capturing(std::mem::replace(&mut s.seen, now), now);
            status::app_status(&AppView {
                snap: &snap,
                underruns_before: before,
                capturing,
                in_rate: s.in_rate,
                outputs: &s.names,
                current: s.current,
            })
        });
        let (shown, events) = (Arc::clone(&self.shown), Mutex::new(events));
        self.link.set_app_command(move |words| {
            let n = status::parse_command(words, &lock(&shown).names)?;
            lock(&events)
                .send(Event::Output(n))
                .map_err(|_| "dsend is stopping".to_string())?;
            Ok(String::new())
        });
    }

    /// Reads the output devices again (one plugged in or out, a format
    /// changed); true when the list changed. The one captured goes on,
    /// found again by name ([`outputs::refind`]) and opened again when its
    /// format changed; one that is gone falls back to the default; with no
    /// output left the old list stays (and the capture's watch retries).
    pub fn refresh_outputs(&mut self) -> bool {
        let Ok((devices, outputs)) = audio::outputs() else {
            return false;
        };
        if outputs == self.outputs {
            return false;
        }
        let captured = self.outputs[self.current].clone();
        let (index, reopen) = match outputs::refind(&captured, &outputs) {
            Refind::Same { index, changed } => (index, changed),
            Refind::Default(index) => {
                tracing::warn!(
                    output = %captured.name,
                    default = %outputs[index].name,
                    "dsend: the output captured is gone: capturing the default"
                );
                (index, true)
            }
            Refind::Gone => {
                tracing::warn!(
                    output = %captured.name,
                    "dsend: the output captured is gone and there is no default: waiting"
                );
                return true;
            }
        };
        self.devices = devices;
        self.outputs = outputs;
        self.current = index;
        {
            let mut s = lock(&self.shown);
            s.names = self.outputs.iter().map(|o| o.name.clone()).collect();
            s.current = index;
        }
        if reopen && let Err(e) = self.capture(index) {
            tracing::warn!(output = %self.outputs[index].name, "dsend: the output: {e}");
        }
        true
    }

    /// The status line (the console run's log).
    pub fn line(&self) -> String {
        let s = lock(&self.shown);
        format!(
            "{} at {} Hz: {}",
            self.outputs
                .get(self.current)
                .map_or("no output", |o| o.name.as_str()),
            s.in_rate,
            status::line(&Snapshot::read(&s.fifo, &self.stats, s.in_rate))
        )
    }

    /// Why the device ended by itself, if it did.
    pub fn ended(&self) -> Option<String> {
        self.link.ended()
    }

    /// Stops the device (its goodbyes sent), then the capture.
    pub fn stop(mut self) {
        let stopped = self.link.stop();
        self.capturing = None;
        tracing::info!(?stopped, "dsend: stopped");
    }
}
