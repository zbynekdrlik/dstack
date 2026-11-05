use std::sync::Mutex;

use super::*;

const ALIVE_LOCKED: View = View {
    alive: true,
    locked: true,
    settled: true,
    anchored: true,
    epoch: 0,
};
const HOLDOVER: View = View {
    alive: true,
    locked: false,
    settled: false,
    anchored: true,
    epoch: 0,
};
/// Locked, but its boot fit not done yet (dsync 1.27.3: 20-60 µs off for
/// the first 30-70 s without a seed).
const UNSETTLED: View = View {
    settled: false,
    ..ALIVE_LOCKED
};

/// The selector's rule from no internal choice yet (a fresh device).
fn choose_from(internal: Option<View>, service_ready: bool, was_locked: bool) -> Source {
    choose(internal, service_ready, was_locked, Source::None)
}

/// dstack #68 (dsync 1.27.3 on mbc): a fresh follower locks in 9 s but is
/// 20-60 µs off until it settles; the service is better meanwhile. It takes
/// over only once settled, unless the service is not ready; once it is the
/// source, losing `settled` alone does not switch back (a switch costs a
/// re-align).
#[test]
fn a_locked_follower_takes_over_from_a_ready_service_only_once_settled() {
    assert_eq!(
        choose(Some(UNSETTLED), true, true, Source::None),
        Source::Service
    );
    assert_eq!(
        choose(Some(UNSETTLED), true, true, Source::Service),
        Source::Service
    );
    assert_eq!(
        choose(Some(ALIVE_LOCKED), true, true, Source::Service),
        Source::Internal
    );
    // No service: the locked follower, settled or not.
    assert_eq!(
        choose(Some(UNSETTLED), false, true, Source::None),
        Source::Internal
    );
    // In use: it stays while locked, settled or not.
    assert_eq!(
        choose(Some(UNSETTLED), true, true, Source::Internal),
        Source::Internal
    );
    // Unlocked in use: the service, as before.
    assert_eq!(
        choose(Some(HOLDOVER), true, true, Source::Internal),
        Source::Service
    );
}

#[test]
fn the_follower_wins_when_locked_else_the_service_else_a_holdover_after_a_lock() {
    assert_eq!(
        choose_from(Some(ALIVE_LOCKED), true, true),
        Source::Internal
    );
    assert_eq!(
        choose_from(Some(ALIVE_LOCKED), false, false),
        Source::Internal
    );
    // Not locked: the service when it is ready.
    assert_eq!(choose_from(Some(HOLDOVER), true, true), Source::Service);
    assert_eq!(choose_from(None, true, false), Source::Service);
    // Neither locked nor a service: the follower's holdover, only after a lock.
    assert_eq!(choose_from(Some(HOLDOVER), false, true), Source::Internal);
    assert_eq!(choose_from(Some(HOLDOVER), false, false), Source::None);
    // A dead follower is never chosen, locked or not.
    let dead = View {
        alive: false,
        ..ALIVE_LOCKED
    };
    assert_eq!(choose_from(Some(dead), false, true), Source::None);
    assert_eq!(choose_from(Some(dead), true, true), Source::Service);
    assert_eq!(choose_from(None, false, true), Source::None);
    // Unanchored and unlocked: nothing.
    assert_eq!(
        choose_from(Some(View::default()), false, true),
        Source::None
    );
}

#[test]
fn a_switch_changes_the_epoch_s_high_half_and_a_re_anchor_its_low_half() {
    assert_eq!(epoch_of(0, 0), 0);
    assert_eq!(epoch_of(1, 0), 0x1_0000);
    assert_eq!(epoch_of(1, 7), 0x1_0007);
    assert_eq!(epoch_of(2, 0x1_0003), 0x2_0003, "the low half wraps");
    assert_ne!(epoch_of(1, 1), epoch_of(2, 0));
}

/// A follower the test drives.
struct Fake(Mutex<(Option<i64>, View)>);

