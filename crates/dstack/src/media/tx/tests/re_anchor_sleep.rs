// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #440: the TX loop's sleep, like the pump's, re-read the time
//! base after the pass's own read, so a re-anchor between the two (16.11.2026
//! on iemmixer-snv: the service's k by -43.9 days) stretched it by the whole
//! step. Here the clock takes a new k as the loop's sleep is asked (after
//! every read of the pass, before the sleep's own); the flow must keep
//! sending.

use std::sync::atomic::AtomicI64;

use super::*;
use crate::clock::{MonoClock, ReAnchor, monotonic_ns};

/// The field's step (16.11.2026, the service's `delta_ns`).
const FIELD_STEP_NS: i64 = -3_792_840_297_399_476;
/// Sleeps before the step.
const SLEEPS: u64 = 50;
/// Packets the flow must send after the step, and in what real time: at
/// the test device's fpp that is a few ms; a sleep stretched by seconds
/// or days never makes it.
const AFTER: u64 = 100;
const RESUME: Duration = Duration::from_secs(1);

/// The service's clock that takes the step `step_ns` once, as its
/// `SLEEPS`th sleep is asked, before the sleep reads anything.
struct StepsAtSleep {
    inner: MonoClock,
    sleeps: AtomicU64,
    step_ns: AtomicI64,
}

impl StepsAtSleep {
    fn asked(&self) {
        if self.sleeps.fetch_add(1, Ordering::SeqCst) + 1 >= SLEEPS {
            let step = self.step_ns.swap(0, Ordering::SeqCst);
            if step != 0 {
                self.inner.set_k(self.inner.k_ns().unwrap() + step);
            }
        }
    }
}

impl PtpClock for StepsAtSleep {
    fn now_ns(&self) -> Option<u64> {
        self.inner.now_ns()
    }

    fn sleep_until(&self, ptp_ns: u64) {
        self.asked();
        self.inner.sleep_until(ptp_ns);
    }

    fn sleep_until_within(&self, ptp_ns: u64, max_ns: u64) {
        self.asked();
        self.inner.sleep_until_within(ptp_ns, max_ns);
    }

    fn epoch(&self) -> u32 {
        self.inner.epoch()
    }

    fn last_re_anchor(&self) -> Option<ReAnchor> {
        self.inner.last_re_anchor()
    }
}

#[test]
fn a_re_anchor_before_the_tx_loop_s_sleep_never_stops_its_flows() {
    for step in [-3_000_000_000, FIELD_STEP_NS, 3_000_000_000, -FIELD_STEP_NS] {
        let shared = shared_with(Vec::new());
        assert!(shared.set_source(0, Source::Counter));
        let rx = socket();
        let (slot, _) = add(&shared, &req(v4(&rx)));
        let clock = Arc::new(StepsAtSleep {
            inner: MonoClock::new(),
            sleeps: AtomicU64::new(0),
            step_ns: AtomicI64::new(step),
        });
        clock
            .inner
            .set_k(i64::try_from(T).unwrap() - monotonic_ns());
        let thread = {
            let clock = Arc::clone(&clock);
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || run_with(&*clock, shared, false))
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while clock.step_ns.load(Ordering::SeqCst) != 0 {
            assert!(Instant::now() < deadline, "{step}: the step never came");
            std::thread::sleep(Duration::from_millis(1));
        }
        let stepped = Instant::now();
        let before = shared.lane(slot).packets;
        while shared.lane(slot).packets < before + AFTER {
            // The loop stays asleep: leave its thread (it would hold the
            // test for the whole step) and fail.
            assert!(
                stepped.elapsed() < RESUME,
                "{step}: the TX loop sent nothing for {RESUME:?} after the clock moved: {:?}",
                shared.lane(slot)
            );
            drain(&rx);
            std::thread::sleep(Duration::from_millis(1));
        }
        shared.stop();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !thread.is_finished() {
            assert!(Instant::now() < deadline, "{step}: the TX loop did not end");
            std::thread::sleep(Duration::from_millis(1));
        }
        thread.join().unwrap();
    }
}
