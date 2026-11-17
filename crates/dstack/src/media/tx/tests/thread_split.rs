// SPDX-License-Identifier: MIT OR Apache-2.0

//! The TX thread's lost time split by cause (dstack #359): the sleep's
//! oversleep, each pass's wall and CPU time, as peaks in the status and,
//! for the sleep and pass before it, in `the TX schedule starts again`.

use super::*;

use crate::media::tx::split::{Before, TxThreadStats, TxThreadView, Us};

/// The peaks keep the largest of each; the CPU peak exists only once a
/// pass's CPU time was read, and a pass without one leaves it as it was.
#[test]
fn the_tx_thread_s_peaks_keep_the_largest_of_each() {
    let s = TxThreadStats::default();
    assert_eq!(s.view(), TxThreadView::default());
    s.overslept(8_551_999);
    s.overslept(3_000_000);
    s.passed(250_999, None);
    assert_eq!(
        s.view(),
        TxThreadView {
            oversleep_max_us: 8_551,
            pass_max_us: 250,
            pass_max_cpu_us: None,
        }
    );
    s.passed(9_300_000, Some(150_000));
    s.passed(100_000, Some(20_000));
    s.passed(200_000, None);
    assert_eq!(
        s.view(),
        TxThreadView {
            oversleep_max_us: 8_551,
            pass_max_us: 9_300,
            pass_max_cpu_us: Some(150),
        }
    );
    // A CPU time of 0 is measured, not absent.
    let z = TxThreadStats::default();
    z.passed(5_000, Some(0));
    assert_eq!(z.view().pass_max_cpu_us, Some(0));
}

#[test]
fn a_restart_line_time_is_whole_us_or_a_dash() {
    assert_eq!(Us(Some(8_551_999)).to_string(), "8551");
    assert_eq!(Us(Some(999)).to_string(), "0");
    assert_eq!(Us(Some(1_000)).to_string(), "1");
    assert_eq!(Us(None).to_string(), "-");
}

/// A pass timed through the loop's read (fake wall and CPU times): its
/// times become the next restart line's and count in the peaks; the pass
/// itself runs (its packets go out).
#[test]
fn a_timed_pass_records_its_wall_and_cpu_time() {
    let mut rig = Rig::new(Vec::new());
    assert!(rig.shared.set_source(0, Source::Counter));
    rig.flow(vec![Some(1)]);
    let mut reads = [(1_000_000, Some(500_000)), (1_250_000, Some(520_000))].into_iter();
    rig.tx.timed_step(Some(T), 0, || reads.next().unwrap());
    assert_eq!(reads.next(), None, "one read before, one after");
    assert_eq!(
        rig.tx.before,
        Before {
            oversleep_ns: None,
            pass_ns: Some(250_000),
            pass_cpu_ns: Some(20_000),
        }
    );
    let mut reads = [(2_000_000, None), (11_300_000, Some(9_000))].into_iter();
    rig.tx.timed_step(Some(T + MS), 0, || reads.next().unwrap());
    assert!(!drain(&rig.rx).is_empty(), "the pass sent");
    assert_eq!(rig.tx.before.pass_ns, Some(9_300_000));
    assert_eq!(rig.tx.before.pass_cpu_ns, None, "one CPU read missing");
    assert_eq!(
        rig.shared.tx_thread(),
        TxThreadView {
            oversleep_max_us: 0,
            pass_max_us: 9_300,
            pass_max_cpu_us: Some(20),
        }
    );
    // A wall read that went back is 0, not a wrap.
    let mut reads = [(5_000, None), (4_000, None)].into_iter();
    rig.tx
        .timed_step(Some(T + 2 * MS), 0, || reads.next().unwrap());
    assert_eq!(rig.tx.before.pass_ns, Some(0));
}

/// The sleep's oversleep: the wake's read less the target, only for a
/// sleep asked before its target, both reads made, on one epoch; anything
/// else is unmeasured (and clears the last one).
#[test]
fn a_sleep_records_how_late_it_returned_on_one_epoch() {
    let mut rig = Rig::new(Vec::new());
    rig.tx.slept(10 * MS, Some(9_900_000), Some(11 * MS), true);
    assert_eq!(rig.tx.before.oversleep_ns, Some(MS));
    assert_eq!(rig.shared.tx_thread().oversleep_max_us, 1_000);
    for (asked, woke, same) in [
        (Some(10 * MS), Some(18 * MS), true),
        (None, Some(18 * MS), true),
        (Some(9_900_000), None, true),
        (Some(9_900_000), Some(18 * MS), false),
    ] {
        rig.tx.before.oversleep_ns = Some(1);
        rig.tx.slept(10 * MS, asked, woke, same);
        assert_eq!(
            rig.tx.before.oversleep_ns, None,
            "{asked:?} {woke:?} {same}"
        );
    }
    assert_eq!(rig.shared.tx_thread().oversleep_max_us, 1_000, "unchanged");
    rig.tx.slept(20 * MS, Some(19 * MS), Some(22_000_999), true);
    assert_eq!(rig.tx.before.oversleep_ns, Some(2_000_999));
    assert_eq!(rig.shared.tx_thread().oversleep_max_us, 2_000);
    // A wake before the target (a read that moved back) is on time.
    rig.tx.slept(30 * MS, Some(29 * MS), Some(29_500_000), true);
    assert_eq!(rig.tx.before.oversleep_ns, Some(0));
}