impl Follow for Arc<Fake> {
    fn now_ns(&self) -> Option<i64> {
        self.0.lock().unwrap().0
    }
    fn view(&self) -> View {
        self.0.lock().unwrap().1
    }
    /// Counts the test can tell apart from the service's: 7 windows, the
    /// view's epoch as re-anchors, 2 holdovers, in holdover while unlocked,
    /// 1 absorbed.
    fn counts(&self) -> FollowerCounts {
        let v = self.view();
        FollowerCounts {
            counts: ClockCounts {
                polls: 7,
                re_anchors: v.epoch,
                holdovers: 2,
                in_holdover: !v.locked,
            },
            absorbed: 1,
            locked: v.locked,
            settled: v.settled,
            offset_ns: Some(-39_806),
            recent_abs_e_ns: None,
        }
    }
}

fn set(f: &Fake, now: Option<i64>, view: View) {
    *f.0.lock().unwrap() = (now, view);
}

#[test]
fn the_linked_clock_reads_the_chosen_source_and_moves_its_epoch_on_a_switch() {
    let fake = Arc::new(Fake(Mutex::new((None, View::default()))));
    let service = Arc::new(MonoClock::new());
    let linked = Linked::new(
        Some(Box::new(Arc::clone(&fake))),
        Arc::clone(&service),
        None,
    );
    assert!(linked.has_internal());
    // Nothing ready.
    assert_eq!(linked.tick(), Source::None);
    assert_eq!(linked.now_ns(), None);
    let e0 = linked.epoch();
    // The service becomes ready: it is read.
    service.set_k(1_000_000_000_000);
    assert_eq!(linked.tick(), Source::Service);
    let from_service = linked.now_ns().unwrap();
    assert!(from_service >= 1_000_000_000_000, "{from_service}");
    let e1 = linked.epoch();
    assert_ne!(e1, e0);
    // The follower locks, far off the service: it waits to agree (v0.6.31),
    // then wins with a re-align, and its time is read.
    set(&fake, Some(42_000_000_000), ALIVE_LOCKED);
    for _ in 0..HANDOVER_WAIT {
        assert_eq!(linked.tick(), Source::Service);
    }
    assert_eq!(linked.tick(), Source::Internal);
    assert_eq!(linked.now_ns(), Some(42_000_000_000));
    let e2 = linked.epoch();
    assert_ne!(e2, e1);
    // A re-anchor of the follower moves the epoch too.
    set(
        &fake,
        Some(42_000_000_000),
        View {
            epoch: 1,
            ..ALIVE_LOCKED
        },
    );
    assert_eq!(linked.tick(), Source::Internal);
    assert_ne!(linked.epoch(), e2);
    // Unlocked again: back to the service, a new epoch.
    let e3 = linked.epoch();
    set(&fake, Some(42_000_000_000), HOLDOVER);
    assert_eq!(linked.tick(), Source::Service);
    assert_ne!(linked.epoch(), e3);
    assert_eq!(linked.source(), Source::Service);
    // A negative PTP time from the follower reads as not ready (it never
    // agrees with the service: taken after the wait).
    set(&fake, Some(-1), ALIVE_LOCKED);
    for _ in 0..HANDOVER_WAIT {
        assert_eq!(linked.tick(), Source::Service);
    }
    assert_eq!(linked.tick(), Source::Internal);
    assert_eq!(linked.now_ns(), None);
}

#[test]
fn without_a_follower_the_linked_clock_is_the_service_s() {
    let service = Arc::new(MonoClock::new());
    let linked = Linked::new(None, Arc::clone(&service), None);
    assert!(!linked.has_internal());
    assert_eq!(linked.tick(), Source::None);
    service.set_k(5_000_000_000);
    assert_eq!(linked.tick(), Source::Service);
    let now = linked.now_ns().unwrap();
    let svc = service.now_ns().unwrap();
    assert!(svc >= now && svc - now < 1_000_000_000, "{now} {svc}");
    // The service's own re-anchor moves the linked epoch.
    let e = linked.epoch();
    service.set_k(6_000_000_000);
    assert_ne!(linked.epoch(), e);
}

