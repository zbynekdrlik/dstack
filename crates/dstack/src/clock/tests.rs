use super::*;

/// The fleet's D (wall = PTP + D), a grandmaster up 100 s, a box up 50 s.
const D: i64 = 1_790_000_000_000_000_000;
const PTP: i64 = 100_000_000_000;
const MONO: i64 = 50_000_000_000;
const K: i64 = PTP - MONO;

fn reading(date: Option<DateState>) -> Reading {
    Reading {
        wall_ns: D + PTP,
        locked: true,
        settled: true,
        mode: 3,
        freq_ppb: -750,
        grandmaster: [0; 6],
        master: [0; 6],
        date,
    }
}

/// D in effect, the last step 10 minutes ago.
fn steady(d: i64) -> Option<DateState> {
    Some(DateState {
        d_ns: d,
        step_at_ptp_ns: Some(PTP - 600_000_000_000),
        slewing: false,
    })
}

#[test]
fn k_is_wall_minus_d_minus_mono() {
    let r = reading(steady(D));
    assert_eq!(sample(&r, D + PTP, MONO), Sample::Usable { k_ns: K });
    // The box's wall is read, never dsync's: 3 ms later both clocks
    // moved on and k is the same.
    assert_eq!(
        sample(&r, D + PTP + 3_000_000, MONO + 3_000_000),
        Sample::Usable { k_ns: K }
    );
    // A grandmaster that booted after the box: k is negative.
    assert_eq!(
        sample(&r, D + 1_000, MONO),
        Sample::Usable { k_ns: 1_000 - MONO }
    );
}

#[test]
fn a_date_step_moves_wall_and_d_together_and_k_stays() {
    for step in [200_000_000, -200_000_000] {
        let r = reading(steady(D + step));
        assert_eq!(
            sample(&r, D + step + PTP, MONO),
            Sample::Usable { k_ns: K },
            "{step}"
        );
    }
    // A wall that stepped while the D read stayed (a reply built across the
    // step) is a different k: one odd poll, which the tracker holds.
    let r = reading(steady(D));
    assert_eq!(
        sample(&r, D + PTP + 200_000_000, MONO),
        Sample::Usable {
            k_ns: K + 200_000_000
        }
    );
}

#[test]
fn no_anchor_within_two_seconds_of_a_scheduled_step() {
    let at = |step_at: i64| {
        let date = DateState {
            d_ns: D,
            step_at_ptp_ns: Some(step_at),
            slewing: false,
        };
        sample(&reading(Some(date)), D + PTP, MONO)
    };
    // Due within 2 s, or done within 2 s: no anchor.
    assert_eq!(at(PTP + STEP_GUARD_NS), Sample::StepNear);
    assert_eq!(at(PTP + 1), Sample::StepNear);
    assert_eq!(at(PTP), Sample::StepNear);
    assert_eq!(at(PTP - STEP_GUARD_NS), Sample::StepNear);
    // Further away: usable.
    assert_eq!(at(PTP + STEP_GUARD_NS + 1), Sample::Usable { k_ns: K });
    assert_eq!(at(PTP - STEP_GUARD_NS - 1), Sample::Usable { k_ns: K });
    // No step named (a slew's announce): usable when no slew runs.
    let date = DateState {
        d_ns: D,
        step_at_ptp_ns: None,
        slewing: false,
    };
    assert_eq!(
        sample(&reading(Some(date)), D + PTP, MONO),
        Sample::Usable { k_ns: K }
    );
}

#[test]
fn none_while_a_slew_runs() {
    let date = DateState {
        d_ns: D,
        step_at_ptp_ns: None,
        slewing: true,
    };
    assert_eq!(sample(&reading(Some(date)), D + PTP, MONO), Sample::Slewing);
    // The tracker takes none of it.
    let mut t = Tracker::new();
    assert_eq!(t.feed(Sample::Slewing), Event::NotReady);
    assert_eq!(t.feed(Sample::StepNear), Event::NotReady);
    assert_eq!(t.k(), None);
}

#[test]
fn not_locked_is_not_ready() {
    let mut r = reading(steady(D));
    r.locked = false;
    assert_eq!(sample(&r, D + PTP, MONO), Sample::NotLocked);
    // Unlocked wins over every other reason.
    r.date = Some(DateState {
        d_ns: D,
        step_at_ptp_ns: Some(PTP),
        slewing: true,
    });
    assert_eq!(sample(&r, D + PTP, MONO), Sample::NotLocked);
    r.date = None;
    assert_eq!(sample(&r, D + PTP, MONO), Sample::NotLocked);
    // Locked without a date offset: no D, no k.
    assert_eq!(sample(&reading(None), D + PTP, MONO), Sample::NoDateState);
    let mut t = Tracker::new();
    for s in [
        Sample::NotLocked,
        Sample::NoDateState,
        Sample::NoReply,
        Sample::StepNear,
    ] {
        assert_eq!(t.feed(s), Event::NotReady, "{s:?}");
    }
    assert_eq!(t.k(), None);
    assert_eq!(t.feed(Sample::Usable { k_ns: K }), Event::Ready { k_ns: K });
    assert_eq!(t.k(), Some(K));
}

