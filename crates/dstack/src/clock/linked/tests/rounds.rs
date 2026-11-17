//! Selector rounds timed in real time (dstack #351's follow-up on v0.6.158's
//! main CI): a test whose follower runs on the service's clock
//! (`Time::OnService`) assumes the selector's three reads of a switch round
//! (the service, the follower, the service again) fall within the offset
//! the round must see. A test thread not run between those reads (a loaded
//! box; on windows-2025 a test thread on CPU 0 behind this binary's own
//! TIME_CRITICAL spinners pinned there, which since v0.6.158 give CPU 0
//! away once per 10 ms instead of at every spin turn) reads that gap as an
//! offset, and the selector rightly counts it against agreement: it waits,
//! or a forced switch re-aligns. That is the selector's design
//! ([`offset_of`]), not a fault, so a test run with such a round proves
//! nothing either way: [`on_time`] runs it again. A run whose every timed
//! round stayed within its bound must pass every assertion as written.

use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::time::{Duration, Instant};

use super::super::{HANDOVER_NS, Linked, Source};

/// The bound of a switch round that must agree: the handover's bound (a
/// round shorter than it reads at most its own length plus the follower's
/// set offset).
pub(super) const AGREE_NS: i64 = HANDOVER_NS;
/// The bound of a round of a follower drifting 1 ms a round: a round
/// shorter than 1 ms never reads nearer than the next round's offset, so a
/// later round never passes for progress ([`super::super::wait_round`]).
pub(super) const DRIFT_NS: i64 = 1_000_000;
/// The runs [`on_time`] makes at most.
pub(super) const ATTEMPTS: u32 = 20;

/// The timed rounds of one run.
#[derive(Default)]
pub(super) struct Rounds {
    /// A timed round took longer than its bound.
    slow: Cell<bool>,
}

impl Rounds {
    /// `f`, timed: a run in which it took longer than `bound_ns` is retried
    /// when it fails.
    pub(super) fn timed<T>(&self, bound_ns: i64, f: impl FnOnce() -> T) -> T {
        let t = Instant::now();
        let out = f();
        let bound = u64::try_from(bound_ns).expect("a round's bound is not negative");
        if t.elapsed() > Duration::from_nanos(bound) {
            self.slow.set(true);
        }
        out
    }

    /// A selector round that must run within `bound_ns` of real time.
    pub(super) fn tick(&self, linked: &Linked, bound_ns: i64) -> Source {
        self.timed(bound_ns, || linked.tick())
    }
}

/// Runs `scenario` until it passes; a run that fails with every timed round
/// within its bound fails the test at once, and so does the
/// [`ATTEMPTS`]th. Returns the runs made.
pub(super) fn on_time(scenario: impl Fn(&Rounds)) -> u32 {
    let mut attempt = 0;
    loop {
        attempt += 1;
        let rounds = Rounds::default();
        match catch_unwind(AssertUnwindSafe(|| scenario(&rounds))) {
            Ok(()) => return attempt,
            Err(e) if !rounds.slow.get() || attempt == ATTEMPTS => resume_unwind(e),
            Err(_) => eprintln!("run {attempt}: a timed round passed its bound, run again"),
        }
    }
}

#[test]
fn a_run_that_fails_with_a_slow_round_runs_again() {
    let runs = Cell::new(0);
    let made = on_time(|r| {
        runs.set(runs.get() + 1);
        let pause = if runs.get() == 1 { 2 } else { 0 };
        r.timed(1_000_000, || {
            std::thread::sleep(Duration::from_millis(pause))
        });
        assert!(pause == 0, "slow");
    });
    assert_eq!((made, runs.get()), (2, 2));
}

#[test]
fn a_run_that_passes_with_a_slow_round_stands() {
    let made = on_time(|r| {
        r.timed(0, || std::thread::sleep(Duration::from_millis(1)));
    });
    assert_eq!(made, 1);
}

#[test]
fn a_run_that_fails_with_every_round_on_time_fails_at_once() {
    let runs = Cell::new(0);
    let failed = catch_unwind(AssertUnwindSafe(|| {
        on_time(|r| {
            runs.set(runs.get() + 1);
            r.timed(1_000_000_000, || ());
            panic!("on time");
        })
    }));
    assert!(failed.is_err());
    assert_eq!(runs.get(), 1);
}

#[test]
fn a_run_that_keeps_failing_slow_fails_at_the_last_attempt() {
    let runs = Cell::new(0);
    let failed = catch_unwind(AssertUnwindSafe(|| {
        on_time(|r| {
            runs.set(runs.get() + 1);
            r.timed(0, || std::thread::sleep(Duration::from_micros(10)));
            panic!("slow");
        })
    }));
    assert!(failed.is_err());
    assert_eq!(runs.get(), ATTEMPTS);
}

/// The CI shape (v0.6.158 main, windows-2025): a switch round preempted
/// between its reads waits instead of handing over. A follower whose read
/// takes 1 ms (the service's time at its end) stands in for the preempted
/// reads on the first run, which always fails slow; a later run reads at
/// once and hands over (a later run preempted itself makes one more).
#[test]
fn a_preempted_switch_round_runs_again_and_hands_over_on_time() {
    use super::{ALIVE_LOCKED, tricky_on_service};
    use crate::clock::PtpClock;
    let runs = Cell::new(0);
    let made = on_time(|r| {
        runs.set(runs.get() + 1);
        let slow_ns = if runs.get() == 1 { 1_000_000 } else { 0 };
        let (f, linked) = tricky_on_service(slow_ns, false);
        let e = linked.epoch();
        *f.view.lock().unwrap() = ALIVE_LOCKED;
        assert_eq!(r.tick(&linked, AGREE_NS), Source::Internal);
        assert_eq!(linked.epoch(), e);
    });
    assert!(made >= 2, "{made}");
}
