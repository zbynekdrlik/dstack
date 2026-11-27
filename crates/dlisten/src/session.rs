// SPDX-License-Identifier: MIT OR Apache-2.0

//! A running dlisten: the dlink device, the FIFO, the output and its
//! choice (thin glue, run by hand: it needs a sound card and the network).
//! `main.rs` drives it. Its decisions are the library's, each tested: the
//! output's choice ([`outputs::choose`]), the settings ([`settings`]), the
//! status and the app command ([`status`]).
//!
//! A run with `--output` plays on that output only ([`follow`], dstack
//! #506): while none matches it plays nothing and waits (a WARN, then a
//! line a minute), and it opens the output when a read of the list finds
//! it; it never falls back to another output.
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
use std::time::Instant;

use cpal::traits::DeviceTrait;
use dlink::bridge::pull::{IdleWatch, PullStats};
use dlink::bridge::{AppBuffer, SharedPull};
use dlink::{Block, Link, Map, Process};
use dlisten::cli::{self, Options};
use dlisten::fifo::Fifo;
use dlisten::follow::{self, Line, MissingLog, Step, Want};
use dlisten::outputs::Output;
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

/// The device's processor: the pump has nothing to do for dlisten, whose
/// output callback pulls the RX channels itself ([`dlink::bridge::RxPull`], dstack #307).
struct Idle;

impl Process for Idle {
    fn process(&mut self, _block: &mut Block<'_>) {}
}

/// A running dlisten (module doc).
pub struct Session {
    link: Link,
    fifo: Arc<Fifo>,
    /// The RX pull (dstack #307): the playing output's callback calls it
    /// before each read of the FIFO. Shared so the next stream of an output
    /// switch takes it over; a callback only tries the lock, so the old
    /// stream's last callback beside the new one's skips a pull, never
    /// waits ([`SharedPull`]).
    pull: Arc<SharedPull>,
    /// The pull's counts, and the main thread's watch over them
    /// ([`Session::keep_pulled`]).
    pull_stats: Arc<PullStats>,
    watch: IdleWatch,
    stats: Arc<Stats>,
    rate: u32,
    /// The app buffer the players keep (dstack #247).
    buffer: AppBuffer,
    /// `--mono` ([`dlisten::map::source_mono`]).
    mono: bool,
    devices: Vec<cpal::Device>,
    outputs: Vec<Output>,
    /// What the run plays on (`--output`, [`follow::Want`]).
    want: Want,
    /// The output in use: `None` while a named run waits for its output
    /// (dstack #506). A free run keeps the last one played here even while
    /// no output is left (`waiting`).
    current: Option<usize>,
    playing: Option<audio::Playing>,
    /// What the app status's thread and the command hook show.
    shown: Arc<Mutex<Shown>>,
    settings_file: PathBuf,
    /// The output must be opened again (its format changed, or the
    /// default took over) and the last try failed: tried each refresh.
    replay: bool,
    /// A free run: no output is left (logged once until one comes back).
    waiting: bool,
    /// A named run with no output that matches: since when, and its log
    /// (a WARN, then a line a minute).
    missing: Option<(Instant, MissingLog)>,
    /// A stream reported an error ([`audio::open`]): opened again at the
    /// next refresh.
    failed: Arc<AtomicBool>,
}

/// The outputs' names, the one playing and the `--output` text waited for,
/// for the app status's thread and the command hook.
#[derive(Default)]
struct Shown {
    names: Vec<String>,
    current: Option<usize>,
    waiting_for: Option<String>,
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
    /// the device and plays; a named output that is not there is waited
    /// for, the device running (dstack #506). dtray's output choices come
    /// to `events`.
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
        let want = Want::of(o.output.as_deref());
        let (current, note) = follow::start(&outputs, &want, saved.output.as_deref())?;
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
        // The FIFO the output callback fills from the RX rings, at the
        // device's rate (dstack #307).
        let rate = cli::link_config(o, &name, state.clone()).device_rate();
        let fifo = Arc::new(fifo_for(o.channels, rate));
        // The app buffer (dstack #247): the output's WASAPI period and one
        // pump block, or `--app-buffer-ms`, as the device logs it.
        let buffer = cli::link_config(o, &name, state.clone())
            .bridge_buffer()
            .ok_or_else(|| "dlisten: the device config states no app buffer".to_string())?;
        let free = with_free_ports(o, &place)
            .map_err(|e| format!("the Dante device {name} did not start: {e}"))?;
        // The place is there now (`with_free_ports` waited for it, dstack
        // #225), so `Link::start`, which fails at once on a place not
        // there (#230), takes it.
        // The output callback pulls the RX channels (dstack #307): it drains
        // the flow sockets and reads the rings by position itself, so a late
        // pump cannot empty the FIFO. While no output plays nothing drains:
        // the flow reads silent and is asked again once one plays
        // (`Link::start_pulling`).
        let (link, pull) = Link::start_pulling(
            &cli::link_config(&free, &name, state.clone()),
            &place,
            Map {
                rx: (0..o.channels).collect(),
                tx: vec![],
            },
            Idle,
            Arc::clone(&fifo),
        )
        .map_err(|e| format!("the Dante device {name} did not start: {e}"))?;
        tracing::info!(name, state = %state.display(), "dlisten: the Dante device runs");
        let mut session = Session {
            link,
            fifo,
            pull_stats: pull.stats(),
            pull: Arc::new(SharedPull::new(pull)),
            watch: IdleWatch::default(),
            stats: Arc::default(),
            rate,
            buffer,
            mono: o.mono,
            devices,
            outputs,
            want,
            current,
            playing: None,
            shown: Arc::default(),
            settings_file,
            replay: false,
            waiting: false,
            missing: None,
            failed: Arc::default(),
        };
        match current {
            Some(index) => {
                if let Err(e) = session.play(index) {
                    if session.named().is_none() {
                        session.stop();
                        return Err(e);
                    }
                    // A named output that is listed but did not open (an
                    // endpoint that just came, at boot): tried again at each
                    // read of the list, never another output.
                    tracing::warn!(
                        output = %session.outputs[index].name,
                        "dlisten: the output did not open (tried again in 5 s): {e}"
                    );
                    session.replay = true;
                    // The wait's log too (WARN once, a line a minute) until
                    // the output really plays.
                    session.missing = session
                        .named()
                        .map(|t| (Instant::now(), MissingLog::new(t)));
                    session.show();
                    session.log_missing();
                }
            }
            None => session.go_missing(),
        }
        session.hooks(events);
        Ok(session)
    }

