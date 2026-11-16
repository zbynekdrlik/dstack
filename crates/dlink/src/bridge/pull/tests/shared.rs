// SPDX-License-Identifier: MIT OR Apache-2.0

//! [`SharedPull`]: the callback's pull never waits, a poisoned lock still
//! pulls, and the watching thread drains only while nothing pulls.

use super::*;

/// The rig's pull shared, with its FIFO and clock.
fn shared() -> (SharedPull, Rig) {
    let mut r = rig(0);
    let pull = std::mem::replace(&mut r.pull, rig(0).pull);
    (SharedPull::new(pull), r)
}

#[test]
fn a_shared_pull_states_its_fifo_channels_and_counts() {
    let (s, r) = shared();
    assert!(Arc::ptr_eq(s.fifo(), &r.fifo));
    assert_eq!(s.channels(), 2);
    assert_eq!(s.pull(), 0, "the start");
    assert_eq!(s.stats().pulls(), 1);
    r.clock.sleep_until(ns_of(T + P + 10, RATE));
    assert_eq!(s.pull(), 10);
    assert_eq!(r.fifo.len(), 10);
}

#[test]
fn a_held_lock_pulls_nothing_and_never_waits() {
    let (s, r) = shared();
    assert_eq!(s.pull(), 0);
    r.clock.sleep_until(ns_of(T + P + 10, RATE));
    {
        let _held = s.pull.lock().unwrap();
        assert_eq!(s.pull(), 0, "held elsewhere");
    }
    assert_eq!(s.stats().pulls(), 1, "no pull while held");
    assert_eq!(s.pull(), 10, "the next one reads the gap");
}

#[test]
fn a_poisoned_lock_still_pulls_and_its_poison_is_reported_once() {
    let (s, r) = shared();
    let s = Arc::new(s);
    assert_eq!(s.pull(), 0);
    let held = Arc::clone(&s);
    let _ = std::thread::spawn(move || {
        let _guard = held.pull.lock().unwrap();
        panic!("a pull panicked");
    })
    .join();
    r.clock.sleep_until(ns_of(T + P + 10, RATE));
    assert_eq!(s.pull(), 10, "poisoned: it pulls on");
    assert!(s.clear_poison());
    assert!(!s.clear_poison(), "cleared");
    assert_eq!(s.pull(), 0);
}

#[test]
fn the_watch_drains_only_while_nothing_pulls_and_never_waits() {
    let (s, r) = shared();
    let mut watch = IdleWatch::default();
    let look = s.keep(&mut watch);
    assert!(look.idle && look.changed, "{look:?}");
    assert_eq!(s.stats().idle_drains(), 1);
    assert_eq!(s.pull(), 0, "afresh after the drain");
    r.clock.sleep_until(ns_of(T + P + 10, RATE));
    assert_eq!(s.pull(), 10);
    let look = s.keep(&mut watch);
    assert!(!look.idle && look.changed, "{look:?}");
    assert_eq!(s.stats().idle_drains(), 1, "it pulled: no drain");
    {
        let _held = s.pull.lock().unwrap();
        let look = s.keep(&mut watch);
        assert!(look.idle && look.changed, "{look:?}");
    }
    assert_eq!(s.stats().idle_drains(), 1, "held: never waited for");
    let look = s.keep(&mut watch);
    assert!(look.idle && !look.changed, "{look:?}");
    assert_eq!(s.stats().idle_drains(), 2);
}