#[test]
fn a_sleep_on_the_follower_s_time_returns_at_its_target() {
    let fake = Arc::new(Fake(Mutex::new((None, View::default()))));
    let service = Arc::new(MonoClock::new());
    let linked = Linked::new(Some(Box::new(Arc::clone(&fake))), service, None);
    set(&fake, Some(1_000_000_000), ALIVE_LOCKED);
    assert_eq!(linked.tick(), Source::Internal);
    // The fake's time stands still: a sleep 2 ms ahead takes about 2 ms.
    let t = std::time::Instant::now();
    linked.sleep_until(1_002_000_000);
    let took = t.elapsed();
    assert!(took >= Duration::from_millis(2), "{took:?}");
    assert!(took < Duration::from_millis(500), "{took:?}");
    // Past: at once.
    let t = std::time::Instant::now();
    linked.sleep_until(999_000_000);
    assert!(t.elapsed() < Duration::from_millis(100));
}

/// The follower's start returns a clock and its keeper together, or
/// neither (no capability for port 319, as on a runner): never one alone.
#[test]
fn the_follower_starts_with_its_keeper_or_not_at_all() {
    let (internal, keep) = start_internal("lo", None);
    assert_eq!(internal.is_some(), keep.is_some());
    let (internal, keep) = start_internal("lo", Some(-4_671));
    assert_eq!(internal.is_some(), keep.is_some());
}

/// The linked clock keeps a ready service while the follower is locked but
/// not settled, takes the follower once settled, and keeps it after.
#[test]
fn the_linked_clock_waits_for_a_settled_follower_while_the_service_is_ready() {
    let fake = Arc::new(Fake(Mutex::new((None, View::default()))));
    let service = Arc::new(MonoClock::new());
    let linked = Linked::new(
        Some(Box::new(Arc::clone(&fake))),
        Arc::clone(&service),
        None,
    );
    service.set_k(1_000_000_000_000);
    set(&fake, Some(42_000_000_000), UNSETTLED);
    assert_eq!(linked.tick(), Source::Service);
    assert_eq!(linked.tick(), Source::Service);
    // Settled, and in agreement with the service: it takes over at once.
    set(&fake, service_now(&service), ALIVE_LOCKED);
    assert_eq!(linked.tick(), Source::Internal);
    set(&fake, Some(42_000_000_000), UNSETTLED);
    assert_eq!(
        linked.tick(),
        Source::Internal,
        "kept: a switch costs a re-align"
    );
}

fn reading(locked: bool, settled: bool, freq_ppb: i32) -> crate::clock::dsyx::Reading {
    crate::clock::dsyx::Reading {
        wall_ns: 0,
        locked,
        settled,
        mode: 0,
        freq_ppb,
        grandmaster: [0; 6],
        master: [0; 6],
        date: None,
    }
}

/// dstack #68: the follower is seeded with the service's frequency word
/// (DSYX bytes 36-39, ppb) only when the service is locked and settled; a
/// word learned while it is still pulling in is no seed.
#[test]
fn the_seed_is_the_word_of_a_locked_settled_service() {
    assert_eq!(seed_of(&reading(true, true, -4_671)), Some(-4_671));
    assert_eq!(seed_of(&reading(true, true, 0)), Some(0));
    assert_eq!(seed_of(&reading(true, false, -4_671)), None);
    assert_eq!(seed_of(&reading(false, true, -4_671)), None);
    assert_eq!(seed_of(&reading(false, false, -4_671)), None);
}

/// Only a lock counts for the holdover rule: an alive follower that is
/// anchored but never locked is not taken when the service goes away.
#[test]
fn a_follower_never_locked_is_not_taken_in_holdover() {
    let fake = Arc::new(Fake(Mutex::new((Some(7), HOLDOVER))));
    let service = Arc::new(MonoClock::new());
    let linked = Linked::new(Some(Box::new(Arc::clone(&fake))), service, None);
    assert_eq!(linked.tick(), Source::None);
    assert_eq!(linked.now_ns(), None);
    // Locked once, then in holdover: taken.
    set(&fake, Some(7), ALIVE_LOCKED);
    assert_eq!(linked.tick(), Source::Internal);
    set(&fake, Some(7), HOLDOVER);
    assert_eq!(linked.tick(), Source::Internal);
}