/// The restart line says what was measured before the pass that restarts:
/// the last sleep's oversleep and the last pass's wall and CPU time, `-`
/// for what was not.
#[test]
fn the_restart_line_carries_the_sleep_and_pass_before_it() {
    let mut s = Schedule::bootstrap(1_000, 32, 96);
    let before = Before {
        oversleep_ns: Some(8_551_999),
        pass_ns: Some(250_000),
        pass_cpu_ns: None,
    };
    crate::test_log::undecided();
    let warnings = crate::test_log::capture_warnings(|| {
        assert_eq!(s.due_after(1_056 + 97, &before), None);
        assert_eq!(s.due_after(1_184 + 32, &before), Some(1_184));
    });
    assert_eq!(
        warnings.matches("the TX schedule starts again").count(),
        1,
        "{warnings}"
    );
    assert!(
        warnings
            .contains("lag=97 max_lag=96 oversleep_us=8551 last_pass_us=250 last_pass_cpu_us=-"),
        "{warnings}"
    );
    let warnings = crate::test_log::capture_warnings(|| {
        assert_eq!(s.due(10_000), None);
    });
    assert!(
        warnings.contains("oversleep_us=- last_pass_us=- last_pass_cpu_us=-"),
        "{warnings}"
    );
}

/// Through the pass: a pass that reads the clock past the latency restarts
/// its flow, and the line names the sleep and pass the TX thread measured
/// before it; the next pass's line names that pass.
#[test]
fn a_pass_s_restart_line_names_what_came_before_it() {
    let mut rig = Rig::new(Vec::new());
    assert!(rig.shared.set_source(0, Source::Counter));
    let (slot, _) = rig.flow(vec![Some(1)]);
    let mut reads = [(0, Some(0)), (400_000, Some(30_000))].into_iter();
    rig.tx.timed_step(Some(T), 0, || reads.next().unwrap());
    let target = rig.tx.wake_ns(Some(T));
    rig.tx
        .slept(target, Some(T + 10_000), Some(target + 5 * MS), true);
    crate::test_log::undecided();
    let warnings = crate::test_log::capture_warnings(|| {
        rig.tx.step(Some(target + 5 * MS), 0);
    });
    assert_eq!(rig.shared.lane(slot).rebootstraps, 1);
    assert!(
        warnings.contains("oversleep_us=5000 last_pass_us=400 last_pass_cpu_us=30"),
        "{warnings}"
    );
}

/// A bounded escape hands the next passes to the relay thread: what the
/// TX thread measured before it no longer describes them.
#[test]
fn an_escape_clears_what_was_measured_before_it() {
    let mut rig = Rig::new(Vec::new());
    rig.tx.before = Before {
        oversleep_ns: Some(1),
        pass_ns: Some(2),
        pass_cpu_ns: Some(3),
    };
    assert!(!rig.tx.before_wait(|| (Some(T), 0), || false, || false));
    assert_eq!(rig.tx.before.pass_ns, Some(2), "no escape: kept");
    assert!(!rig.tx.before_wait(|| (Some(T), 1), || true, || true));
    assert_eq!(rig.tx.before.pass_ns, Some(2), "put off: kept");
    assert!(rig.tx.before_wait(|| (Some(T), 0), || true, || false));
    assert_eq!(rig.tx.before, Before::default());
}

/// A clock whose every sleep returns [`LateClock::LATE`] past its target
/// (the TX thread scheduled late), 1 ms of real time a sleep.
#[derive(Debug)]
struct LateClock {
    now: AtomicU64,
}

impl LateClock {
    /// Past the test device's 1 ms latency: every wake restarts.
    const LATE: u64 = 3 * MS;
}

impl PtpClock for LateClock {
    fn now_ns(&self) -> Option<u64> {
        Some(self.now.load(Ordering::SeqCst))
    }

    fn sleep_until(&self, ptp_ns: u64) {
        let now = self.now.load(Ordering::SeqCst);
        self.now
            .store(now.max(ptp_ns) + Self::LATE, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(1));
    }

    fn epoch(&self) -> u32 {
        0
    }
}

/// The TX loop's wiring: it times each pass (its CPU time on Linux only)
/// and reads the clock around each sleep, so a late wake shows as the
/// oversleep in the status and in the restart line it causes.
#[test]
fn the_tx_loop_measures_its_sleeps_and_passes() {
    crate::test_log::undecided();
    let shared = shared_with(Vec::new());
    assert!(shared.set_source(0, Source::Counter));
    let rx = socket();
    let (slot, _) = add(&shared, &req(v4(&rx)));
    let clock = LateClock {
        now: AtomicU64::new(T),
    };
    let stopper = {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            shared.stop();
        })
    };
    let warnings =
        crate::test_log::capture_warnings(|| run_with(&clock, Arc::clone(&shared), false));
    stopper.join().unwrap();
    let view = shared.tx_thread();
    assert_eq!(view.oversleep_max_us, LateClock::LATE / 1_000, "{view:?}");
    assert_eq!(
        view.pass_max_cpu_us.is_some(),
        cfg!(target_os = "linux"),
        "{view:?}"
    );
    assert!(
        warnings.contains(&format!("oversleep_us={}", LateClock::LATE / 1_000)),
        "{warnings}"
    );
    // Every wake is past the latency: each pass restarts, nothing goes out.
    assert!(
        shared.lane(slot).rebootstraps > 0,
        "{:?}",
        shared.lane(slot)
    );
    assert!(drain(&rx).is_empty());
}
