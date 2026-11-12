// SPDX-License-Identifier: MIT OR Apache-2.0

//! A running dlisten: the dlink device, the FIFO, the output and its
//! choice (thin glue, run by hand: it needs a sound card and the network).
//! `main.rs` drives it. Its decisions are the library's, each tested: the
//! output's choice ([`outputs::choose`]), the settings ([`settings`]), the
//! status and the app command ([`status`]).
//!
//! dtray shows it: the app status carries the outputs, and dtray's Output
//! menu sends `app output <n>`. The command hook runs on the device's
//! control task, so it only checks the number and hands it to the main
//! thread ([`Session::start`]'s receiver), which owns the output stream
//! (cpal's stream stays on the thread that made it).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};

use dlink::{Link, Map};
use dlisten::cli::{self, Options};
use dlisten::feed::Feed;
use dlisten::fifo::Fifo;
use dlisten::outputs::{self, Output, Refind};
use dlisten::player::{Player, Stats, fifo_for};
use dlisten::settings::{self, Settings};
use dlisten::status::{self, AppView, Snapshot};
use dstack_endpoint::host;

use crate::audio;

/// The state file of a run: `--state`, else the user's state folder.
pub fn state_of(o: &Options, name: &str) -> PathBuf {
    o.state.clone().unwrap_or_else(|| {
        cli::state_path(
            cfg!(windows),
            name,
            std::env::var_os("LOCALAPPDATA"),
            std::env::var_os("XDG_STATE_HOME"),
            std::env::var_os("HOME"),
            std::env::temp_dir(),
        )
    })
}

/// The device name of a run: `--name`, else `<host>-dlisten`.
pub fn name_of(o: &Options) -> String {
    o.name
        .clone()
        .unwrap_or_else(|| cli::default_name(&host::host_name()))
}

/// A running dlisten (module doc).
pub struct Session {
    link: Link,
    fifo: Arc<Fifo>,
    stats: Arc<Stats>,
    rate: u32,
    /// `--mono` ([`dlisten::map::source_mono`]).
    mono: bool,
    devices: Vec<cpal::Device>,
    outputs: Vec<Output>,
    current: usize,
    playing: Option<audio::Playing>,
    /// The outputs' names and the one playing, for the app status's thread
    /// and the command hook.
    shown: Arc<Mutex<(Vec<String>, usize)>>,
    settings_file: PathBuf,
    /// The output must be opened again (its format changed, or the
    /// default took over) and the last try failed: tried each refresh.
    replay: bool,
    /// No output is left (logged once until one comes back).
    waiting: bool,
    /// A stream reported an error ([`audio::open`]): opened again at the
    /// next refresh.
    failed: Arc<AtomicBool>,
}