/// Catalog (dstack #1, iem-mixer to FOH, 1.11.2026 22:44 and 2.11 01:32
/// CET: thousands of late packets, peak 479 µs): dsync v1.20 reported
/// [41] locked at a phase error of about −720 µs after a gen3 reboot, and the
/// stack, which goes ready on [41] alone, stamped its TX off by that until
/// the slew ended. dsync v1.23.0 (dsync#146) holds [41] at 0 until |e| ≤
/// 50 µs for 1.5 s; byte 62's `settled` comes earlier and says nothing about
/// phase. Replayed as iemmixer measured v1.23.0's cold start (2.11 08:34
/// CET: engaged at 101 s with e −720 µs, [41] = 1 at 155 s, FOH late 0):
/// 155 polls settled but unlocked, then locked. The clock is ready only at
/// the lock, on that poll's k, and later polls on the same time hold it.
#[test]
fn a_boot_with_a_large_phase_error_is_ready_only_at_the_lock() {
    let mut t = Tracker::new();
    let stats = ClockStats::default();
    for poll in 0..155 {
        let mut r = reading(steady(D));
        r.locked = false;
        // Settled (the servo's first accepted Sync) long before the phase.
        r.settled = poll >= 30;
        r.mode = 1;
        let ev = t.feed(sample(&r, D + PTP, MONO));
        stats.record(ev);
        assert_eq!(ev, Event::NotReady, "poll {poll}");
    }
    assert_eq!(t.k(), None);
    // The lock: ready once, on this poll's k.
    let ev = t.feed(sample(&reading(steady(D)), D + PTP, MONO));
    stats.record(ev);
    assert_eq!(ev, Event::Ready { k_ns: K });
    // A minute on the same time: held, never re-anchored.
    for s in 1..=60i64 {
        let ev = t.feed(sample(
            &reading(steady(D)),
            D + PTP + s * 1_000_000_000,
            MONO + s * 1_000_000_000,
        ));
        stats.record(ev);
        assert_eq!(ev, Event::Held, "second {s}");
    }
    let counts = stats.counts();
    assert_eq!(
        (counts.polls, counts.re_anchors, counts.holdovers),
        (216, 0, 0)
    );
}

#[test]
fn one_odd_poll_is_held_three_re_anchor() {
    let mut t = Tracker::new();
    assert_eq!(t.feed(Sample::Usable { k_ns: K }), Event::Ready { k_ns: K });
    // Polls that agree within the tolerance keep k as it was.
    assert_eq!(
        t.feed(Sample::Usable {
            k_ns: K + TOLERANCE_NS
        }),
        Event::Held
    );
    assert_eq!(
        t.feed(Sample::Usable {
            k_ns: K - TOLERANCE_NS
        }),
        Event::Held
    );
    assert_eq!(t.k(), Some(K));
    // One odd poll is held; an agreeing one ends the run.
    let odd = K + 200_000_000;
    assert_eq!(t.feed(Sample::Usable { k_ns: odd }), Event::Held);
    assert_eq!(t.feed(Sample::Usable { k_ns: odd }), Event::Held);
    assert_eq!(t.feed(Sample::Usable { k_ns: K }), Event::Held);
    assert_eq!(t.feed(Sample::Usable { k_ns: odd }), Event::Held);
    assert_eq!(t.feed(Sample::Usable { k_ns: odd }), Event::Held);
    assert_eq!(t.k(), Some(K));
    // The third in a row re-anchors, to the latest.
    assert_eq!(
        t.feed(Sample::Usable { k_ns: odd + 5 }),
        Event::ReAnchored {
            delta_ns: 200_000_005
        }
    );
    assert_eq!(t.k(), Some(odd + 5));
    // From there, the old k is the odd one.
    assert_eq!(t.feed(Sample::Usable { k_ns: odd }), Event::Held);
    assert_eq!(t.feed(Sample::Usable { k_ns: K }), Event::Held);
    assert_eq!(t.feed(Sample::Usable { k_ns: K }), Event::Held);
    assert_eq!(
        t.feed(Sample::Usable { k_ns: K - 7 }),
        Event::ReAnchored {
            delta_ns: K - 7 - (odd + 5)
        }
    );
}

#[test]
fn a_k_just_past_the_tolerance_is_odd_and_odd_polls_must_agree() {
    let mut t = Tracker::new();
    t.feed(Sample::Usable { k_ns: K });
    // Three polls each just past the tolerance, agreeing with each other.
    for _ in 1..CONFIRM {
        assert_eq!(
            t.feed(Sample::Usable {
                k_ns: K + TOLERANCE_NS + 1
            }),
            Event::Held
        );
    }
    assert_eq!(
        t.feed(Sample::Usable {
            k_ns: K + TOLERANCE_NS + 1
        }),
        Event::ReAnchored {
            delta_ns: TOLERANCE_NS + 1
        }
    );
    // Odd polls that disagree with each other start the count again.
    let mut t = Tracker::new();
    t.feed(Sample::Usable { k_ns: K });
    let a = K + 1_000_000;
    let b = a + TOLERANCE_NS + 1;
    for k in [a, a, b, b, a, a] {
        assert_eq!(t.feed(Sample::Usable { k_ns: k }), Event::Held, "{k}");
    }
    assert_eq!(t.k(), Some(K));
    // Odd polls within the tolerance of the first odd one count together.
    assert_eq!(
        t.feed(Sample::Usable {
            k_ns: a + TOLERANCE_NS
        }),
        Event::ReAnchored {
            delta_ns: 1_000_000 + TOLERANCE_NS
        }
    );
    // An unusable poll breaks a run too.
    let mut t = Tracker::new();
    t.feed(Sample::Usable { k_ns: K });
    for s in [
        Sample::Usable { k_ns: a },
        Sample::Usable { k_ns: a },
        Sample::NotLocked,
        Sample::Usable { k_ns: a },
        Sample::Usable { k_ns: a },
    ] {
        assert_ne!(
            t.feed(s),
            Event::ReAnchored {
                delta_ns: 1_000_000
            },
            "{s:?}"
        );
    }
    assert_eq!(t.k(), Some(K));
}

