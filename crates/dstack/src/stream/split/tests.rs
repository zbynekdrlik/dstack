use super::*;

#[test]
fn a_peak_keeps_the_largest_since_the_start_and_since_the_last_take() {
    let peak = Peak::default();
    assert_eq!((peak.max(), peak.take_second()), (0, 0));
    peak.record(700);
    peak.record(300);
    assert_eq!(peak.max(), 700);
    assert_eq!(peak.take_second(), 700);
    // The second starts afresh; the start's peak stays.
    assert_eq!(peak.take_second(), 0);
    peak.record(400);
    assert_eq!((peak.max(), peak.take_second()), (700, 400));
    peak.record(900);
    assert_eq!((peak.max(), peak.take_second()), (900, 900));
}

#[test]
fn an_oversleep_is_the_wake_past_the_target_of_a_sleep_asked_before_it() {
    // Asked 1 ms before its target, woke 8.5 ms past it (obs-ci #354).
    assert_eq!(
        oversleep_ns(10_000_000, 9_000_000, 18_500_000),
        Some(8_500_000)
    );
    // On time, and a read that moved back before the target: 0.
    assert_eq!(oversleep_ns(10_000_000, 9_000_000, 10_000_000), Some(0));
    assert_eq!(oversleep_ns(10_000_000, 9_000_000, 9_999_000), Some(0));
    // Asked one ns before the target: a sleep.
    assert_eq!(oversleep_ns(10_000_000, 9_999_999, 10_000_007), Some(7));
    // Asked at or after the target (the work ran into it): no sleep.
    assert_eq!(oversleep_ns(10_000_000, 10_000_000, 18_000_000), None);
    assert_eq!(oversleep_ns(10_000_000, 12_000_000, 18_000_000), None);
}

#[test]
fn the_cpu_between_two_reads_needs_both() {
    assert_eq!(cpu_between(Some(1_000), Some(81_000)), Some(80_000));
    assert_eq!(cpu_between(Some(5), Some(5)), Some(0));
    assert_eq!(cpu_between(None, Some(81_000)), None);
    assert_eq!(cpu_between(Some(1_000), None), None);
    // A read that went back is 0, never a wrap.
    assert_eq!(cpu_between(Some(9), Some(7)), Some(0));
}

#[test]
fn a_timespec_is_its_seconds_and_nanoseconds() {
    assert_eq!(timespec_ns(0, 3, 250), Some(3_000_000_250));
    assert_eq!(timespec_ns(0, 0, 999_999_999), Some(999_999_999));
    assert_eq!(timespec_ns(-1, 3, 250), None);
    assert_eq!(timespec_ns(0, -1, 250), None);
    assert_eq!(timespec_ns(0, 3, -1), None);
    // A read past u64's ns (585 years past 1970) is none, never a wrap.
    assert_eq!(timespec_ns(0, 18_446_744_074, 0), None);
    assert_eq!(timespec_ns(0, 18_446_744_073, 709_551_615), Some(u64::MAX));
    assert_eq!(timespec_ns(0, 18_446_744_073, 709_551_616), None);
}

#[cfg(target_os = "linux")]
#[test]
fn the_thread_cpu_time_counts_this_thread_s_work_and_not_its_sleep() {
    // A fresh thread: its CPU time starts near 0 (well under a second).
    let (busy, slept) = std::thread::spawn(|| {
        let start = thread_cpu_ns().expect("Linux reads the thread's CPU time");
        assert!(start < 1_000_000_000, "{start}");
        let wall = std::time::Instant::now();
        let mut x = 0u64;
        while wall.elapsed() < std::time::Duration::from_millis(20) {
            x = std::hint::black_box(x.wrapping_add(1));
        }
        let busy = cpu_between(Some(start), thread_cpu_ns()).unwrap();
        let before = thread_cpu_ns();
        std::thread::sleep(std::time::Duration::from_millis(20));
        (busy, cpu_between(before, thread_cpu_ns()).unwrap())
    })
    .join()
    .unwrap();
    // Spinning 20 ms of wall time is CPU time (some of it, on a loaded
    // box); sleeping 20 ms is almost none.
    assert!(busy >= 1_000_000, "busy {busy}");
    assert!(slept < 2_000_000, "slept {slept}");
}
