// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;

/// The reset counter is the process's: tests that count it take turns.
static COUNTER: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn counter() -> std::sync::MutexGuard<'static, ()> {
    COUNTER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[test]
fn a_pin_is_due_at_the_start_not_before_a_second_and_again_after_one() {
    let mut pin = RePin::new(Some(3));
    assert!(pin.due(5), "due at the first look");
    assert!(!pin.due(5), "not again at once");
    assert!(!pin.due(5 + REPIN_NS - 1), "not before a second");
    assert!(pin.due(5 + REPIN_NS), "due a second after the last");
    assert!(!pin.due(5 + REPIN_NS + 1));
    assert!(
        pin.due(5 + 3 * REPIN_NS),
        "a late look is due, and counts from itself"
    );
    assert!(!pin.due(5 + 4 * REPIN_NS - 1));
    assert!(pin.due(5 + 4 * REPIN_NS));
}

#[test]
fn a_thread_without_a_cpu_is_never_due() {
    let mut pin = RePin::new(None);
    for now in [0, 1, REPIN_NS, 10 * REPIN_NS, u64::MAX] {
        assert!(!pin.due(now), "{now}");
    }
}

#[test]
fn the_schedule_does_not_overflow_at_the_clock_s_end() {
    let mut pin = RePin::new(Some(0));
    assert!(pin.due(u64::MAX - 1));
    assert_eq!(pin.next_ns, u64::MAX);
}

#[test]
fn keep_off_windows_re_applies_nothing_and_counts_no_reset() {
    let _turn = counter();
    let before = pin_resets();
    let mut pin = RePin::new(Some(0));
    pin.keep(0);
    assert_eq!(pin.next_ns, REPIN_NS, "the schedule moved");
    #[cfg(not(windows))]
    assert_eq!(pin_resets(), before);
    #[cfg(windows)]
    let _ = before;
}

#[test]
fn a_reset_is_counted() {
    let _turn = counter();
    let before = pin_resets();
    note_reset();
    note_reset();
    assert_eq!(pin_resets(), before + 2);
}

#[test]
fn the_watch_tells_the_first_reset_once() {
    crate::test_log::undecided();
    let mut watch = PinWatch::new();
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        watch.watch(0);
        watch.watch(2);
        watch.watch(3);
        watch.watch(5);
    });
    let lines: Vec<&str> = log.lines().filter(|l| l.contains(RESET_LINE)).collect();
    assert_eq!(lines.len(), 1, "{log}");
    assert!(lines[0].contains("resets=2"), "{log}");
}

#[test]
fn the_watch_tells_nothing_without_a_reset() {
    crate::test_log::undecided();
    let mut watch = PinWatch::new();
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        watch.watch(0);
        watch.watch(0);
    });
    assert!(!log.contains(RESET_LINE), "{log}");
}