#[test]
fn an_unlock_after_ready_is_holdover_and_keeps_k() {
    let mut t = Tracker::new();
    t.feed(Sample::Usable { k_ns: K });
    for s in [
        Sample::NotLocked,
        Sample::NoReply,
        Sample::NoDateState,
        Sample::StepNear,
        Sample::Slewing,
    ] {
        assert_eq!(t.feed(s), Event::Holdover, "{s:?}");
        assert_eq!(t.k(), Some(K));
    }
    // Back with the same k: held, no re-anchor; the clock never stopped.
    assert_eq!(t.feed(Sample::Usable { k_ns: K + 3 }), Event::Held);
    assert_eq!(t.k(), Some(K));
}

#[test]
fn the_status_counts_polls_re_anchors_and_holdover_runs() {
    let stats = ClockStats::default();
    assert_eq!(stats.counts(), ClockCounts::default());
    let held = |stats: &ClockStats| stats.counts().in_holdover;
    stats.record(Event::NotReady);
    stats.record(Event::Ready { k_ns: K });
    assert!(!held(&stats));
    // Two polls in holdover in a row are one holdover.
    stats.record(Event::Holdover);
    assert!(held(&stats));
    stats.record(Event::Holdover);
    assert_eq!(stats.counts().holdovers, 1);
    stats.record(Event::Held);
    assert!(!held(&stats));
    stats.record(Event::Holdover);
    stats.record(Event::ReAnchored { delta_ns: 25_000 });
    assert_eq!(
        stats.counts(),
        ClockCounts {
            polls: 7,
            re_anchors: 1,
            holdovers: 2,
            in_holdover: false
        }
    );
    stats.record(Event::ReAnchored { delta_ns: -25_000 });
    assert_eq!(stats.counts().re_anchors, 2);
}

#[test]
fn media_time_survives_years_of_uptime() {
    // Ten years of a grandmaster's uptime at 96 kHz.
    let ten_years_ns: u64 = 10 * 365 * 86_400 * 1_000_000_000;
    assert_eq!(to_samples(ten_years_ns, 96_000), 10 * 365 * 86_400 * 96_000);
    // Rounded down, never up.
    assert_eq!(to_samples(10_416, 96_000), 0);
    assert_eq!(to_samples(10_417, 96_000), 1);
    assert_eq!(to_samples(1_000_000_000, 48_000), 48_000);
    // The whole u64 range: no overflow (u64::MAX ns × 192 kHz).
    assert_eq!(
        to_samples(u64::MAX, 192_000),
        ((u64::MAX as u128) * 192_000 / 1_000_000_000) as u64
    );
    // A stamp and its sample index round-trip after ten years too.
    let n = to_samples(ten_years_ns, 96_000) + 12_345;
    let (secs, sub) = stamp(n, 96_000);
    assert_eq!(u64::from(secs), n / 96_000);
    assert_eq!(sub, 12_345);
    assert_eq!(from_stamp(secs, sub, 96_000, n + 96_000 * 3_600), n);
    // A rate of 0 reads as 1 and never divides by zero.
    assert_eq!(to_samples(3_000_000_000, 0), 3);
}

#[test]
fn a_latency_is_its_nearest_whole_samples() {
    assert_eq!(latency_samples(333_333, 96_000), 32);
    assert_eq!(latency_samples(250_000, 96_000), 24);
    assert_eq!(latency_samples(10_666_667, 96_000), 1024);
    // Half a sample (5 208.33 ns at 96 kHz) is the edge.
    assert_eq!(latency_samples(5_208, 96_000), 0);
    assert_eq!(latency_samples(5_209, 96_000), 1);
    assert_eq!(latency_samples(3_000_000_000, 0), 3);
    assert_eq!(latency_samples(u64::from(u32::MAX), 192_000), 824_634);
    assert_eq!(stamp(7, 0), (7, 0));
    assert_eq!(from_stamp(7, 0, 0, 9), 7);
}

