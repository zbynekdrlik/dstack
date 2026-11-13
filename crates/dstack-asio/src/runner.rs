// SPDX-License-Identifier: MIT OR Apache-2.0

//! A host's worker for an app with no queue of its own (dstack #243: dasio's
//! bridge instance): it opens the driver, starts it with the app's
//! `Process`, polls it every 10 ms and closes and reopens it on the
//! decisions of [`crate::state`] — SongPlayer's worker lifecycle (songplayer
//! #233, #229) without its program queue and servo.
//!
//! - **Open:** a failed open waits the backoff (2 / 10 / 30 / 60 s); the app
//!   makes a fresh `Process` for every open (`make`, from what the open
//!   read: the driver's rate and buffer), so a reopen at another rate gets
//!   one built for it.
//! - **Close:** a reset, a rate change, a stall or a panicking `Process`
//!   closes the run and waits the backoff (a run of 60 s resets it); a
//!   parked driver is never opened again.
//! - **No clock:** a driver that does not tick waits calmly from 2 s after
//!   the open and is opened again every 60 s (no backoff, no reset).
//! - **Off rate** (songplayer #229): two 10 s windows in a row more than 1 %
//!   off the reported rate stop the run (one WARN per episode) and open the
//!   driver again at once to PROVE its rate silently: the app's `Process`
//!   does not run (the driver plays silence), the status reads waiting
//!   [`Reason::ClockOffRate`], one window within 1 % ends the wait (the
//!   driver is opened again with the app's `Process`), and a card still off
//!   60 s after the open is opened again at once to go on proving. An open
//!   at another rate or buffer than the one found off is a fresh open.
//! - **Ticks** count from the run's first step: a burst of callbacks at the
//!   open is no clock and no rate.
//!
//! [`Runner::step`] is pure over its `now` (ns on a monotonic clock) and the
//! [`Device`]; [`Runner::run`] is the thread loop around it. The app reads
//! [`Status`] from any thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use dstack_audio_io::Process;
use tracing::{debug, info, warn};

use crate::device::{Channels, Device, Opened};
use crate::engine::Engine;
use crate::state::{
    ClockStep, NO_CLOCK_REOPEN_NS, OFF_RATE_STRIKES_PLAYING, POLL_NS, RateStep, RateWatch, Reason,
    StallWatch, backoff_ns, clock_step, close_reason, failures_after_close, rate_settled,
};

/// What the app builds for each open: its `Process` for a driver opened as
/// `Opened` (its rate and buffer), or why it cannot.
pub type MakeProcess = Box<dyn FnMut(&Opened) -> Result<Box<dyn Process>, String> + Send>;

/// Where a runner is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunState {
    /// Not opened yet.
    #[default]
    Opening,
    /// The driver runs the app's `Process`.
    Running,
    /// Closed or without a usable clock: see [`Status::reason`].
    Waiting,
}

/// A runner's state for the app (a status line, an API).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub state: RunState,
    pub reason: Option<Reason>,
    /// When the next open comes, s (`None`: no retry is due, or none will
    /// come: a parked driver).
    pub retry_in_s: Option<f64>,
    /// The last open's facts.
    pub rate: u32,
    pub buffer_frames: u32,
    pub input_sample: Option<&'static str>,
    pub output_sample: Option<&'static str>,
    pub input_latency_frames: u32,
    pub output_latency_frames: u32,
    /// The current run's counts.
    pub callbacks: u64,
    pub late_callbacks: u64,
    pub fast_callbacks: u64,
    pub overloads: u64,
    pub faults: u64,
    /// The longest gap between two buffer switches over the last poll, ms.
    pub gap_max_ms: f64,
    /// Runs closed (a reset, a rate change, a stall, a fault).
    pub resets: u64,
    /// Reopens for a driver with no usable clock.
    pub clock_waits: u64,
    /// The driver's resync requests, so far.
    pub resyncs: u64,
}