    /// The `--output` text of a named run.
    fn named(&self) -> Option<&str> {
        match &self.want {
            Want::Named(t) => Some(t),
            Want::Free => None,
        }
    }

    /// A named run's output is not there: nothing plays, the wait begins
    /// (its WARN logged now) unless it already runs.
    fn go_missing(&mut self) {
        self.playing = None;
        self.current = None;
        self.replay = false;
        if self.missing.is_none()
            && let Some(text) = self.named()
        {
            self.missing = Some((Instant::now(), MissingLog::new(text)));
        }
        self.show();
        self.log_missing();
    }

    /// The wait's line due now, if any (asked at each read of the list).
    fn log_missing(&mut self) {
        let Some((since, log)) = &mut self.missing else {
            return;
        };
        match log.due(since.elapsed().as_secs()) {
            Some(Line::Warn(text)) => tracing::warn!("{text}"),
            Some(Line::Info(text)) => tracing::info!("{text}"),
            None => {}
        }
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
            self.buffer,
            // The output callback writes the FIFO (its RX pull, dstack
            // #307), a period at a time: the app buffer's chunk.
            self.buffer.chunk_ns(),
            Arc::clone(&self.stats),
        )
        .map_err(|e| format!("the rate conversion {} to {out_rate} Hz: {e}", self.rate))?
        .with_mono(self.mono);
        // The new stream is opened while the old one plays (a failure
        // leaves the old one playing); the old one stops before the new
        // one starts: the FIFO has one reader.
        let stream = audio::open(device, &config, player, &self.pull, &self.failed)?;
        let was = self.playing.take().and(self.current);
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
        self.current = Some(index);
        self.replay = false;
        if let Some((since, log)) = self.missing.take() {
            tracing::info!(
                "{}",
                log.found(&self.outputs[index].name, since.elapsed().as_secs())
            );
        }
        // The endpoint's id beside its name: two endpoints can carry one
        // name (Windows keeps a not-present duplicate of each), dstack #506.
        let id = self.devices[index]
            .id()
            .map_or_else(|e| format!("unknown ({e})"), |id| id.to_string());
        tracing::info!(
            output = %self.outputs[index].name,
            id,
            out_rate,
            out_channels = config.channels(),
            format = %config.sample_format(),
            "dlisten: playing on the output (its own default config)"
        );
        self.show();
        Ok(())
    }

    /// The outputs, the one playing and the text waited for, for the hooks.
    fn show(&self) {
        let shown = Shown {
            names: self.outputs.iter().map(|o| o.name.clone()).collect(),
            current: self.current,
            waiting_for: self
                .missing
                .as_ref()
                .and_then(|_| self.named().map(str::to_string)),
        };
        *lock(&self.shown) = shown;
    }

    /// dtray's choice: plays on output `index` and remembers it. A named
    /// run is named after it from now on (its exact name: the user's
    /// newest choice, still never left for another output).
    pub fn choose_output(&mut self, index: usize) -> Result<(), String> {
        // A choice ends a wait by itself: no "is there again" line for it.
        let missing = self.missing.take();
        if let Err(e) = self.play(index) {
            self.missing = missing;
            return Err(e);
        }
        let name = self.outputs[index].name.clone();
        if self.named().is_some() {
            tracing::info!(output = %name, "dlisten: dtray's choice: this run now waits for this output only");
            self.want = Want::Named(name.clone());
        }
        let s = Settings { output: Some(name) };
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
            let shown = lock(&shown);
            status::app_status(&AppView {
                snap: &s,
                underruns_before: before,
                flowing,
                outputs: &shown.names,
                current: shown.current,
                waiting_for: shown.waiting_for.as_deref(),
            })
        });
        let (shown, events) = (Arc::clone(&self.shown), Mutex::new(events));
        self.link.set_app_command(move |words| {
            let n = status::parse_command(words, &lock(&shown).names)?;
            lock(&events)
                .send(Event::Output(n))
                .map_err(|_| "dlisten is stopping".to_string())?;
            Ok(String::new())
        });
    }

    /// Reads the output devices again (one plugged in or out, a format
    /// changed); true when the list changed. What plays then is
    /// [`follow::next`]'s: the one playing keeps playing, found again by
    /// name, and is opened again when its format changed or a stream
    /// reported an error. A named run (`--output`) whose output is gone
    /// plays nothing and waits, and opens it when it is back (dstack #506);
    /// a free run falls back to the default (else the first), and with no
    /// output left keeps the old list.
    pub fn refresh_outputs(&mut self) -> bool {
        if self.failed.swap(false, Ordering::Relaxed) {
            self.replay = true;
        }
        let Ok((devices, outputs)) = audio::outputs() else {
            self.log_missing();
            return false;
        };
        if outputs == self.outputs {
            // A replay owed (a stream error, a failed opening) takes the
            // devices just read: a device handle may have been re-created.
            if self.replay && !self.waiting && self.current.is_some() {
                self.devices = devices;
                self.replay = self.replay_current();
            }
            self.log_missing();
            return false;
        }
        let playing = self.current.map(|i| self.outputs[i].clone());
        let named = self.named().is_some();
        let step = follow::next(&self.want, playing.as_ref(), &outputs);
        if step == Step::Wait && !named {
            if !self.waiting
                && let Some(p) = &playing
            {
                tracing::warn!(output = %p.name, "dlisten: no output is left: waiting for one");
            }
            self.waiting = true;
            return true;
        }
        self.waiting = false;
        self.devices = devices;
        self.outputs = outputs;
        match step {
            Step::Keep { index, reopen } => {
                // The index of the output now in use in the new list (its
                // stream is the old one until it opens again).
                self.current = Some(index);
                self.show();
                if reopen {
                    self.replay = self.replay_current();
                }
            }
            Step::Open(index) => {
                if !named && let Some(p) = &playing {
                    tracing::warn!(
                        output = %p.name,
                        default = %self.outputs[index].name,
                        "dlisten: the output playing is gone: playing on the default (else the first)"
                    );
                }
                self.current = Some(index);
                self.show();
                self.replay = self.replay_current();
            }
            Step::Wait => {
                if let Some(p) = &playing {
                    tracing::warn!(output = %p.name, "dlisten: the output playing is gone: playing nothing");
                }
                self.go_missing();
            }
        }
        true
    }

    /// Opens the current output again; true when it failed (to try again).
    fn replay_current(&mut self) -> bool {
        let Some(current) = self.current else {
            return false;
        };
        match self.play(current) {
            Ok(()) => false,
            Err(e) => {
                tracing::warn!(
                    output = %self.outputs[current].name,
                    "dlisten: the output did not open (tried again in 5 s): {e}"
                );
                true
            }
        }
    }

    /// The main thread's look at the RX pull, every 250 ms (dstack #307).
    /// While no output pulls (none open, a stream that stopped calling
    /// back), it drains the flow sockets itself, so the flow stays heard
    /// and the device keeps it ([`SharedPull::keep`]); a callback that
    /// pulls again takes over at once. Logs the change both ways, and each
    /// rise of the pull's restarts (a callback gap past the FIFO, a clock
    /// step).
    pub fn keep_pulled(&mut self) {
        if self.pull.clear_poison() {
            // A panic inside a pull (its message went to stderr): the
            // callbacks pull on through the poison, its state being read
            // positions; reported and cleared here, off the audio thread.
            tracing::warn!("dlisten: an RX pull panicked; pulling on");
        }
        // The counts read just before the look, so the log carries what it
        // saw (a callback can pull in between: the look reads them again).
        let (pulls, restarts) = (self.pull_stats.pulls(), self.pull_stats.restarts());
        let look = self.pull.keep(&mut self.watch);
        if look.changed && look.idle {
            tracing::warn!(
                pulls,
                "dlisten: no output pulls the audio: draining it from the main thread"
            );
        } else if look.changed {
            tracing::info!(pulls, "dlisten: the output pulls the audio again");
        }
        if look.restarts_rose > 0 {
            tracing::warn!(
                restarts,
                rose = look.restarts_rose,
                frames = self.pull_stats.frames(),
                "dlisten: the RX pull started again at now (a gap past the FIFO or a clock step)"
            );
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