#[test]
fn stamps_split_and_join_across_the_seconds_wrap() {
    let rate = 96_000u32;
    let wrap = (1u64 << 32) * u64::from(rate);
    for n in [wrap - 96_001, wrap - 1, wrap, wrap + 1, wrap + 95_999] {
        let (secs, sub) = stamp(n, rate);
        assert_eq!(u64::from(secs), (n / u64::from(rate)) % (1 << 32), "{n}");
        assert_eq!(u64::from(sub), n % u64::from(rate), "{n}");
        // Joined near the clock, on either side of the wrap.
        for near in [n - 10_000, n, n + 10_000, wrap] {
            assert_eq!(from_stamp(secs, sub, rate, near), n, "{n} near {near}");
        }
    }
    assert_eq!(stamp(wrap, rate), (0, 0));
    assert_eq!(stamp(wrap - 1, rate), (u32::MAX, rate - 1));
    // The nearest of the stamp's repeats, never more than half a period
    // away.
    let n = 5 * wrap + 777;
    let (secs, sub) = stamp(n, rate);
    assert_eq!(from_stamp(secs, sub, rate, n + wrap / 2 - 1), n);
    assert_eq!(from_stamp(secs, sub, rate, n - wrap / 2 + 1), n);
    assert_eq!(from_stamp(secs, sub, rate, n + wrap / 2 + 1), n + wrap);
    assert_eq!(from_stamp(secs, sub, rate, n - wrap / 2 - 1), n - wrap);
    // A tie goes to the later one.
    assert_eq!(from_stamp(secs, sub, rate, n + wrap / 2), n + wrap);
    // Near the clock's start the stamp never maps below zero.
    assert_eq!(from_stamp(u32::MAX, 5, rate, 3), wrap - u64::from(rate) + 5);
    assert_eq!(from_stamp(0, 5, rate, 3), 5);
    assert_eq!(from_stamp(0, 0, rate, 10), 0);
}

#[test]
fn the_manual_clock_moves_only_when_asked() {
    let c = ManualClock::new(1_000);
    assert_eq!(c.now_ns(), Some(1_000));
    assert_eq!(c.now_ns(), Some(1_000));
    c.sleep_until(5_000);
    assert_eq!(c.now_ns(), Some(5_000));
    // A past time does not move it back.
    c.sleep_until(2_000);
    assert_eq!(c.now_ns(), Some(5_000));
    c.advance(10);
    assert_eq!(c.now_ns(), Some(5_010));
    assert_eq!(c.epoch(), 0);
    c.re_anchor(-3_000);
    assert_eq!((c.now_ns(), c.epoch()), (Some(2_010), 1));
    c.re_anchor(500);
    assert_eq!((c.now_ns(), c.epoch()), (Some(2_510), 2));
    c.set_ready(false);
    assert_eq!(c.now_ns(), None);
    c.set_ready(true);
    assert_eq!(c.now_ns(), Some(2_510));
}

#[cfg(any(target_os = "linux", windows))]
mod os {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use super::super::*;
    use crate::clock::dsyx::tests::locked_reply;

    /// One hour: a k whose sign error would sleep for hours.
    const HOUR: i64 = 3_600_000_000_000;

    #[cfg(target_os = "linux")]
    /// The kernel's clock `id`, read here independently of the module.
    fn kernel(id: libc::clockid_t) -> i64 {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: a valid, writable timespec.
        unsafe { libc::clock_gettime(id, &mut ts) };
        ts.tv_sec * 1_000_000_000 + ts.tv_nsec
    }

    /// Windows: the steered clock follows the wall after a poll's pair and
    /// never runs back (QPC through the model).
    #[cfg(windows)]
    #[test]
    fn the_windows_steered_clock_follows_the_wall() {
        for _ in 0..3 {
            let m = super::super::sys::observe(realtime_ns());
            assert!((m - realtime_ns()).abs() < 5_000_000, "{m}");
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut last = monotonic_ns();
        for _ in 0..100_000 {
            let m = monotonic_ns();
            // A model update between a read of the model and of QPC may
            // put a read nanoseconds back; never more.
            assert!(m >= last - 1_000, "{last} {m}");
            last = m;
        }
        assert!((monotonic_ns() - realtime_ns()).abs() < 5_000_000);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_box_clocks_read_the_kernel_s() {
        for _ in 0..1_000 {
            let before = kernel(libc::CLOCK_MONOTONIC);
            let m = monotonic_ns();
            let after = kernel(libc::CLOCK_MONOTONIC);
            assert!(before <= m && m <= after, "{before} {m} {after}");
            let before = kernel(libc::CLOCK_REALTIME);
            let w = realtime_ns();
            let after = kernel(libc::CLOCK_REALTIME);
            assert!(before <= w && w <= after, "{before} {w} {after}");
            // After 2026-01-01 and before 2100.
            assert!(
                w > 1_767_225_600_000_000_000 && w < 4_102_444_800_000_000_000,
                "{w}"
            );
        }
    }

    #[test]
    fn the_mono_clock_is_monotonic_plus_k_and_counts_re_anchors() {
        let c = MonoClock::new();
        assert_eq!((c.now_ns(), c.k_ns(), c.epoch()), (None, None, 0));
        c.set_k(HOUR);
        let before = monotonic_ns();
        let now = c.now_ns().unwrap() as i64;
        let after = monotonic_ns();
        assert!(
            now >= before + HOUR && now <= after + HOUR,
            "{before} {now} {after}"
        );
        assert_eq!((c.k_ns(), c.epoch()), (Some(HOUR), 0));
        // The same k again changes nothing; another k re-anchors.
        c.set_k(HOUR);
        assert_eq!(c.epoch(), 0);
        c.set_k(HOUR - 5);
        assert_eq!((c.k_ns(), c.epoch()), (Some(HOUR - 5), 1));
        c.set_k(HOUR + 7);
        assert_eq!(c.epoch(), 2);
        // A k that would put PTP time below zero reads as not ready.
        c.set_k(-monotonic_ns() - HOUR);
        assert_eq!(c.now_ns(), None);
    }

    /// `sleep_until` on a thread, bounded at 5 s: a mutant that sleeps for
    /// hours fails here instead of hanging the test run.
    fn sleeps_for(c: Arc<MonoClock>, ptp_ns: u64) -> Duration {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let t0 = Instant::now();
            c.sleep_until(ptp_ns);
            let _ = tx.send(t0.elapsed());
        });
        rx.recv_timeout(Duration::from_secs(5))
            .expect("sleep_until returned within 5 s")
    }