/// One open run.
struct Run {
    opened: Opened,
    opened_at: i64,
    stall: StallWatch,
    rate: RateWatch,
    /// The frames taken at the run's first step: ticks count from there.
    tick_base: Option<u64>,
    /// A silent run proving the card's rate (module doc).
    proving: bool,
}

/// A close that is no fault: no backoff, no reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Calm {
    /// No clock 60 s after the open: open again at once, still waiting.
    NoClock,
    /// The card is off its rate: open again at once, silently proving.
    OffRate,
    /// A proving run found the rate within 1 %: open again at once with the
    /// app's `Process`.
    Proven,
}

/// What a proving run plays: nothing.
struct Silence;

impl Process for Silence {
    fn process(&mut self, block: &mut dstack_audio_io::Block<'_>) {
        block.zero_outputs();
    }
}

enum State {
    Closed { retry_at: i64 },
    Running(Run),
}

/// No retry: a parked driver.
const NEVER: i64 = i64::MAX;

/// The worker of one driver (module doc).
pub struct Runner<D: Device> {
    device: D,
    driver: String,
    channels: Channels,
    make: MakeProcess,
    status: Arc<Mutex<Status>>,
    state: State,
    failures: u32,
    waiting_for_clock: bool,
    /// The rate and buffer the card was found off its rate at.
    off_rate: Option<(u32, u32)>,
}

fn lock(m: &Mutex<Status>) -> MutexGuard<'_, Status> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn update(status: &Mutex<Status>, f: impl FnOnce(&mut Status)) {
    f(&mut lock(status));
}

impl<D: Device> Runner<D> {
    /// A runner of `driver` (its registered name) on `device`, using
    /// `channels`, building each open's `Process` with `make`. The first
    /// step opens it.
    pub fn new(device: D, driver: &str, channels: Channels, make: MakeProcess) -> Self {
        Runner {
            device,
            driver: driver.to_string(),
            channels,
            make,
            status: Arc::new(Mutex::new(Status::default())),
            state: State::Closed { retry_at: i64::MIN },
            failures: 0,
            waiting_for_clock: false,
            off_rate: None,
        }
    }

    /// The status, shared with the app.
    pub fn status(&self) -> Arc<Mutex<Status>> {
        Arc::clone(&self.status)
    }

    /// The device (a test reads its fake).
    pub fn device(&self) -> &D {
        &self.device
    }