#[test]
fn a_follower_that_did_not_start_says_why_in_a_word() {
    use dsync_ptp::follower::FollowerError;
    assert_eq!(
        follower_failure(&FollowerError::NpcapMissing {
            detail: "wpcap.dll".into()
        }),
        "Npcap is not installed"
    );
    assert_eq!(
        follower_failure(&FollowerError::PermissionDenied {
            cause: std::io::Error::from(std::io::ErrorKind::PermissionDenied).into()
        }),
        "port 319 needs CAP_NET_BIND_SERVICE or root"
    );
    assert_eq!(
        follower_failure(&FollowerError::InterfaceNotFound {
            interface: "eth9".into(),
            cause: std::io::Error::from(std::io::ErrorKind::NotFound).into()
        }),
        "no such interface, or no IPv4 on it"
    );
    assert_eq!(
        follower_failure(&FollowerError::Other(
            std::io::Error::from(std::io::ErrorKind::Other).into()
        )),
        "another failure"
    );
}

/// The service's time now, as the follower's type.
fn service_now(service: &MonoClock) -> Option<i64> {
    service.now_ns().map(|t| i64::try_from(t).unwrap())
}

/// The service's time now moved by `offset_ns`.
fn service_now_plus(service: &MonoClock, offset_ns: i64) -> Option<i64> {
    service_now(service).map(|t| t + offset_ns)
}

/// A linked clock on a ready service with a fake follower.
fn linked_on_service() -> (Arc<Fake>, Arc<MonoClock>, Linked) {
    let fake = Arc::new(Fake(Mutex::new((None, View::default()))));
    let service = Arc::new(MonoClock::new());
    let linked = Linked::new(
        Some(Box::new(Arc::clone(&fake))),
        Arc::clone(&service),
        None,
    );
    service.set_k(1_000_000_000_000);
    assert_eq!(linked.tick(), Source::Service);
    (fake, service, linked)
}

/// iemmixer (4.11.2026, v0.6.29 live): every engine start re-aligned the
/// pump once, at the Service -> Internal switch about 35 s in (epoch 65536
/// -> 131072, missed 0). The follower and the service trail the grandmaster
/// by the same receive latency: a settled follower that agrees with the
/// service takes over with the epoch unchanged, so neither the pump nor the
/// TX flows start again; its own re-anchor after that still moves it.
#[test]
fn a_follower_that_settles_in_agreement_takes_over_without_a_new_epoch() {
    for offset in [0, 10_000, -10_000] {
        let (fake, service, linked) = linked_on_service();
        let before = linked.epoch();
        set(&fake, service_now_plus(&service, offset), ALIVE_LOCKED);
        assert_eq!(linked.tick(), Source::Internal, "offset {offset}");
        assert_eq!(linked.epoch(), before, "offset {offset}: no re-align");
        assert_eq!(linked.tick(), Source::Internal);
        assert_eq!(linked.epoch(), before, "offset {offset}: stays");
        // Its own re-anchor is a new epoch.
        set(
            &fake,
            service_now(&service),
            View {
                epoch: 1,
                ..ALIVE_LOCKED
            },
        );
        assert_ne!(linked.epoch(), before, "offset {offset}: a re-anchor");
    }
}

/// A seamless handover carries the re-anchors already counted: a service
/// that re-anchored twice before the switch hands over the same epoch, to
/// a follower whose own count is elsewhere, and back.
#[test]
fn a_seamless_handover_keeps_an_epoch_with_re_anchors_in_it() {
    let (fake, service, linked) = linked_on_service();
    service.set_k(1_000_100_000_000);
    service.set_k(1_000_200_000_000);
    let before = linked.epoch();
    assert_eq!(before & 0xffff, 2, "{before:#x}");
    set(
        &fake,
        service_now(&service),
        View {
            epoch: 9,
            ..ALIVE_LOCKED
        },
    );
    assert_eq!(linked.tick(), Source::Internal);
    assert_eq!(linked.epoch(), before);
    // And back: the follower loses its lock, the service agrees.
    set(
        &fake,
        service_now(&service),
        View {
            epoch: 9,
            ..HOLDOVER
        },
    );
    assert_eq!(linked.tick(), Source::Service);
    assert_eq!(linked.epoch(), before, "seamless back too");
    // The service's next re-anchor is a new epoch.
    service.set_k(1_000_300_000_000);
    assert_ne!(linked.epoch(), before);
}

