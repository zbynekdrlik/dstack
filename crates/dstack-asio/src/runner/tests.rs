// SPDX-License-Identifier: MIT OR Apache-2.0

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use dstack_audio_io::Block;

use super::*;
use crate::device::Started;
use crate::format::Sample;
use crate::state::{DeviceEvents, NO_CLOCK_NS, STALL_NS, rate_window_ns};

const S: i64 = 1_000_000_000;

/// What the fake driver does and what it was asked.
#[derive(Default)]
struct Fake {
    /// The next opens' answers (empty: open succeeds as `opened`).
    opens: VecDeque<Result<Opened, Reason>>,
    start_error: Option<Reason>,
    events: DeviceEvents,
    consumed: u64,
    latencies: Started,
    opened: u32,
    started: u32,
    closes: u32,
}

struct FakeDevice(Rc<RefCell<Fake>>);

fn opened() -> Opened {
    Opened {
        rate: 96_000.0,
        buffer_frames: 256,
        driver_inputs: 2,
        driver_outputs: 2,
        inputs: vec![0],
        outputs: vec![1],
        input_sample: Some(Sample::Int32),
        output_sample: Sample::Int32,
    }
}

impl Device for FakeDevice {
    fn open(&mut self, driver: &str, channels: &Channels) -> Result<Opened, Reason> {
        assert_eq!(driver, "Card");
        assert_eq!(channels.outputs, vec![1]);
        let mut f = self.0.borrow_mut();
        f.opened += 1;
        f.opens.pop_front().unwrap_or(Ok(opened()))
    }

    fn start(&mut self, engine: Engine) -> Result<Started, Reason> {
        assert_eq!(engine.frames(), 256);
        let mut f = self.0.borrow_mut();
        if let Some(e) = f.start_error.take() {
            return Err(e);
        }
        f.started += 1;
        // A run starts its counts from 0.
        f.events = DeviceEvents::default();
        f.consumed = 0;
        Ok(f.latencies)
    }

    fn poll(&mut self) -> DeviceEvents {
        let mut f = self.0.borrow_mut();
        let ev = f.events;
        // One-shot flags, as the driver's.
        f.events.reset = false;
        f.events.latencies_changed = false;
        f.events.resync = false;
        f.events.rate_changed = None;
        ev
    }

    fn consumed_frames(&self) -> u64 {
        self.0.borrow().consumed
    }

    fn latencies(&self) -> Started {
        self.0.borrow().latencies
    }

    fn close(&mut self) {
        self.0.borrow_mut().closes += 1;
    }
}

struct Silence;

impl Process for Silence {
    fn process(&mut self, block: &mut Block<'_>) {
        block.zero_outputs();
    }
}

struct Rig {
    fake: Rc<RefCell<Fake>>,
    runner: Runner<FakeDevice>,
    status: Arc<Mutex<Status>>,
    made: Arc<std::sync::atomic::AtomicU32>,
}

fn rig_with(make_fails: bool) -> Rig {
    let fake = Rc::new(RefCell::new(Fake::default()));
    let made = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let m = Arc::clone(&made);
    let make: MakeProcess = Box::new(move |o: &Opened| {
        assert_eq!(o.buffer_frames, 256);
        m.fetch_add(1, Ordering::SeqCst);
        if make_fails {
            Err("no such rate".into())
        } else {
            Ok(Box::new(Silence) as Box<dyn Process>)
        }
    });
    let channels = Channels {
        inputs: crate::device::Inputs::Exact(vec![0]),
        outputs: vec![1],
    };
    let runner = Runner::new(FakeDevice(Rc::clone(&fake)), "Card", channels, make);
    let status = runner.status();
    Rig {
        fake,
        runner,
        status,
        made,
    }
}

fn rig() -> Rig {
    rig_with(false)
}

impl Rig {
    fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    fn opens(&self) -> u32 {
        self.fake.borrow().opened
    }

    /// The driver ticks: `callbacks` so far, `consumed` frames taken.
    fn tick(&self, callbacks: u64, consumed: u64) {
        let mut f = self.fake.borrow_mut();
        f.events.callbacks = callbacks;
        f.consumed = consumed;
    }
}