    /// `sleep_until` of the clock's time `ahead_ns` later, both read on the
    /// sleeping thread: how long it slept, and whether the clock had
    /// reached the time when it woke. Taken on the thread, so the time its
    /// start took never counts (under a mutation shard's load it took
    /// milliseconds, and a 30 ms target set before the spawn left less than
    /// 29 ms to sleep: PR #79, shard 89).
    fn sleeps_ahead(c: Arc<MonoClock>, ahead_ns: u64) -> (Duration, bool) {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let target = c.now_ns().unwrap() + ahead_ns;
            let t0 = Instant::now();
            c.sleep_until(target);
            let slept = t0.elapsed();
            let _ = tx.send((slept, c.now_ns().unwrap() >= target));
        });
        rx.recv_timeout(Duration::from_secs(5))
            .expect("sleep_until returned within 5 s")
    }

    #[test]
    fn the_mono_clock_sleeps_until_a_ptp_time() {
        let c = Arc::new(MonoClock::new());
        c.set_k(HOUR);
        let (slept, reached) = sleeps_ahead(c.clone(), 30_000_000);
        assert!(slept >= Duration::from_millis(29), "{slept:?}");
        assert!(slept < Duration::from_secs(2), "{slept:?}");
        assert!(reached, "woke before the time asked");
        let target = c.now_ns().unwrap();
        // A past time returns at once: an hour back, so a sleep that read it
        // as anything but past would outlast the bound by far. Bounded at
        // 1 s, not tens of ms: under a mutation shard's load the thread's
        // own start and wake took more than 20 ms, and this test then
        // "caught" mutants of unrelated modules (PR #79, shards 96, 178).
        let slept = sleeps_for(c.clone(), target - HOUR as u64);
        assert!(slept < Duration::from_secs(1), "{slept:?}");
        // A negative k: the same. Smaller than the time since boot, which a
        // fresh CI runner keeps under an hour (CI run 40289615289: k = -1 h
        // made the PTP time negative, so now_ns was None).
        c.set_k(-1_000_000);
        let (slept, reached) = sleeps_ahead(c.clone(), 30_000_000);
        assert!(slept >= Duration::from_millis(29), "{slept:?}");
        assert!(slept < Duration::from_secs(2), "{slept:?}");
        assert!(reached, "woke before the time asked");
        // Before the clock is ready: a short nap, whatever the time asked.
        let c = Arc::new(MonoClock::new());
        let slept = sleeps_for(c, u64::MAX);
        assert!(slept >= Duration::from_millis(9), "{slept:?}");
        assert!(slept < Duration::from_secs(2), "{slept:?}");
    }

    /// The poller against a stand-in for dsync's time server on
    /// loopback: a locked reply with D, the last step long ago.
    #[tokio::test]
    async fn the_poller_anchors_the_clock_from_dsync_s_reply() {
        let server = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = server.local_addr().unwrap();
        let d: i64 = 1_790_000_000_000_000_000;
        let serving = tokio::spawn(async move {
            let mut buf = [0u8; 128];
            loop {
                let Ok((n, from)) = server.recv_from(&mut buf).await else {
                    return;
                };
                assert_eq!(n, dsyx::REQUEST_LEN);
                assert_eq!(&buf[0..4], b"DSYX");
                let id = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
                // A stale reply to another request first: skipped.
                let wall = realtime_ns();
                let stale = locked_reply(id.wrapping_sub(1), wall, d, wall - d - HOUR, 1);
                server.send_to(&stale, from).await.unwrap();
                // dsync 1.26's 120 bytes: the master after the extension.
                let mut reply = locked_reply(id, wall, d, wall - d - HOUR, -750);
                reply.extend_from_slice(&[0x00, 0x1d, 0xc1, 0x0d, 0x0e, 0x0f, 0, 0]);
                server.send_to(&reply, from).await.unwrap();
            }
        });
        let clock = Arc::new(MonoClock::new());
        let freq = Arc::new(AtomicI32::new(0));
        let stats = Arc::new(ClockStats::default());
        let polling = tokio::spawn(crate::clock::os::poll(
            addr,
            clock.clone(),
            freq.clone(),
            stats.clone(),
        ));
        let t0 = Instant::now();
        while stats.counts().polls == 0 && t0.elapsed() < Duration::from_secs(5) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        polling.abort();
        serving.abort();
        assert!(clock.k_ns().is_some(), "ready within 5 s");
        // The status's counts: the poll that made the clock ready (another
        // a second later at most), no re-anchor, no holdover.
        let counts = stats.counts();
        assert!((1..=2).contains(&counts.polls), "{counts:?}");
        assert_eq!(
            (counts.re_anchors, counts.holdovers, counts.in_holdover),
            (0, 0, false)
        );
        assert_eq!(freq.load(Ordering::Relaxed), -750);
        // The clock stats' ids: the grandmaster and dsync's master.
        assert_eq!(
            stats.grandmaster(),
            Some([0x00, 0x1d, 0xc1, 0x0a, 0x0b, 0x0c])
        );
        assert_eq!(stats.master(), Some([0x00, 0x1d, 0xc1, 0x0d, 0x0e, 0x0f]));
        // PTP now = the box's wall less D, within the loop's few ms.
        let ptp = clock.now_ns().unwrap() as i64;
        let expect = realtime_ns() - d;
        assert!((ptp - expect).abs() < 50_000_000, "{ptp} {expect}");
        assert_eq!(clock.epoch(), 0);
    }

    /// The stand-in answers the next request with `make(its id)`, or not at
    /// all; returns the reply sent.
    async fn answer(
        server: &tokio::net::UdpSocket,
        make: Option<fn(u32) -> Vec<u8>>,
    ) -> Option<(u32, Vec<u8>)> {
        let mut buf = [0u8; 128];
        let (n, from) = server.recv_from(&mut buf).await.unwrap();
        assert_eq!((n, &buf[0..4]), (dsyx::REQUEST_LEN, &b"DSYX"[..]));
        let id = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let reply = make?(id);
        server.send_to(&reply, from).await.unwrap();
        Some((id, reply))
    }

    const D: i64 = 1_790_000_000_000_000_000;

    /// A locked reply now, the last step an hour ago.
    fn locked_now(id: u32) -> Vec<u8> {
        let wall = realtime_ns();
        locked_reply(id, wall, D, wall - D - HOUR, -750)
    }

    /// The same, not locked.
    fn unlocked_now(id: u32) -> Vec<u8> {
        let mut b = locked_now(id);
        b[41] = 0;
        b[36..40].copy_from_slice(&12_000i32.to_be_bytes());
        b
    }

    /// One poll, the stand-in answering it with `make` (or not), bounded at
    /// 5 s.
    async fn step(
        poller: &mut Poller,
        clock: &MonoClock,
        freq: &AtomicI32,
        server: &tokio::net::UdpSocket,
        make: Option<fn(u32) -> Vec<u8>>,
    ) -> (Polled, Option<(u32, Vec<u8>)>) {
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(poller.once(clock, freq), answer(server, make))
        })
        .await
        .expect("a poll within 5 s")
    }

    /// dstack #68: the follower's seed is one poll of the service: its
    /// word when locked and settled, none when not locked or silent.
    #[tokio::test]
    async fn the_service_seed_is_one_poll_s_word() {
        let server = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = server.local_addr().unwrap();
        let ask = |make: Option<fn(u32) -> Vec<u8>>| {
            let server = &server;
            async move {
                tokio::time::timeout(Duration::from_secs(5), async {
                    tokio::join!(super::linked::service_seed(addr), answer(server, make))
                })
                .await
                .expect("a seed within 5 s")
                .0
            }
        };
        assert_eq!(ask(Some(locked_now)).await, Some(-750));
        assert_eq!(ask(Some(unlocked_now)).await, None);
        assert_eq!(ask(None).await, None);
    }

    /// One poll at a time (the `clock` subcommand's step): each reports the
    /// reply, its sample, the event and the anchored k, and sets the clock
    /// and the frequency as the service's poller does.
    #[tokio::test]
    async fn a_poll_reports_its_reply_sample_event_and_k() {
        let server = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = server.local_addr().unwrap();
        let clock = MonoClock::new();
        let freq = AtomicI32::new(0);
        let mut poller = Poller::connect(addr).await.unwrap();

        // Not locked: no k.
        let (p, sent) = step(&mut poller, &clock, &freq, &server, Some(unlocked_now)).await;
        let (id, reply) = sent.unwrap();
        let r = dsyx::parse(&reply, id).unwrap();
        assert!(!r.locked);
        assert_eq!(
            p,
            Polled {
                reading: Some(r),
                sample: Sample::NotLocked,
                event: Event::NotReady,
                k_ns: None,
            }
        );
        assert_eq!((clock.k_ns(), freq.load(Ordering::Relaxed)), (None, 12_000));

        // Locked: the first k, the clock ready with it.
        let (p, sent) = step(&mut poller, &clock, &freq, &server, Some(locked_now)).await;
        let (id, reply) = sent.unwrap();
        assert_eq!(p.reading, Some(dsyx::parse(&reply, id).unwrap()));
        let Sample::Usable { k_ns } = p.sample else {
            panic!("{p:?}");
        };
        assert_eq!(
            (p.event, p.k_ns, clock.k_ns()),
            (Event::Ready { k_ns }, Some(k_ns), Some(k_ns))
        );
        assert_eq!(freq.load(Ordering::Relaxed), -750);
        // k = PTP now less the box's monotonic clock, within the loop's ms.
        let expect = realtime_ns() - D - monotonic_ns();
        assert!((k_ns - expect).abs() < 50_000_000, "{k_ns} {expect}");

        // Silence: holdover, k and the frequency kept.
        let (p, sent) = step(&mut poller, &clock, &freq, &server, None).await;
        assert_eq!(sent, None);
        assert_eq!(
            p,
            Polled {
                reading: None,
                sample: Sample::NoReply,
                event: Event::Holdover,
                k_ns: Some(k_ns),
            }
        );
        assert_eq!(
            (clock.k_ns(), clock.epoch(), freq.load(Ordering::Relaxed)),
            (Some(k_ns), 0, -750)
        );
    }

    /// No answer: the clock stays not ready and the poller keeps polling.
    #[tokio::test]
    async fn the_poller_waits_for_a_silent_dsync() {
        let server = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = server.local_addr().unwrap();
        let clock = Arc::new(MonoClock::new());
        let freq = Arc::new(AtomicI32::new(7));
        let stats = Arc::new(ClockStats::default());
        let polling = tokio::spawn(crate::clock::os::poll(
            addr,
            clock.clone(),
            freq.clone(),
            stats,
        ));
        let mut buf = [0u8; 128];
        for _ in 0..2 {
            let got =
                tokio::time::timeout(Duration::from_secs(3), server.recv_from(&mut buf)).await;
            assert!(matches!(got, Ok(Ok((64, _)))), "a poll a second");
        }
        polling.abort();
        assert_eq!((clock.k_ns(), freq.load(Ordering::Relaxed)), (None, 7));
    }
}