/// A follower that disagrees with a ready service waits up to
/// [`HANDOVER_WAIT`] rounds to come into agreement, then takes over with a
/// re-align (no more breaks than before v0.6.31); one that agrees within
/// the wait takes over seamlessly.
#[test]
fn a_follower_that_disagrees_waits_then_takes_over_with_a_new_epoch() {
    let (fake, service, linked) = linked_on_service();
    let before = linked.epoch();
    for _ in 0..HANDOVER_WAIT {
        set(&fake, service_now_plus(&service, 1_000_000), ALIVE_LOCKED);
        assert_eq!(linked.tick(), Source::Service);
        assert_eq!(linked.epoch(), before);
    }
    set(&fake, service_now_plus(&service, 1_000_000), ALIVE_LOCKED);
    assert_eq!(linked.tick(), Source::Internal);
    assert_ne!(linked.epoch(), before, "a re-align after the wait");

    // Agreement within the wait: seamless.
    let (fake, service, linked) = linked_on_service();
    let before = linked.epoch();
    for _ in 0..HANDOVER_WAIT - 1 {
        set(&fake, service_now_plus(&service, -1_000_000), ALIVE_LOCKED);
        assert_eq!(linked.tick(), Source::Service);
    }
    set(&fake, service_now(&service), ALIVE_LOCKED);
    assert_eq!(linked.tick(), Source::Internal);
    assert_eq!(linked.epoch(), before);

    // A wait that ended (the follower unsettled) starts again from zero.
    let (fake, service, linked) = linked_on_service();
    for _ in 0..HANDOVER_WAIT - 1 {
        set(&fake, service_now_plus(&service, 1_000_000), ALIVE_LOCKED);
        assert_eq!(linked.tick(), Source::Service);
    }
    set(&fake, service_now_plus(&service, 1_000_000), UNSETTLED);
    assert_eq!(linked.tick(), Source::Service);
    set(&fake, service_now_plus(&service, 1_000_000), ALIVE_LOCKED);
    assert_eq!(linked.tick(), Source::Service, "the wait starts again");
}

/// A forced switch (the follower in use lost its lock) never waits: a
/// re-align when the service disagrees (seamless when it agrees: above).
#[test]
fn a_forced_switch_that_disagrees_re_aligns_at_once() {
    let (fake, service, linked) = linked_on_service();
    set(&fake, service_now(&service), ALIVE_LOCKED);
    assert_eq!(linked.tick(), Source::Internal);
    let before = linked.epoch();
    set(&fake, service_now_plus(&service, 1_000_000), HOLDOVER);
    assert_eq!(linked.tick(), Source::Service);
    assert_ne!(linked.epoch(), before);
}

/// The first round's waits are logged once, with the offset; a seamless
/// handover and a re-align each log their own line.
#[test]
fn the_handover_logs_its_wait_once_and_its_outcome() {
    crate::test_log::undecided();
    let (fake, service, linked) = linked_on_service();
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        for _ in 0..3 {
            set(&fake, service_now_plus(&service, 1_000_000), ALIVE_LOCKED);
            linked.tick();
        }
    });
    assert_eq!(log.matches(WAIT_LINE).count(), 1, "{log}");
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        set(&fake, service_now(&service), ALIVE_LOCKED);
        linked.tick();
    });
    assert_eq!(log.matches(SEAMLESS_LINE).count(), 1, "{log}");
    assert_eq!(log.matches(WAIT_LINE).count(), 0, "{log}");
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        set(&fake, service_now_plus(&service, 1_000_000), HOLDOVER);
        linked.tick();
    });
    assert_eq!(log.matches(RE_ALIGN_LINE).count(), 1, "{log}");
}

