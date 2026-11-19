// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #440: a sleep bounded by the caller's own time read
//! ([`PtpClock::sleep_until_within`]), on every clock.

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::linked::{Follow, Linked, Source, View};
use super::*;

#[test]
fn the_deadline_is_the_target_at_most_the_bound_past_now() {
    assert_eq!(bounded_deadline(100, 40, 50), 90);
    assert_eq!(bounded_deadline(80, 40, 50), 80);
    assert_eq!(bounded_deadline(90, 40, 50), 90);
    assert_eq!(bounded_deadline(-10, -100, 50), -50);
    // A target in the past stays past: the sleep returns at once.
    assert_eq!(bounded_deadline(10, 40, 50), 10);
    // Saturating: no bound is no limit.
    assert_eq!(bounded_deadline(i64::MAX, i64::MAX - 5, 10), i64::MAX);
    assert_eq!(bounded_deadline(7, 3, u64::MAX), 7);
}

#[test]
fn a_manual_clock_moves_to_the_target_at_most_the_bound_on() {
    let clock = ManualClock::new(1_000);
    clock.sleep_until_within(5_000, 300);
    assert_eq!(clock.now_ns(), Some(1_300));
    clock.sleep_until_within(1_400, 1_000);
    assert_eq!(clock.now_ns(), Some(1_400));
    // A target already past moves nothing.
    clock.sleep_until_within(100, 50);
    assert_eq!(clock.now_ns(), Some(1_400));
}

/// A clock with only the trait's required methods: the target each sleep
/// was asked.
#[derive(Default)]
struct Plain {
    now: AtomicU64,
    ready: AtomicBool,
    asked: AtomicU64,
}

impl PtpClock for Plain {
    fn now_ns(&self) -> Option<u64> {
        self.ready
            .load(Ordering::SeqCst)
            .then(|| self.now.load(Ordering::SeqCst))
    }

    fn sleep_until(&self, ptp_ns: u64) {
        self.asked.store(ptp_ns, Ordering::SeqCst);
    }

    fn epoch(&self) -> u32 {
        0
    }
}

#[test]
fn the_default_bounds_the_target_through_one_time_read() {
    let clock = Plain::default();
    clock.now.store(1_000, Ordering::SeqCst);
    clock.ready.store(true, Ordering::SeqCst);
    clock.sleep_until_within(10_000, 500);
    assert_eq!(clock.asked.load(Ordering::SeqCst), 1_500);
    clock.sleep_until_within(1_200, 500);
    assert_eq!(clock.asked.load(Ordering::SeqCst), 1_200);
    // Not ready: the target as asked (the sleep naps).
    clock.ready.store(false, Ordering::SeqCst);
    clock.sleep_until_within(10_000, 500);
    assert_eq!(clock.asked.load(Ordering::SeqCst), 10_000);
}

/// How long `sleep` took, real time.
fn took(sleep: impl FnOnce()) -> Duration {
    let start = Instant::now();
    sleep();
    start.elapsed()
}

/// The bound in these tests, and the least a sleep to it takes.
const BOUND_NS: u64 = 3_000_000;
const AT_LEAST: Duration = Duration::from_millis(2);
/// Far under the targets past the bound (10 s, 10 s back).
const AT_MOST: Duration = Duration::from_secs(1);

/// A ready clock with any k.
fn service() -> Arc<MonoClock> {
    let clock = Arc::new(MonoClock::new());
    clock.set_k(1_700_000_000_000_000_000 - monotonic_ns());
    clock
}

fn assert_bounded(t: Duration, what: &str) {
    assert!(t >= AT_LEAST && t < AT_MOST, "{what}: {t:?}");
}

#[test]
fn the_service_clock_sleeps_to_the_target_at_most_the_bound() {
    let clock = service();
    let now = clock.now_ns().unwrap();
    assert_bounded(
        took(|| clock.sleep_until_within(now + 10_000_000_000, BOUND_NS)),
        "a far target",
    );
    let now = clock.now_ns().unwrap();
    assert_bounded(
        took(|| clock.sleep_until_within(now + BOUND_NS, 10_000_000_000)),
        "a near target",
    );
    // A new k 10 s back after the target was read: the bound holds.
    let target = clock.now_ns().unwrap() + BOUND_NS;
    clock.set_k(clock.k_ns().unwrap() - 10_000_000_000);
    assert_bounded(
        took(|| clock.sleep_until_within(target, BOUND_NS)),
        "a k 10 s back",
    );
}

/// A locked follower on the monotonic clock.
struct OnMonotonic;

impl Follow for OnMonotonic {
    fn now_ns(&self) -> Option<i64> {
        Some(monotonic_ns() + 1_700_000_000_000_000_000)
    }

    fn view(&self) -> View {
        View {
            alive: true,
            locked: true,
            settled: true,
            anchored: true,
            epoch: 0,
        }
    }

    fn counts(&self) -> FollowerCounts {
        FollowerCounts::default()
    }
}

#[test]
fn the_linked_clock_sleeps_to_the_target_at_most_the_bound_on_either_source() {
    let on_follower = Linked::new(
        Some(Box::new(OnMonotonic)),
        Arc::new(MonoClock::new()),
        None,
    );
    let on_service = Linked::new(None, service(), None);
    for (linked, source) in [
        (on_follower, Source::Internal),
        (on_service, Source::Service),
    ] {
        assert_eq!(linked.tick(), source);
        let now = linked.now_ns().unwrap();
        assert_bounded(
            took(|| linked.sleep_until_within(now + 10_000_000_000, BOUND_NS)),
            &format!("{source:?}: a far target"),
        );
        let now = linked.now_ns().unwrap();
        assert_bounded(
            took(|| linked.sleep_until_within(now + BOUND_NS, 10_000_000_000)),
            &format!("{source:?}: a near target"),
        );
        // No bound: the target.
        let now = linked.now_ns().unwrap();
        assert_bounded(
            took(|| linked.sleep_until(now + BOUND_NS)),
            &format!("{source:?}: sleep_until"),
        );
    }
}