    /// One step at `now` (ns, monotonic); returns how long the loop may wait
    /// before the next, ns.
    pub fn step(&mut self, now: i64) -> i64 {
        let run = match &mut self.state {
            State::Closed { retry_at } if now < *retry_at => {
                let left = *retry_at - now;
                let retry = (*retry_at != NEVER).then_some(left as f64 / 1e9);
                update(&self.status, |s| s.retry_in_s = retry);
                return left.min(POLL_NS);
            }
            State::Closed { .. } => {
                self.open(now);
                return POLL_NS;
            }
            State::Running(run) => run,
        };
        let ev = self.device.poll();
        let consumed = self.device.consumed_frames();
        let primed = run.tick_base.is_some();
        let base = *run.tick_base.get_or_insert(consumed);
        let ticks = consumed.saturating_sub(base);
        let since_open = now - run.opened_at;
        let (rate, buffer) = (run.opened.rate, run.opened.buffer_frames);
        let proving = run.proving;
        let clock = clock_step(ticks, primed, self.waiting_for_clock, since_open);
        let mut calm = None;
        match clock {
            ClockStep::Ticking | ClockStep::Quiet => {}
            ClockStep::ClockArrived => {
                self.waiting_for_clock = false;
                info!(driver = %self.driver, proving, "asio: the driver gives a clock now");
                update(&self.status, |s| {
                    if !proving {
                        s.state = RunState::Running;
                        s.reason = None;
                    }
                });
            }
            ClockStep::StartsWaiting => {
                self.waiting_for_clock = true;
                warn!(
                    driver = %self.driver,
                    "asio: no callback since the driver opened - waiting for its clock (the driver stays open, opened again every 60 s)"
                );
                update(&self.status, |s| {
                    s.state = RunState::Waiting;
                    s.reason = Some(Reason::NoClock);
                    s.retry_in_s = None;
                });
            }
            ClockStep::Reopen => calm = Some(Calm::NoClock),
        }
        let ticking = matches!(clock, ClockStep::Ticking | ClockStep::ClockArrived);
        if calm.is_none() && proving && since_open >= NO_CLOCK_REOPEN_NS {
            calm = Some(Calm::OffRate);
        }
        if calm.is_none() && ticking && rate_settled(ticks, rate) {
            let driver = (rate, buffer);
            let strikes = if proving { 1 } else { OFF_RATE_STRIKES_PLAYING };
            run.rate.track(consumed, now, driver);
            match run.rate.step(consumed, now, driver, strikes) {
                RateStep::Measuring => {}
                RateStep::Plausible => {
                    if proving {
                        info!(driver = %self.driver, "asio: the card takes audio at its driver's rate - running");
                        self.off_rate = None;
                        calm = Some(Calm::Proven);
                    }
                }
                RateStep::OffRate { ppm } => {
                    if !proving {
                        warn!(
                            driver = %self.driver,
                            ppm,
                            "asio: the card takes audio more than 1 % off its driver's rate - no usable clock; the driver plays silence and is opened again every 60 s until it is on its rate (e.g. Waves SoundGrid with its network off)"
                        );
                        self.off_rate = Some((rate.round() as u32, buffer));
                        calm = Some(Calm::OffRate);
                    }
                }
            }
        }
        if ev.latencies_changed {
            let l = self.device.latencies();
            info!(
                driver = %self.driver,
                input_frames = l.input_latency_frames,
                output_frames = l.output_latency_frames,
                "asio: the driver's latencies changed (read again)"
            );
            update(&self.status, |s| {
                s.input_latency_frames = l.input_latency_frames;
                s.output_latency_frames = l.output_latency_frames;
            });
        }
        let fault = close_reason(&ev, rate).or_else(|| {
            if ev.faults > 0 {
                Some(Reason::Failed("the app's audio callback panicked".into()))
            } else {
                (ticking && run.stall.stalled(ev.callbacks, now)).then_some(Reason::Stalled)
            }
        });
        update(&self.status, |s| {
            s.callbacks = ev.callbacks;
            s.late_callbacks = ev.late_callbacks;
            s.fast_callbacks = ev.fast_callbacks;
            s.overloads = ev.overloads;
            s.faults = ev.faults;
            s.gap_max_ms = ev.callback_gap_max_ns as f64 / 1e6;
            s.resyncs += u64::from(ev.resync);
        });
        match (fault, calm) {
            (Some(reason), _) => self.close(now, reason, since_open),
            (None, Some(c)) => self.calm_close(now, c),
            (None, None) => {}
        }
        POLL_NS
    }