#[test]
fn a_failing_open_waits_2_10_30_then_60_s() {
    let mut r = rig();
    for _ in 0..6 {
        r.fake
            .borrow_mut()
            .opens
            .push_back(Err(Reason::Busy("in use".into())));
    }
    let mut at = 0;
    for (n, wait) in [2, 10, 30, 60, 60].into_iter().enumerate() {
        assert_eq!(r.runner.step(at), POLL_NS);
        assert_eq!(r.opens(), n as u32 + 1);
        let s = r.status();
        assert_eq!(s.state, RunState::Waiting);
        assert_eq!(s.reason, Some(Reason::Busy("in use".into())));
        assert_eq!(s.retry_in_s, Some(wait as f64));
        // Nothing before the retry is due; the status counts down.
        assert_eq!(r.runner.step(at + wait * S - 1), 1);
        assert_eq!(r.opens(), n as u32 + 1);
        assert_eq!(r.status().retry_in_s, Some(1e-9));
        at += wait * S;
    }
    assert_eq!(
        r.fake.borrow().closes,
        5,
        "a failed open releases the driver"
    );
    assert_eq!(r.made.load(Ordering::SeqCst), 0);
}

#[test]
fn an_open_starts_the_apps_process_and_reads_running() {
    let mut r = rig();
    r.fake.borrow_mut().latencies = Started {
        input_latency_frames: 300,
        output_latency_frames: 400,
    };
    r.runner.step(0);
    assert_eq!((r.opens(), r.fake.borrow().started), (1, 1));
    assert_eq!(r.made.load(Ordering::SeqCst), 1);
    let s = r.status();
    assert_eq!(s.state, RunState::Running);
    assert_eq!(s.reason, None);
    assert_eq!((s.rate, s.buffer_frames), (96_000, 256));
    assert_eq!(
        (s.input_sample, s.output_sample),
        (Some("Int32LSB"), Some("Int32LSB"))
    );
    assert_eq!(
        (s.input_latency_frames, s.output_latency_frames),
        (300, 400)
    );
    r.tick(10, 2560);
    r.fake.borrow_mut().events.late_callbacks = 2;
    r.fake.borrow_mut().events.fast_callbacks = 3;
    r.fake.borrow_mut().events.overloads = 4;
    r.fake.borrow_mut().events.callback_gap_max_ns = 5_500_000;
    r.fake.borrow_mut().events.resync = true;
    assert_eq!(r.runner.step(POLL_NS), POLL_NS);
    let s = r.status();
    assert_eq!(
        (
            s.callbacks,
            s.late_callbacks,
            s.fast_callbacks,
            s.overloads,
            s.resyncs
        ),
        (10, 2, 3, 4, 1)
    );
    assert_eq!(s.gap_max_ms, 5.5);
    assert_eq!(s.state, RunState::Running);
    assert_eq!(r.fake.borrow().closes, 0);
}

#[test]
fn an_app_that_refuses_or_a_failed_start_waits_the_backoff() {
    let mut r = rig_with(true);
    r.runner.step(0);
    let s = r.status();
    assert_eq!(
        s.reason,
        Some(Reason::Failed(
            "the app refused the driver: no such rate".into()
        ))
    );
    assert_eq!((s.retry_in_s, r.fake.borrow().started), (Some(2.0), 0));
    assert_eq!(r.fake.borrow().closes, 1);
    let mut r = rig();
    r.fake.borrow_mut().start_error = Some(Reason::Failed("start: x".into()));
    r.runner.step(0);
    assert_eq!(r.status().reason, Some(Reason::Failed("start: x".into())));
    assert_eq!(r.status().retry_in_s, Some(2.0));
    r.runner.step(2 * S);
    assert_eq!(r.status().state, RunState::Running);
}

#[test]
fn a_reset_closes_the_run_and_a_60_s_run_resets_the_backoff() {
    let mut r = rig();
    r.runner.step(0);
    r.tick(1, 256);
    r.fake.borrow_mut().events.reset = true;
    r.runner.step(S);
    let s = r.status();
    assert_eq!(
        (s.state, s.reason.clone(), s.resets),
        (RunState::Waiting, Some(Reason::Reset), 1)
    );
    assert_eq!(s.retry_in_s, Some(2.0));
    assert_eq!(r.fake.borrow().closes, 1);
    // The second reset in a row after a short run waits 10 s.
    r.runner.step(3 * S);
    r.tick(1, 256);
    r.fake.borrow_mut().events.rate_changed = Some(48_000.0);
    r.runner.step(4 * S);
    assert_eq!(r.status().reason, Some(Reason::RateChanged(48_000)));
    assert_eq!(r.status().retry_in_s, Some(10.0));
    // A run of 60 s: back to 2 s.
    r.runner.step(14 * S);
    r.tick(1, 256);
    r.fake.borrow_mut().events.buffer_size_change = true;
    r.runner.step(74 * S);
    assert_eq!(r.status().reason, Some(Reason::Reset));
    assert_eq!(r.status().retry_in_s, Some(2.0));
    assert_eq!(r.status().resets, 3);
}