#[test]
fn the_normal_profile_waits_on_the_timer_and_low_latency_spins() {
    assert_eq!(wait_mode_for(TimingProfile::LowLatency), WaitMode::Spin);
    assert_eq!(wait_mode_for(TimingProfile::Normal), WaitMode::Timer);
}

#[test]
fn a_profile_cell_keeps_the_first_profile_and_refuses_another() {
    let cell = ProfileCell::new();
    assert_eq!(
        cell.get(),
        TimingProfile::LowLatency,
        "low latency until set"
    );
    assert!(cell.set(TimingProfile::Normal));
    assert_eq!(cell.get(), TimingProfile::Normal);
    assert!(cell.set(TimingProfile::Normal), "the same again is fine");
    assert!(!cell.set(TimingProfile::LowLatency), "another is refused");
    assert_eq!(cell.get(), TimingProfile::Normal);
}

#[test]
fn only_the_low_latency_profile_asks_for_real_time_priority() {
    assert!(realtime_for(TimingProfile::LowLatency));
    assert!(!realtime_for(TimingProfile::Normal));
}

/// dstack #90: dlink's normal-profile threads ask THREAD_PRIORITY_HIGHEST
/// on Windows, normal elsewhere; low latency asks real-time everywhere.
#[test]
fn a_media_thread_s_priority_follows_the_profile_and_the_os() {
    use ThreadPriority::*;
    assert_eq!(
        thread_priority_for(TimingProfile::LowLatency, Os::Windows),
        Realtime
    );
    assert_eq!(
        thread_priority_for(TimingProfile::LowLatency, Os::Other),
        Realtime
    );
    assert_eq!(
        thread_priority_for(TimingProfile::Normal, Os::Windows),
        Highest
    );
    assert_eq!(
        thread_priority_for(TimingProfile::Normal, Os::Other),
        Normal
    );
    let here = if cfg!(windows) {
        Os::Windows
    } else {
        Os::Other
    };
    assert_eq!(Os::current(), here);
}