    fn open(&mut self, now: i64) {
        let opened = match self.device.open(&self.driver, &self.channels) {
            Ok(opened) => opened,
            Err(reason) => {
                self.device.close();
                self.failures = self.failures.saturating_add(1);
                self.wait(now, reason, "asio: opening the driver failed");
                return;
            }
        };
        let this = (opened.rate.round() as u32, opened.buffer_frames);
        if self.off_rate.is_some_and(|off| off != this) {
            // Another rate or buffer (SoundGrid's network back): fresh.
            self.off_rate = None;
        }
        let proving = self.off_rate.is_some();
        let process = if proving {
            Ok(Box::new(Silence) as Box<dyn Process>)
        } else {
            (self.make)(&opened)
                .map_err(|e| Reason::Failed(format!("the app refused the driver: {e}")))
        };
        let started = process.and_then(|p| self.device.start(Engine::new(p, &opened)));
        match started {
            Ok(l) => {
                let waiting = self.waiting_for_clock || proving;
                if waiting {
                    debug!(driver = %self.driver, proving, "asio: opened the driver again - still waiting for a usable clock");
                } else {
                    info!(
                        driver = %self.driver,
                        rate = opened.rate,
                        buffer_frames = opened.buffer_frames,
                        inputs = opened.inputs.len(),
                        outputs = opened.outputs.len(),
                        input_latency_frames = l.input_latency_frames,
                        output_latency_frames = l.output_latency_frames,
                        "asio: the driver runs (its rate, buffer and sample types are the driver's own)"
                    );
                }
                let reason = if proving {
                    Some(Reason::ClockOffRate)
                } else {
                    self.waiting_for_clock.then_some(Reason::NoClock)
                };
                update(&self.status, |s| {
                    s.state = if waiting {
                        RunState::Waiting
                    } else {
                        RunState::Running
                    };
                    s.reason = reason;
                    s.retry_in_s = None;
                    s.rate = this.0;
                    s.buffer_frames = opened.buffer_frames;
                    s.input_sample = opened.input_sample.map(|x| x.name());
                    s.output_sample = Some(opened.output_sample.name());
                    s.input_latency_frames = l.input_latency_frames;
                    s.output_latency_frames = l.output_latency_frames;
                });
                self.state = State::Running(Run {
                    opened,
                    opened_at: now,
                    stall: StallWatch::default(),
                    rate: RateWatch::default(),
                    tick_base: None,
                    proving,
                });
            }
            Err(reason) => {
                self.device.close();
                self.failures = self.failures.saturating_add(1);
                self.wait(now, reason, "asio: starting the driver failed");
            }
        }
    }

    /// Close the run to open it again at once, no fault (no backoff, no
    /// reset): a driver without a usable clock, or one just proven.
    fn calm_close(&mut self, now: i64, calm: Calm) {
        self.waiting_for_clock = calm == Calm::NoClock;
        let reason = match calm {
            Calm::NoClock => Some(Reason::NoClock),
            Calm::OffRate => Some(Reason::ClockOffRate),
            Calm::Proven => None,
        };
        if let Some(reason) = reason {
            update(&self.status, |s| {
                s.clock_waits += 1;
                s.state = RunState::Waiting;
                s.reason = Some(reason);
            });
        }
        debug!(driver = %self.driver, ?calm, "asio: closing the driver to open it again");
        self.device.close();
        self.state = State::Closed { retry_at: now };
    }

    /// Close the run for `reason` (a fault): a reset with the backoff.
    fn close(&mut self, now: i64, reason: Reason, ran: i64) {
        self.failures = failures_after_close(self.failures, ran);
        update(&self.status, |s| s.resets += 1);
        // Waiting is shown before the device closes: a vanished driver can
        // block its stop or release for seconds.
        self.wait(now, reason, "asio: closing the driver");
        self.device.close();
    }

    fn wait(&mut self, now: i64, reason: Reason, what: &str) {
        self.waiting_for_clock = false;
        // A parked driver never opens again in this process: no retry.
        let wait = (reason != Reason::Parked).then(|| backoff_ns(self.failures));
        let retry_in_s = wait.map(|w| w as f64 / 1e9);
        warn!(
            driver = %self.driver,
            reason = %reason,
            failures = self.failures,
            retry_in_s = ?retry_in_s,
            "{what}"
        );
        self.state = State::Closed {
            retry_at: wait.map_or(NEVER, |w| now + w),
        };
        update(&self.status, |s| {
            s.state = RunState::Waiting;
            s.reason = Some(reason);
            s.retry_in_s = retry_in_s;
        });
    }

    /// The thread loop: steps until `stop` is set, then releases the driver.
    /// Run it on the thread that made the device (a COM STA on Windows).
    pub fn run(mut self, stop: &AtomicBool) {
        let start = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            let now = i64::try_from(start.elapsed().as_nanos()).unwrap_or(i64::MAX);
            let wait = self.step(now);
            std::thread::sleep(Duration::from_nanos(wait.max(0) as u64));
        }
        self.device.close();
        info!(driver = %self.driver, "asio: the driver is released (stopped)");
    }
}

#[cfg(test)]
mod tests;