#[test]
fn a_driver_with_no_clock_waits_calmly_and_reopens_every_60_s() {
    let mut r = rig();
    r.runner.step(0);
    r.runner.step(NO_CLOCK_NS - 1);
    assert_eq!(r.status().state, RunState::Running);
    r.runner.step(NO_CLOCK_NS);
    let s = r.status();
    assert_eq!(
        (s.state, s.reason.clone()),
        (RunState::Waiting, Some(Reason::NoClock))
    );
    r.runner.step(60 * S - 1);
    assert_eq!(r.fake.borrow().closes, 0);
    r.runner.step(60 * S);
    assert_eq!(r.fake.borrow().closes, 1, "closed to open again");
    assert_eq!(r.status().clock_waits, 1);
    assert_eq!(r.status().resets, 0, "no reset");
    r.runner.step(60 * S);
    assert_eq!(r.opens(), 2, "opened again at once, no backoff");
    let s = r.status();
    assert_eq!(
        (s.state, s.reason.clone()),
        (RunState::Waiting, Some(Reason::NoClock))
    );
    // A burst of callbacks at the reopen is no clock: the run's first step
    // takes it as its tick base.
    r.tick(2, 512);
    r.runner.step(60 * S + POLL_NS);
    assert_eq!(r.status().reason, Some(Reason::NoClock));
    r.runner.step(63 * S);
    let s = r.status();
    assert_eq!(
        (s.state, s.reason.clone()),
        (RunState::Waiting, Some(Reason::NoClock))
    );
    assert_eq!(s.resets, 0, "never read as a stall");
    // It ticks from there: the clock arrived.
    r.tick(3, 768);
    r.runner.step(64 * S);
    let s = r.status();
    assert_eq!((s.state, s.reason), (RunState::Running, None));
}

#[test]
fn a_driver_that_stops_ticking_for_2_s_is_a_stall() {
    let mut r = rig();
    r.runner.step(0);
    r.tick(5, 1280);
    r.runner.step(POLL_NS);
    r.tick(6, 1536);
    r.runner.step(S);
    r.runner.step(S + STALL_NS - 1);
    assert_eq!(r.status().state, RunState::Running);
    r.runner.step(S + STALL_NS);
    assert_eq!(r.status().reason, Some(Reason::Stalled));
    assert_eq!(r.status().resets, 1);
}

#[test]
fn a_panicking_app_closes_the_run() {
    let mut r = rig();
    r.runner.step(0);
    r.tick(5, 1280);
    r.fake.borrow_mut().events.faults = 1;
    r.runner.step(S);
    assert_eq!(
        r.status().reason,
        Some(Reason::Failed("the app's audio callback panicked".into()))
    );
    assert_eq!(r.status().faults, 1);
    r.runner.step(3 * S);
    assert_eq!(
        r.made.load(Ordering::SeqCst),
        2,
        "a fresh Process for the reopen"
    );
}

#[test]
fn a_parked_driver_is_never_opened_again() {
    let mut r = rig();
    r.fake.borrow_mut().opens.push_back(Err(Reason::Parked));
    r.runner.step(0);
    let s = r.status();
    assert_eq!((s.reason, s.retry_in_s), (Some(Reason::Parked), None));
    r.runner.step(i64::MAX - 1);
    assert_eq!(r.opens(), 1);
    assert_eq!(r.status().retry_in_s, None);
}

#[test]
fn changed_latencies_are_read_again() {
    let mut r = rig();
    r.runner.step(0);
    r.tick(1, 256);
    {
        let mut f = r.fake.borrow_mut();
        f.events.latencies_changed = true;
        f.latencies = Started {
            input_latency_frames: 7,
            output_latency_frames: 9,
        };
    }
    r.runner.step(S);
    let s = r.status();
    assert_eq!((s.input_latency_frames, s.output_latency_frames), (7, 9));
}

/// The driver taking `per_s` frames a second (its reported rate is 96 kHz),
/// stepped every 50 ms from `from` until `until`, its count going on from
/// `consumed`; the first step after which `stop` holds.
fn drive(
    r: &mut Rig,
    per_s: f64,
    (from, until): (i64, i64),
    consumed: &mut f64,
    stop: impl Fn(&Status) -> bool,
) -> Option<i64> {
    let step = 50_000_000;
    let mut at = from;
    while at < until {
        at += step;
        *consumed += per_s * step as f64 / 1e9;
        let c = *consumed as u64;
        r.tick(c / 256 + 1, c);
        r.runner.step(at);
        if stop(&r.status()) {
            return Some(at);
        }
    }
    None
}