#[test]
fn the_spin_tail_is_the_device_s_within_its_bound_and_only_for_the_spin_wait() {
    assert_eq!(spin_tail_of(None), SPIN_TAIL_NS);
    assert_eq!(spin_tail_of(Some(200_000)), 200_000);
    assert_eq!(spin_tail_of(Some(0)), 0);
    assert_eq!(spin_tail_of(Some(SPIN_TAIL_MAX_NS)), SPIN_TAIL_MAX_NS);
    assert_eq!(spin_tail_of(Some(SPIN_TAIL_MAX_NS + 1)), SPIN_TAIL_MAX_NS);
    assert_eq!(spin_tail_for(WaitMode::Spin, 200_000), 200_000);
    assert_eq!(spin_tail_for(WaitMode::Timer, 200_000), 0);
}

#[test]
fn the_spin_escape_is_the_device_s_period_in_ns_and_off_when_unset() {
    assert_eq!(spin_escape_of(None), 0);
    assert_eq!(spin_escape_of(Some(100)), 100_000_000);
    assert_eq!(spin_escape_of(Some(SPIN_ESCAPE_MAX_MS)), 10_000_000_000);
    assert_eq!(spin_escape_of(Some(u64::MAX)), u64::MAX);
}

#[test]
fn the_presend_window_covers_the_escape_block_and_the_timer_s_late_wake() {
    assert_eq!(ESCAPE_BLOCK_NS, 1_000_000);
    assert_eq!(TIMER_LATE_NS, 1_140_000);
    assert_eq!(PRESEND_NS, 2_140_000);
}

#[test]
fn the_spin_escape_line_names_the_mode_on_windows_and_says_ignored_elsewhere() {
    assert_eq!(spin_escape_line(None, Os::Windows), None);
    assert_eq!(spin_escape_line(None, Os::Other), None);
    assert_eq!(
        spin_escape_line(Some(250), Os::Windows).unwrap(),
        "bounded escape on (trial, dstack #1): the low-latency spin does not yield, \
         each spinning media thread blocks 1000 µs on the timer every 250 ms, TX sends 2140 µs ahead first"
    );
    assert_eq!(
        spin_escape_line(Some(250), Os::Other).unwrap(),
        "spin_escape_ms = 250 is for Windows only: ignored on this OS"
    );
}

