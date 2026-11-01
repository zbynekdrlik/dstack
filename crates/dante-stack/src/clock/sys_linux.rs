//! The media clock's OS clocks on Linux: `CLOCK_REALTIME`, `CLOCK_MONOTONIC`
//! (dantesync steers its rate; no step moves it) and an absolute sleep on it.

fn read(clock: libc::clockid_t) -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable timespec; clock_gettime only
    // writes it, and a clock id it does not know leaves it zero.
    unsafe { libc::clock_gettime(clock, &mut ts) };
    ts.tv_sec * 1_000_000_000 + ts.tv_nsec
}

/// The box's `CLOCK_MONOTONIC`, ns.
pub fn monotonic_ns() -> i64 {
    read(libc::CLOCK_MONOTONIC)
}

/// A poll's wall reading: Linux's monotonic clock needs no model (the
/// kernel steers it and no step moves it), so this is the clock now.
pub fn observe(_wall: i64) -> i64 {
    monotonic_ns()
}

/// The box's `CLOCK_REALTIME`, ns since 1970.
pub fn realtime_ns() -> i64 {
    read(libc::CLOCK_REALTIME)
}

/// Sleeps until `CLOCK_MONOTONIC` reaches `target` ns (a past or negative
/// time returns at once: clock_nanosleep refuses it).
pub fn sleep_until_monotonic(target: i64) {
    let ts = libc::timespec {
        tv_sec: target / 1_000_000_000,
        tv_nsec: target % 1_000_000_000,
    };
    // SAFETY: `ts` is a valid timespec read by the call; the remaining-time
    // pointer may be null with TIMER_ABSTIME.
    unsafe {
        libc::clock_nanosleep(
            libc::CLOCK_MONOTONIC,
            libc::TIMER_ABSTIME,
            &ts,
            std::ptr::null_mut(),
        )
    };
}