#[test]
fn the_handover_is_seamless_within_the_bound_else_waits_when_voluntary() {
    assert_eq!(handover(Some(0), true, 0), Handover::Seamless);
    assert_eq!(handover(Some(HANDOVER_NS), true, 0), Handover::Seamless);
    assert_eq!(handover(Some(-HANDOVER_NS), false, 0), Handover::Seamless);
    assert_eq!(handover(Some(HANDOVER_NS + 1), true, 0), Handover::Wait);
    assert_eq!(handover(Some(-HANDOVER_NS - 1), true, 0), Handover::Wait);
    assert_eq!(
        handover(Some(HANDOVER_NS + 1), true, HANDOVER_WAIT - 1),
        Handover::Wait
    );
    assert_eq!(
        handover(Some(HANDOVER_NS + 1), true, HANDOVER_WAIT),
        Handover::Break
    );
    assert_eq!(handover(Some(HANDOVER_NS + 1), false, 0), Handover::Break);
    assert_eq!(handover(None, true, 0), Handover::Wait);
    assert_eq!(handover(None, false, 0), Handover::Break);
    assert_eq!(handover(Some(i64::MIN), false, 0), Handover::Break);
}

/// The bound against the pump's tolerance (its schedule alone, at 192 kHz;
/// `stream::tests` runs the whole pump at 96 kHz): at the tightest schedule the
/// stack allows (192 kHz, block 16, W 16), a step of [`HANDOVER_NS`] either
/// way at any point of a block is a late block or a wait, never a
/// re-bootstrap.
#[test]
fn the_pump_absorbs_a_handover_step_at_its_tightest_schedule() {
    use crate::stream::{Blocks, Due, Timing};
    let step = crate::clock::to_samples(HANDOVER_NS.unsigned_abs(), 192_000) + 1;
    assert_eq!(step, 6);
    let t = Timing {
        block: 16,
        playout: 16,
        margin: 16,
    };
    for forward in [true, false] {
        for into in 0..t.block {
            let mut b = Blocks::bootstrap(1_000_000, t);
            // In time: every block due up to here ran at its due time.
            let mut now = b.due();
            while let Due::Run { .. } | Due::Wait { .. } = b.next(now) {
                if b.due() > 1_000_000 + 10 * t.block {
                    break;
                }
                now = b.due();
            }
            let at = now + into;
            let mut now = if forward { at + step } else { at - step };
            for _ in 0..4 {
                match b.next(now) {
                    Due::Run { .. } => {}
                    Due::Wait { until } => now = until,
                    Due::Rebootstrap { .. } => {
                        panic!("forward {forward} into {into}: a re-bootstrap")
                    }
                }
            }
        }
    }
}

/// The follower's counts, for the status (v0.6.31); none without one.
#[test]
fn the_linked_clock_reads_the_follower_s_counts() {
    let (fake, _service, linked) = linked_on_service();
    set(&fake, None, HOLDOVER);
    assert_eq!(
        linked.follower_counts(),
        Some(FollowerCounts {
            counts: ClockCounts {
                polls: 7,
                re_anchors: 0,
                holdovers: 2,
                in_holdover: true
            },
            absorbed: 1,
            locked: false,
            settled: false,
            offset_ns: Some(-39_806),
            recent_abs_e_ns: None,
        })
    );
    let service = Arc::new(MonoClock::new());
    assert_eq!(Linked::new(None, service, None).follower_counts(), None);
}

/// A first source after none is its own line (no re-align to report), and
/// losing every source is a warning.
#[test]
fn the_first_source_and_no_source_log_their_own_lines() {
    crate::test_log::undecided();
    let fake = Arc::new(Fake(Mutex::new((None, View::default()))));
    let service = Arc::new(MonoClock::new());
    let linked = Linked::new(Some(Box::new(Arc::clone(&fake))), service, None);
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        set(&fake, Some(7_000_000_000), ALIVE_LOCKED);
        assert_eq!(linked.tick(), Source::Internal);
    });
    assert_eq!(log.matches(FIRST_SOURCE_LINE).count(), 1, "{log}");
    assert_eq!(log.matches(RE_ALIGN_LINE).count(), 0, "{log}");
    let log = crate::test_log::capture_warnings(|| {
        set(
            &fake,
            Some(7_000_000_000),
            View {
                alive: false,
                ..ALIVE_LOCKED
            },
        );
        assert_eq!(linked.tick(), Source::None);
    });
    assert_eq!(log.matches(NO_SOURCE_LINE).count(), 1, "{log}");
    assert_eq!(log.matches(FIRST_SOURCE_LINE).count(), 0, "{log}");
}