#[test]
fn a_thread_that_never_asks_escapes_once_a_period_from_its_first_wait() {
    const P: u64 = 100_000_000;
    let mut s = EscapeState::new();
    // The first wait only starts the period.
    assert!(!s.take(1_000, P));
    assert!(!s.take(1_000 + 99_999_999, P));
    assert!(s.take(1_000 + 100_000_000, P));
    // The escape starts the next period.
    assert!(!s.take(1_000 + 100_000_001, P));
    assert!(!s.take(1_000 + 199_999_999, P));
    assert!(s.take(1_000 + 200_000_000, P));
    // Off: never.
    let mut off = EscapeState::new();
    assert!(!off.take(0, 0));
    assert!(!off.take(i64::MAX, 0));
}

#[test]
fn a_thread_that_asks_escapes_only_after_an_ask_said_so() {
    const P: u64 = 100_000_000;
    let mut s = EscapeState::new();
    assert!(!s.ask(0, P));
    assert!(!s.take(0, P));
    // The period ends between the ask and the wait: no escape, since
    // nothing was sent ahead for it.
    assert!(!s.ask(99_999_999, P));
    assert!(!s.take(100_000_000, P));
    assert!(s.ask(100_000_000, P));
    assert!(s.take(100_000_050, P));
    // Taken once: the next wait does not escape again.
    assert!(!s.take(100_000_060, P));
    // The period runs from the escape itself.
    assert!(!s.ask(200_000_049, P));
    assert!(s.ask(200_000_050, P));
    // A disarming ask cancels an armed one.
    assert!(!s.ask(200_000_051, 0));
    assert!(!s.take(200_000_052, P));
}

/// TX puts off an armed escape while a packet in its window has no
/// samples, up to `ESCAPE_DEFER_MAX_NS` past the period; then the escape
/// happens anyway.
#[test]
fn an_armed_escape_is_put_off_until_20_ms_past_its_period() {
    const P: u64 = 100_000_000;
    let mut s = EscapeState::new();
    assert!(!s.defer(0, P), "nothing armed");
    assert!(!s.ask(0, P));
    assert!(!s.defer(50, P), "nothing armed");
    assert!(s.ask(100_000_000, P));
    assert!(s.defer(100_000_000, P));
    assert!(!s.take(100_000_001, P), "put off: no escape");
    assert!(s.ask(119_999_999, P));
    assert!(s.defer(119_999_999, P), "1 ns under the cap");
    assert!(s.ask(120_000_000, P));
    assert!(!s.defer(120_000_000, P), "at the cap: escapes anyway");
    assert!(s.take(120_000_001, P));
    // The next period runs from that escape.
    assert!(!s.ask(220_000_000, P));
    assert!(s.ask(220_000_001, P));
    assert_eq!(ESCAPE_DEFER_MAX_NS, 20_000_000);
}

#[cfg(target_os = "linux")]
#[test]
fn no_wait_escapes_on_linux() {
    assert!(!ask_escape());
    assert!(!defer_escape());
}

/// dstack #1 (iem-snv trial of v0.6.38, 5.11): a pump at block 64 with
/// W 64 (96 kHz, 0.67 ms each) re-bootstrapped and lost 2 blocks at every
/// escape. Woken up to `PRESEND_NS` after its last run, a pump is behind by
/// up to `PRESEND_NS` less one block, so it takes the escape's block only
/// when W covers that; FOH's pumps (W 384 and 512 at 96 kHz) do.
#[test]
fn a_pump_takes_the_escape_only_when_its_margin_covers_the_block() {
    // 96 kHz: PRESEND_NS (2.14 ms) is 205.44 samples.
    assert!(!pump_may_escape(64, 64, 96_000), "the trial's pump");
    assert!(pump_may_escape(384, 64, 96_000), "FOH master");
    assert!(pump_may_escape(512, 256, 96_000), "FOH band");
    assert!(!pump_may_escape(141, 64, 96_000), "205 samples: 2.135 ms");
    assert!(pump_may_escape(142, 64, 96_000), "206 samples: 2.146 ms");
    // 48 kHz: 102.72 samples.
    assert!(!pump_may_escape(70, 32, 48_000));
    assert!(pump_may_escape(71, 32, 48_000));
    assert!(!pump_may_escape(10_000, 10_000, 0), "no rate");
}

/// dstack #1: a pump out of the escape says so at its start, only when
/// the escape is on; and the process's escape is on while a period is set.
#[test]
fn a_pump_out_of_the_escape_says_so_and_the_escape_is_on_while_set() {
    assert_eq!(
        pump_escape_line(true, false, 64, 64).unwrap(),
        "the engine pump stays out of the bounded escape: its margin (64) and block (64) \
         are under the escape's block (2140 µs); it spins yielding"
    );
    assert_eq!(pump_escape_line(true, true, 384, 64), None);
    assert_eq!(pump_escape_line(false, false, 64, 64), None);
    set_spin_escape_ms(Some(250));
    assert!(escape_on());
    set_spin_escape_ms(None);
    assert!(!escape_on());
}