/// What the main thread is asked.
pub enum Event {
    /// Ctrl+C.
    Stop,
    /// dtray's Output menu: play on output `n`.
    Output(usize),
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Session {
    /// Picks the output (`--output`, the saved one, the default), starts
    /// the device and plays. dtray's output choices come to `events`.
    pub fn start(o: &Options, events: Sender<Event>) -> Result<Session, String> {
        let name = name_of(o);
        let state = state_of(o, &name);
        if let Some(dir) = state.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("the state folder {}: {e}", dir.display()))?;
        }
        let settings_file = settings::path_for(&state);
        let saved = settings::load(&settings_file).unwrap_or_else(|e| {
            tracing::warn!("dlisten: {e}; starting without it");
            Settings::default()
        });
        let (devices, outputs) = audio::outputs()?;
        let (current, note) = outputs::choose(
            &outputs,
            o.output.as_deref(),
            saved.output.as_deref(),
            outputs::Fallback::Default,
        )?;
        if let Some(note) = note {
            tracing::warn!("dlisten: {note}");
        }
        let place = match &o.place {
            Some(p) => p.clone(),
            None => {
                let p = host::routed_ipv4()?;
                tracing::info!(
                    place = ?p,
                    "dlisten: no --ip or --iface, so the default route's address (name the Dante NIC with --ip)"
                );
                p
            }
        };
        let fifo = Arc::new(fifo_for(o.channels, o.rate));
        let free = with_free_ports(o, &place)
            .map_err(|e| format!("the Dante device {name} did not start: {e}"))?;
        let link = Link::start(
            &cli::link_config(&free, &name, state.clone()),
            &place,
            Map {
                rx: (0..o.channels).collect(),
                tx: vec![],
            },
            Feed::new(Arc::clone(&fifo)),
        )
        .map_err(|e| format!("the Dante device {name} did not start: {e}"))?;
        tracing::info!(name, state = %state.display(), "dlisten: the Dante device runs");
        let mut session = Session {
            link,
            fifo,
            stats: Arc::default(),
            rate: o.rate,
            mono: o.mono,
            devices,
            outputs,
            current,
            playing: None,
            shown: Arc::default(),
            settings_file,
            replay: false,
            waiting: false,
            failed: Arc::default(),
        };
        if let Err(e) = session.play(current) {
            session.stop();
            return Err(e);
        }
        session.hooks(events);
        Ok(session)
    }

    /// Plays on output `index` of the list (the old stream stopped first:
    /// the FIFO has one reader).
    fn play(&mut self, index: usize) -> Result<(), String> {
        let device = self
            .devices
            .get(index)
            .ok_or_else(|| format!("no output {index}"))?;
        let config = audio::config_of(device)?;
        let out_rate = config.sample_rate();
        let player = Player::new(
            Arc::clone(&self.fifo),
            self.rate,
            out_rate,
            Arc::clone(&self.stats),
        )
        .map_err(|e| format!("the rate conversion {} to {out_rate} Hz: {e}", self.rate))?
        .with_mono(self.mono);
        // The new stream is opened while the old one plays (a failure
        // leaves the old one playing); the old one stops before the new
        // one starts: the FIFO has one reader.
        let stream = audio::open(device, &config, player, &self.failed)?;
        let was = self.playing.take().map(|_| self.current);
        match audio::start(stream) {
            Ok(p) => self.playing = Some(p),
            Err(e) => {
                if let Some(old) = was.filter(|&old| old != index)
                    && let Err(back) = self.play(old)
                {
                    tracing::warn!("dlisten: the output before did not start again: {back}");
                }
                return Err(e);
            }
        }
        self.current = index;
        self.replay = false;
        tracing::info!(
            output = %self.outputs[index].name,
            out_rate,
            out_channels = config.channels(),
            format = %config.sample_format(),
            "dlisten: playing on the output (its own default config)"
        );
        self.show();
        Ok(())
    }

    /// The outputs and the one playing, for the hooks.
    fn show(&self) {
        let names = self.outputs.iter().map(|o| o.name.clone()).collect();
        *lock(&self.shown) = (names, self.current);
    }

    /// dtray's choice: plays on output `index` and remembers it.
    pub fn choose_output(&mut self, index: usize) -> Result<(), String> {
        self.play(index)?;
        let s = Settings {
            output: Some(self.outputs[index].name.clone()),
        };
        settings::save(&self.settings_file, &s)
    }

    /// The app's hooks: its status for dtray ([`status::app_status`]: the
    /// numbers, whether audio flows, the outputs) and its command
    /// (`output <n> [<name>]`, [`status::parse_command`]), handed to
    /// `events`.
    fn hooks(&self, events: Sender<Event>) {
        let (fifo, stats, shown, media) = (
            Arc::clone(&self.fifo),
            Arc::clone(&self.stats),
            Arc::clone(&self.shown),
            Arc::clone(self.link.media()),
        );
        let (seen, rate) = (AtomicU64::new(0), self.rate);
        let written = Mutex::new(Vec::new());
        self.link.set_app_status(move || {
            let s = Snapshot::read(&fifo, &stats, rate);
            let before = seen.swap(s.underruns, Ordering::Relaxed);
            let channels = media.rx_channels();
            let flowing = {
                let mut w = lock(&written);
                let f = status::flowing(&w, &channels);
                *w = status::written(&channels);
                f
            };
            let (outputs, current) = lock(&shown).clone();
            status::app_status(&AppView {
                snap: &s,
                underruns_before: before,
                flowing,
                outputs: &outputs,
                current,
            })
        });
        let (shown, events) = (Arc::clone(&self.shown), Mutex::new(events));
        self.link.set_app_command(move |words| {
            let n = status::parse_command(words, &lock(&shown).0)?;
            lock(&events)
                .send(Event::Output(n))
                .map_err(|_| "dlisten is stopping".to_string())?;
            Ok(String::new())
        });
    }

    /// Reads the output devices again (one plugged in or out, a format
    /// changed); true when the list changed. The one playing keeps playing,
    /// found again by name ([`outputs::refind`]) and opened again when its
    /// format changed or a stream reported an error; one that is gone falls
    /// back to the default (else the first); with no output left the old
    /// list stays.
    pub fn refresh_outputs(&mut self) -> bool {
        if self.failed.swap(false, Ordering::Relaxed) {
            self.replay = true;
        }
        let Ok((devices, outputs)) = audio::outputs() else {
            return false;
        };
        if outputs == self.outputs {
            // A replay owed (a stream error, a failed opening) takes the
            // devices just read: a device handle may have been re-created.
            if self.replay && !self.waiting {
                self.devices = devices;
                self.replay = self.replay_current();
            }
            return false;
        }
        let playing = self.outputs[self.current].clone();
        let (index, replay) = match outputs::refind(&playing, &outputs) {
            Refind::Same { index, changed } => (index, changed),
            Refind::Default(index) => {
                tracing::warn!(
                    output = %playing.name,
                    default = %outputs[index].name,
                    "dlisten: the output playing is gone: playing on the default (else the first)"
                );
                (index, true)
            }
            Refind::Gone => {
                if !self.waiting {
                    tracing::warn!(output = %playing.name, "dlisten: no output is left: waiting for one");
                }
                self.waiting = true;
                return true;
            }
        };
        self.waiting = false;
        self.devices = devices;
        self.outputs = outputs;
        // The index of the output now in use in the new list (its stream is
        // the old one until it opens again).
        self.current = index;
        self.show();
        if replay {
            self.replay = self.replay_current();
        }
        true
    }

    /// Opens the current output again; true when it failed (to try again).
    fn replay_current(&mut self) -> bool {
        match self.play(self.current) {
            Ok(()) => false,
            Err(e) => {
                tracing::warn!(
                    output = %self.outputs[self.current].name,
                    "dlisten: the output did not open (tried again in 5 s): {e}"
                );
                true
            }
        }
    }

    /// The status line (the console run's log).
    pub fn line(&self) -> String {
        status::line(&Snapshot::read(&self.fifo, &self.stats, self.rate).with_peak(&self.fifo))
    }

    /// Why the device ended by itself, if it did.
    pub fn ended(&self) -> Option<String> {
        self.link.ended()
    }

    /// Stops the output, then the device (its goodbyes sent).
    pub fn stop(mut self) {
        self.playing = None;
        let stopped = self.link.stop();
        tracing::info!(?stopped, "dlisten: stopped");
    }
}

/// `o` with the control ports and process id [`dstack_endpoint::slots::pick`]
/// gives on `place` (a free set when another Dante device on the address
/// holds the defaults; dasio, FOH, 10.11.2026), once `place` is there
/// (`pick_when_there`, dstack #225).
fn with_free_ports(o: &cli::Options, place: &dlink::Place) -> Result<cli::Options, String> {
    let mut o = o.clone();
    (o.ports, o.process_id) =
        dstack_endpoint::slots::pick_when_there(place, o.ports, o.process_id, |p| {
            dlink::wait_for_place(p).map_err(|e| e.to_string())
        })?;
    Ok(o)
}