/// A follower whose time is the service's own, read slowly (it returns
/// `slow_ns` of the service's time after the read began) or with a
/// re-anchor of its own during the read.
struct Tricky {
    service: Arc<MonoClock>,
    slow_ns: u64,
    re_anchor_on_read: bool,
    view: Mutex<View>,
}

impl Follow for Arc<Tricky> {
    fn now_ns(&self) -> Option<i64> {
        if self.re_anchor_on_read {
            self.view.lock().unwrap().epoch += 1;
        }
        let target = self.service.now_ns()? + self.slow_ns;
        while self.service.now_ns()? < target {
            std::hint::spin_loop();
        }
        i64::try_from(target).ok()
    }
    fn view(&self) -> View {
        *self.view.lock().unwrap()
    }
    fn counts(&self) -> FollowerCounts {
        FollowerCounts::default()
    }
}

/// A linked clock on a ready service whose [`Tricky`] follower is unlocked:
/// the clock runs on the service.
fn tricky_on_service(slow_ns: u64, re_anchor_on_read: bool) -> (Arc<Tricky>, Linked) {
    let service = Arc::new(MonoClock::new());
    service.set_k(1_000_000_000_000);
    let f = Arc::new(Tricky {
        service: Arc::clone(&service),
        slow_ns,
        re_anchor_on_read,
        view: Mutex::new(HOLDOVER),
    });
    let linked = Linked::new(Some(Box::new(Arc::clone(&f))), service, None);
    assert_eq!(linked.tick(), Source::Service);
    (f, linked)
}

/// A slow read (a preempted selector) never passes for agreement: the
/// follower's time is the service's at the end of its 1 ms read, so it is
/// 1 ms off the service read before it and about 0 off the one read after
/// it. The farther difference counts: the switch waits. Read at once, the
/// same follower hands over seamlessly.
#[test]
fn a_slow_read_counts_against_agreement() {
    let (f, linked) = tricky_on_service(1_000_000, false);
    let e = linked.epoch();
    *f.view.lock().unwrap() = ALIVE_LOCKED;
    assert_eq!(linked.tick(), Source::Service, "waits: 1 ms apart");
    assert_eq!(linked.epoch(), e);

    let (f, linked) = tricky_on_service(0, false);
    let e = linked.epoch();
    *f.view.lock().unwrap() = ALIVE_LOCKED;
    assert_eq!(linked.tick(), Source::Internal);
    assert_eq!(linked.epoch(), e);
}

/// A follower that re-anchors while the switch reads it gives no offset:
/// a voluntary switch waits, never a seamless one over the jump.
#[test]
fn a_re_anchor_during_the_measure_is_no_agreement() {
    let (f, linked) = tricky_on_service(0, true);
    let e = linked.epoch();
    *f.view.lock().unwrap() = ALIVE_LOCKED;
    assert_eq!(linked.tick(), Source::Service, "no agreement: it waits");
    assert_eq!(linked.epoch(), e);
}

/// Switches on back-to-back rounds: after a wait ran out and the follower
/// took over, a forced switch back and a voluntary switch on the next
/// round waits again from zero (the count starts again at every switch).
#[test]
fn every_switch_starts_the_wait_count_again() {
    let (fake, service, linked) = linked_on_service();
    for _ in 0..HANDOVER_WAIT {
        set(&fake, service_now_plus(&service, 1_000_000), ALIVE_LOCKED);
        assert_eq!(linked.tick(), Source::Service);
    }
    set(&fake, service_now_plus(&service, 1_000_000), ALIVE_LOCKED);
    assert_eq!(linked.tick(), Source::Internal);
    set(&fake, service_now_plus(&service, 1_000_000), HOLDOVER);
    assert_eq!(linked.tick(), Source::Service, "forced back");
    set(&fake, service_now_plus(&service, 1_000_000), ALIVE_LOCKED);
    assert_eq!(linked.tick(), Source::Service, "waits again");
}

