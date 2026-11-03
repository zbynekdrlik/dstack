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
    // The follower locks: it wins, and its time is read.
    set(&fake, Some(42_000_000_000), ALIVE_LOCKED);
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
    // A negative PTP time from the follower reads as not ready.
    set(&fake, Some(-1), ALIVE_LOCKED);
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
    set(&fake, Some(42_000_000_000), ALIVE_LOCKED);
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