/// SoundGrid with its network off: ~2.9 % more than its rate.
const OFF: f64 = 96_000.0 * 1.029;

#[test]
fn a_card_far_off_its_rate_is_proven_silently_and_runs_once_on_its_rate() {
    let window = rate_window_ns(256, 96_000.0);
    let mut r = rig();
    r.runner.step(0);
    let mut c = 0.0;
    let off = |s: &Status| s.reason == Some(Reason::ClockOffRate);
    let at = drive(&mut r, OFF, (0, 60 * S), &mut c, off).expect("off rate");
    // The tick base is the first step (0.05 s), the windows start a quarter
    // second of audio later (0.3 s) and two must close off.
    assert_eq!(at, 300_000_000 + 2 * window);
    let s = r.status();
    assert_eq!(
        (s.state, s.clock_waits, s.resets),
        (RunState::Waiting, 1, 0)
    );
    assert_eq!(r.fake.borrow().closes, 1);
    // Opened again at once, proving: the app's Process is not made.
    r.runner.step(at);
    assert_eq!((r.opens(), r.made.load(Ordering::SeqCst)), (2, 1));
    assert_eq!(r.status().reason, Some(Reason::ClockOffRate));
    // Still off: one window is the verdict, but the run stays open (silent)
    // until 60 s after its open, then opens again at once.
    let mut c = 0.0;
    let reopened = |_: &Status| false;
    // The last step 50 ms before the 60 s.
    let last = at + 60 * S - 50_000_000;
    assert_eq!(drive(&mut r, OFF, (at, last - 1), &mut c, reopened), None);
    assert_eq!(
        (r.fake.borrow().closes, r.status().state),
        (1, RunState::Waiting)
    );
    r.runner.step(at + 60 * S - 1);
    assert_eq!(r.fake.borrow().closes, 1);
    r.runner.step(at + 60 * S);
    assert_eq!(r.fake.borrow().closes, 2);
    assert_eq!(r.status().clock_waits, 2);
    r.runner.step(at + 60 * S);
    assert_eq!((r.opens(), r.made.load(Ordering::SeqCst)), (3, 1));
    // On its rate now: one plausible window ends the wait, and the driver is
    // opened again with the app's Process.
    let mut c = 0.0;
    let back = at + 60 * S;
    let runs = |s: &Status| s.state == RunState::Running;
    let ran = drive(&mut r, 96_000.0, (back, back + 30 * S), &mut c, runs).expect("runs");
    assert_eq!(r.made.load(Ordering::SeqCst), 2);
    assert_eq!(r.status().reason, None);
    assert!(ran >= back + window, "{ran}");
    assert_eq!(r.status().resets, 0);
}

#[test]
fn an_open_at_another_rate_ends_the_proving() {
    let mut r = rig();
    r.runner.step(0);
    let mut c = 0.0;
    let off = |s: &Status| s.reason == Some(Reason::ClockOffRate);
    let at = drive(&mut r, OFF, (0, 60 * S), &mut c, off).expect("off rate");
    let mut other = opened();
    other.rate = 48_000.0;
    r.fake.borrow_mut().opens.push_back(Ok(other));
    r.runner.step(at);
    assert_eq!(
        r.made.load(Ordering::SeqCst),
        2,
        "a fresh open runs the app"
    );
    assert_eq!(r.status().state, RunState::Running);
}

#[test]
fn a_card_on_its_rate_runs_on() {
    let mut r = rig();
    r.runner.step(0);
    let mut c = 0.0;
    let stopped = |s: &Status| s.state != RunState::Running;
    assert_eq!(
        drive(&mut r, 96_000.0 * 1.000_3, (0, 40 * S), &mut c, stopped),
        None
    );
    assert_eq!(r.fake.borrow().closes, 0);
}

#[test]
fn the_thread_loop_steps_until_stopped_and_releases_the_driver() {
    let r = rig();
    let fake = Rc::clone(&r.fake);
    let stop = Arc::new(AtomicBool::new(false));
    let s = Arc::clone(&stop);
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        s.store(true, Ordering::Relaxed);
    });
    r.runner.run(&stop);
    stopper.join().unwrap();
    let f = fake.borrow();
    assert_eq!((f.opened, f.started), (1, 1), "it opened and ran");
    assert_eq!(f.closes, 1, "and released the driver once stopped");
}