/// A wait that ends without a switch says so once.
#[test]
fn a_wait_that_ends_without_a_switch_logs_once() {
    crate::test_log::undecided();
    let (fake, service, linked) = linked_on_service();
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        set(&fake, service_now_plus(&service, 1_000_000), ALIVE_LOCKED);
        assert_eq!(linked.tick(), Source::Service);
        assert_eq!(linked.tick(), Source::Service);
        set(&fake, service_now_plus(&service, 1_000_000), UNSETTLED);
        for _ in 0..3 {
            assert_eq!(linked.tick(), Source::Service);
        }
    });
    assert_eq!(log.matches(WAIT_ENDED_LINE).count(), 1, "{log}");
}

/// The offset of a handover counts the gap between the reads against
/// agreement: the old source read before and after the new one, the
/// farther difference wins, whichever side it is on.
#[test]
fn the_offset_is_the_difference_farther_from_zero() {
    let t: i128 = 1_000_000_000_000;
    // Read back to back (no gap): the plain difference.
    assert_eq!(offset_of(t, t + 10_000, t), Some(10_000));
    // A 1 ms gap before the new read: 1 ms, not 0 or the midpoint.
    assert_eq!(offset_of(t, t + 1_000_000, t + 1_000_100), Some(1_000_000));
    // A gap after it: the read after counts.
    assert_eq!(offset_of(t, t + 100, t + 1_000_100), Some(-1_000_000));
    assert_eq!(offset_of(t + 20_000, t, t + 20_000), Some(-20_000));
    // Past i64: none.
    assert_eq!(offset_of(0, i128::from(i64::MAX) + 1, 0), None);
    assert_eq!(offset_of(0, i128::from(i64::MIN), 0), Some(i64::MIN));
}

/// iemmixer (5.11, v0.6.37, after a hard reset): the follower converged on
/// the service slowly (-61 µs, -43 µs, -31.75 µs) and the 60-round wait
/// ran out 1.75 µs outside the bound: a re-align, one pump re-bootstrap.
/// A wait that keeps converging keeps waiting; it re-aligns only after
/// [`HANDOVER_WAIT`] rounds without progress.
#[test]
fn a_converging_wait_keeps_waiting_and_hands_over_seamlessly() {
    // -61 µs closing 0.25 µs a round: 125 rounds to the bound.
    let (mut waited, mut best) = (0, u64::MAX);
    let mut offset = -61_000_i64;
    let way = loop {
        match handover(Some(offset), true, waited) {
            Handover::Wait => (waited, best) = wait_round(waited, best, Some(offset)),
            way => break way,
        }
        offset += 250;
    };
    assert_eq!(way, Handover::Seamless, "waited {waited}, offset {offset}");
    // A steady disagreement (strih, 5.11: 0.48 ms) still re-aligns after
    // HANDOVER_WAIT rounds without progress.
    let (mut waited, mut best) = (0, u64::MAX);
    let mut rounds = 0;
    let way = loop {
        match handover(Some(-480_000), true, waited) {
            Handover::Wait => (waited, best) = wait_round(waited, best, Some(-480_000)),
            way => break way,
        }
        rounds += 1;
    };
    assert_eq!((way, rounds), (Handover::Break, HANDOVER_WAIT));
    // Progress under 1 µs is no progress and leaves the mark where it
    // was (a creeping best would never be 1 µs behind); an unmeasured
    // round is none.
    assert_eq!(wait_round(5, 40_000, Some(-39_500)), (6, 40_000));
    assert_eq!(wait_round(5, 40_000, Some(39_000)), (1, 39_000));
    assert_eq!(wait_round(5, 40_000, None), (6, 40_000));
}
