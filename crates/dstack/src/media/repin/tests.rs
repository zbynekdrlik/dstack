// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;

/// The counters are the process's: tests that count them take turns.
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
fn the_previous_mask_tells_kept_reset_and_refused() {
    let mask = 1 << 3;
    assert_eq!(outcome(mask, mask), PinOutcome::Kept);
    assert_eq!(outcome(0, mask), PinOutcome::Refused, "0 is a refusal");
    assert_eq!(outcome(0xff, mask), PinOutcome::Reset, "the process mask");
    assert_eq!(outcome(1, mask), PinOutcome::Reset, "another CPU");
}

#[test]
fn a_one_cpu_mask_covers_the_first_processor_group() {
    assert_eq!(one_cpu_mask(0), Some(1));
    assert_eq!(one_cpu_mask(5), Some(0b10_0000));
    assert_eq!(one_cpu_mask(63), Some(1 << 63));
    assert_eq!(one_cpu_mask(64), None);
    assert_eq!(one_cpu_mask(crate::device::MAX_CPU), None);
}

#[test]
fn keep_moves_the_schedule_and_off_windows_counts_nothing() {
    let _turn = counter();
    let (resets, refusals) = (pin_resets(), pin_refusals());
    let mut pin = RePin::new(Some(0));
    pin.keep(0);
    assert_eq!(pin.next_ns, REPIN_NS, "the schedule moved");
    #[cfg(not(windows))]
    assert_eq!((pin_resets(), pin_refusals()), (resets, refusals));
    #[cfg(windows)]
    let _ = (resets, refusals);
}

#[test]
fn each_outcome_is_counted_in_its_own_counter() {
    let _turn = counter();
    let (resets, refusals) = (pin_resets(), pin_refusals());
    note(PinOutcome::Kept);
    assert_eq!((pin_resets(), pin_refusals()), (resets, refusals));
    note(PinOutcome::Reset);
    note(PinOutcome::Reset);
    assert_eq!((pin_resets(), pin_refusals()), (resets + 2, refusals));
    note(PinOutcome::Refused);
    note(PinOutcome::Refused);
    note(PinOutcome::Refused);
    assert_eq!((pin_resets(), pin_refusals()), (resets + 2, refusals + 3));
}

/// The OS's reset, on one thread: Windows sets each thread's affinity to
/// the process mask when a program sets the process's; the test widens
/// its own thread the same way (the process's mask itself is left alone,
/// so no other test's thread moves).
#[cfg(windows)]
#[test]
fn on_windows_a_re_pin_finds_and_undoes_a_widened_affinity() {
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentThread, GetProcessAffinityMask, SetThreadAffinityMask,
    };
    std::thread::spawn(|| {
        let (mut process, mut system) = (0usize, 0usize);
        // SAFETY: the pseudo handle of this process and two valid outputs.
        let read =
            unsafe { GetProcessAffinityMask(GetCurrentProcess(), &mut process, &mut system) };
        assert_ne!(read, 0, "the process mask reads");
        assert!(process.count_ones() > 1, "every runner has two CPUs");
        let cpu = process.trailing_zeros() as usize;
        assert_eq!(repin(cpu), PinOutcome::Reset, "the thread ran anywhere");
        assert_eq!(repin(cpu), PinOutcome::Kept, "pinned now");
        // SAFETY: the pseudo handle of this thread and the process's mask.
        let pinned = unsafe { SetThreadAffinityMask(GetCurrentThread(), process) };
        assert_eq!(pinned, 1 << cpu, "the re-pin held");
        assert_eq!(repin(cpu), PinOutcome::Reset, "widened like a reset");
        assert_eq!(repin(64), PinOutcome::Refused, "past the first group");
    })
    .join()
    .unwrap();
}

#[test]
fn the_watch_tells_the_first_reset_once() {
    crate::test_log::undecided();
    let mut watch = PinWatch::new();
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        watch.watch(0, 0);
        watch.watch(2, 0);
        watch.watch(3, 0);
        watch.watch(5, 0);
    });
    let lines: Vec<&str> = log.lines().filter(|l| l.contains(RESET_LINE)).collect();
    assert_eq!(lines.len(), 1, "{log}");
    assert!(lines[0].contains("resets=2"), "{log}");
    assert!(!log.contains(REFUSED_LINE), "{log}");
}

#[test]
fn the_watch_warns_the_first_refusal_once() {
    crate::test_log::undecided();
    let mut watch = PinWatch::new();
    let log = crate::test_log::capture_warnings(|| {
        watch.watch(0, 0);
        watch.watch(0, 1);
        watch.watch(0, 4);
        watch.watch(0, 9);
    });
    let lines: Vec<&str> = log.lines().filter(|l| l.contains(REFUSED_LINE)).collect();
    assert_eq!(lines.len(), 1, "{log}");
    assert!(lines[0].contains("refusals=1"), "{log}");
}

#[test]
fn the_watch_tells_nothing_without_a_reset_or_a_refusal() {
    crate::test_log::undecided();
    let mut watch = PinWatch::new();
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        watch.watch(0, 0);
        watch.watch(0, 0);
    });
    assert!(!log.contains(RESET_LINE), "{log}");
    assert!(!log.contains(REFUSED_LINE), "{log}");
}

/// dstack #1 (0.6.35): a re-pin that holds marks the thread pinned (its
/// spin then never yields on Windows); off Windows the pin always holds.
#[test]
fn a_re_pin_that_holds_marks_the_thread_pinned() {
    std::thread::spawn(|| {
        assert!(!crate::clock::pinned());
        let mut pin = RePin::new(Some(0));
        pin.keep(0);
        assert!(crate::clock::pinned(), "the pin held: the thread is pinned");
    })
    .join()
    .unwrap();
}
