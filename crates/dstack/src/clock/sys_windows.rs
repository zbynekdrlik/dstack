//! The media clock's OS clocks on Windows. dsync steers the system
//! time's rate (`SetSystemTimeAdjustmentPrecise`) and steps it at a date
//! step (`NtSetSystemTime`); QPC is the raw counter and is not steered. The
//! wall is the precise system time (`GetSystemTimePreciseAsFileTime`). The
//! "monotonic" clock is QPC scaled by [`super::steer::Model`], which the
//! clock poller updates from each poll's (QPC, wall) pair ([`observe`]): it
//! runs at the wall's steered rate and a date step does not move it, as
//! Linux's `CLOCK_MONOTONIC` (dstack #1: before, a date step moved PTP time
//! for 4 s, the TX schedule and the engine pump started again).

use std::sync::{LazyLock, Mutex, OnceLock, PoisonError};

use super::steer::{Model, Steer, Update};
use crate::media::tx::Cell;

use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows_sys::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
use windows_sys::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, INFINITE, SetWaitableTimer,
    TIMER_ALL_ACCESS, WaitForSingleObject,
};

/// 100 ns intervals from 1601-01-01 to 1970-01-01.
const UNIX_EPOCH_FT: i64 = 116_444_736_000_000_000;
// The spin tail (`super::SPIN_TAIL`, the device's `spin_tail_ns`,
// 50 ms unless set): waits up to it are spun, never slept. The
// high-resolution timer wakes on the system tick: on the FOH PC (5.11.2026)
// at most one tick + 140 µs late (1.1 ms at the 1 ms resolution, 0.64 ms at
// 0.5 ms); in October 2026, with the Realtek driver's 20-30 ms DPC, up to
// 2.7 ms. A shorter tail is for a PC where the timer wakes on time.

/// The precise system time, ns since 1970.
pub fn realtime_ns() -> i64 {
    let mut ft = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    // SAFETY: `ft` is a valid, writable FILETIME.
    unsafe { GetSystemTimePreciseAsFileTime(&mut ft) };
    let t = (i64::from(ft.dwHighDateTime) << 32) | i64::from(ft.dwLowDateTime);
    (t - UNIX_EPOCH_FT) * 100
}

/// QPC's frequency, ticks a second (fixed at boot).
fn qpc_frequency() -> i64 {
    static FREQ: OnceLock<i64> = OnceLock::new();
    *FREQ.get_or_init(|| {
        let mut f = 0i64;
        // SAFETY: a valid, writable i64.
        unsafe { QueryPerformanceFrequency(&mut f) };
        f.max(1)
    })
}

/// QPC as ns since boot.
fn qpc_ns() -> i64 {
    let mut t = 0i64;
    // SAFETY: a valid, writable i64.
    unsafe { QueryPerformanceCounter(&mut t) };
    (i128::from(t) * 1_000_000_000 / i128::from(qpc_frequency())) as i64
}

/// The steered clock's model: written by [`observe`] (under [`STEER`]),
/// read by every thread without a lock.
static MODEL: LazyLock<Cell<3>> = LazyLock::new(Cell::new);
/// The model's learning: one writer at a time (a poller per process, but
/// the tests run several).
static STEER: Mutex<Steer> = Mutex::new(Steer::new());

/// The steered clock, ns: QPC through the model; the wall until the first
/// poll.
pub fn monotonic_ns() -> i64 {
    if MODEL.version() == 0 {
        return realtime_ns();
    }
    let w = MODEL.read().map_or_else(|| MODEL.peek(), |(_, w)| w);
    Model::from_words(w).at(qpc_ns())
}

/// A poll's wall reading `wall`, taken just before: updates the steered
/// clock's model and returns the clock now. A date step is logged; the
/// clock does not move.
pub fn observe(wall: i64) -> i64 {
    let q = qpc_ns();
    let mut st = STEER.lock().unwrap_or_else(PoisonError::into_inner);
    let update = st.update(q, wall);
    let Some(model) = st.model() else {
        return wall;
    };
    MODEL.write(model.words());
    if let Update::Step { jump_ns } = update {
        tracing::info!(
            jump_ns,
            offset_ns = st.offset(),
            "the system time stepped (a date step): the media clock does not move"
        );
    }
    model.at(q)
}

/// This thread's high-resolution waitable timer (made once per thread).
fn timer() -> HANDLE {
    thread_local! {
        static TIMER: OnceLock<Timer> = const { OnceLock::new() };
    }
    TIMER.with(|t| {
        t.get_or_init(|| {
            // SAFETY: no name, no security attributes; the handle is closed on
            // the thread's end.
            let h = unsafe {
                CreateWaitableTimerExW(
                    std::ptr::null(),
                    std::ptr::null(),
                    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                    TIMER_ALL_ACCESS,
                )
            };
            Timer(h)
        })
        .0
    })
}

struct Timer(HANDLE);

/// Once per thread: without the high-resolution timer a wait ends on the
/// system tick (about 15.6 ms), which a receiver sees as late packets.
fn no_timer_warning() {
    thread_local! {
        static WARNED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if !WARNED.replace(true) {
        tracing::warn!(
            error = %std::io::Error::last_os_error(),
            "no high-resolution waitable timer for this media thread: its waits end on the system tick"
        );
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle is this thread's timer, closed once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// Sleeps until [`monotonic_ns`] reaches `target`: the timer for all but
/// the last spin tail (`super::SPIN_TAIL`), then a spin.
pub fn sleep_until_monotonic(target: i64) {
    sleep_until_in(target, super::wait_mode());
}

/// [`sleep_until_monotonic`] in the wait `mode`.
fn sleep_until_in(target: i64, mode: super::WaitMode) {
    let left = target - monotonic_ns();
    // The timer mode (problem A's measurement): the whole wait on the
    // timer, no core spun.
    let tail = super::SPIN_TAIL.load(std::sync::atomic::Ordering::Relaxed);
    let spin = i64::try_from(super::spin_tail_for(mode, tail)).unwrap_or(i64::MAX);
    if left > spin {
        let h = timer();
        if !h.is_null() {
            // Relative due time: negative, in 100 ns units.
            let due = -((left - spin) / 100);
            // SAFETY: a valid timer handle, a valid due time, no callback.
            let set = unsafe { SetWaitableTimer(h, &due, 0, None, std::ptr::null(), 0) };
            if set != 0 {
                // SAFETY: a valid handle.
                unsafe { WaitForSingleObject(h, INFINITE) };
            }
        } else {
            no_timer_warning();
            std::thread::sleep(std::time::Duration::from_nanos((left - spin) as u64));
        }
    }
    // The spin of a thread pinned to a CPU of its own never yields (dstack
    // #1, dasio's CSwitch trace on the FOH PC, 5.11.2026): SwitchToThread
    // (`std::thread::yield_now`) handed the band TX thread's CPU (priority
    // 31) to a priority-10 thread, which kept it for the rest of its slice,
    // 1.66 ms, and the TX schedule started again. A timer expiry preempts a
    // lower priority at once; a yield does not. Any other thread yields:
    // without a CPU of its own a spin that never yields starves what shares
    // it (0.6.34 on dasio's runner: two unpinned devices, the pump overran
    // hundreds of blocks a second). Sleep(0) (v0.6.28) is no answer either:
    // it yields only to equal or higher priority, the spin never blocked,
    // and two Windows PCs stopped answering (dasio, 4.11.2026). A pinned
    // thread whose deadlines come closer than the tail (TX at 8 frames a
    // packet: every 83 µs) spins all the time, so the other CPU of its core
    // must run threads that block (`docs/windows-box.md`).
    if spin == 0 {
        return;
    }
    let pinned = super::pinned();
    while monotonic_ns() < target {
        if pinned {
            std::hint::spin_loop();
        } else {
            std::thread::yield_now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// dlink (#68): the normal profile's waits end on the high-resolution
    /// timer, not on the 15.6 ms system tick (SongPlayer 31.10: peaks of two
    /// ticks). The median of 200 waits of 1 ms oversleeps under 1 ms.
    #[test]
    fn a_timer_wait_of_1_ms_ends_within_1_ms_at_the_median() {
        let mut late: Vec<i64> = (0..200)
            .map(|_| {
                let target = monotonic_ns() + 1_000_000;
                sleep_until_in(target, super::super::WaitMode::Timer);
                monotonic_ns() - target
            })
            .collect();
        late.sort_unstable();
        let median = late[late.len() / 2];
        assert!(
            (0..1_000_000).contains(&median),
            "median oversleep {median} ns; p90 {} ns, max {} ns",
            late[late.len() * 9 / 10],
            late[late.len() - 1]
        );
    }

    /// dstack #1 (dasio's CSwitch trace, FOH 5.11.2026): the spin tail's
    /// SwitchToThread gave the band TX thread's CPU (priority 31) to a
    /// priority-10 thread for 1.66 ms, and the TX schedule started again.
    /// A pinned thread's spun wait never hands its CPU to a lower priority:
    /// with a lower-priority thread busy on the same CPU, 100 spun waits of 300 µs
    /// end within 100 µs at the median (a yield costs the other thread's
    /// whole slice, many ms).
    #[test]
    fn a_spun_wait_never_yields_its_cpu_to_a_lower_priority_thread() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadAffinityMask, SetThreadPriority,
        };
        /// THREAD_PRIORITY_TIME_CRITICAL and THREAD_PRIORITY_LOWEST.
        const TIME_CRITICAL: i32 = 15;
        const LOWEST: i32 = -2;
        /// Both threads on CPU 0 only.
        const CPU0: usize = 1;
        let pin = |priority: i32| {
            // SAFETY: the calling thread's pseudo handle.
            unsafe {
                assert_ne!(SetThreadAffinityMask(GetCurrentThread(), CPU0), 0);
                assert_ne!(SetThreadPriority(GetCurrentThread(), priority), 0);
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        let running = Arc::new(AtomicBool::new(false));
        let busy = {
            let (stop, running) = (Arc::clone(&stop), Arc::clone(&running));
            std::thread::spawn(move || {
                pin(LOWEST);
                running.store(true, Ordering::Release);
                while !stop.load(Ordering::Relaxed) {
                    std::hint::spin_loop();
                }
            })
        };
        while !running.load(Ordering::Acquire) {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        pin(TIME_CRITICAL);
        // The stack's pin: a CPU of its own (`media::tx::place`).
        super::super::set_pinned(true);
        let mut late: Vec<i64> = (0..100)
            .map(|_| {
                let target = monotonic_ns() + 300_000;
                sleep_until_in(target, super::super::WaitMode::Spin);
                monotonic_ns() - target
            })
            .collect();
        stop.store(true, Ordering::Relaxed);
        busy.join().unwrap();
        late.sort_unstable();
        let median = late[late.len() / 2];
        assert!(
            median < 100_000,
            "median late {median} ns; p90 {} ns, max {} ns",
            late[late.len() * 9 / 10],
            late[late.len() - 1]
        );
    }
    /// dasio's Windows e2e (v0.6.34, two devices on one runner, unpinned):
    /// spinners that never yield starved everything else on the box (the
    /// pump overran hundreds of blocks a second and re-bootstrapped). A
    /// thread without a CPU of its own still yields in its spin: a
    /// lower-priority thread on its CPU makes progress while it waits.
    #[test]
    fn an_unpinned_spun_wait_lets_a_lower_priority_thread_on_its_cpu_run() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadAffinityMask, SetThreadPriority,
        };
        /// THREAD_PRIORITY_TIME_CRITICAL and THREAD_PRIORITY_LOWEST.
        const TIME_CRITICAL: i32 = 15;
        const LOWEST: i32 = -2;
        /// Both threads on CPU 0 only, set behind the stack's back: the
        /// stack did not pin the waiting thread.
        const CPU0: usize = 1;
        let pin = |priority: i32| {
            // SAFETY: the calling thread's pseudo handle.
            unsafe {
                assert_ne!(SetThreadAffinityMask(GetCurrentThread(), CPU0), 0);
                assert_ne!(SetThreadPriority(GetCurrentThread(), priority), 0);
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        let turns = Arc::new(AtomicU64::new(0));
        let busy = {
            let (stop, turns) = (Arc::clone(&stop), Arc::clone(&turns));
            std::thread::spawn(move || {
                pin(LOWEST);
                while !stop.load(Ordering::Relaxed) {
                    turns.fetch_add(1, Ordering::Relaxed);
                }
            })
        };
        while turns.load(Ordering::Relaxed) == 0 {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        pin(TIME_CRITICAL);
        let before = turns.load(Ordering::Relaxed);
        for _ in 0..100 {
            sleep_until_in(monotonic_ns() + 300_000, super::super::WaitMode::Spin);
        }
        let ran = turns.load(Ordering::Relaxed) - before;
        stop.store(true, Ordering::Relaxed);
        busy.join().unwrap();
        assert!(
            ran > 0,
            "the lower-priority thread never ran during 100 spun waits"
        );
    }
    /// FOH froze (5.11.2026, ~04:48Z) right after dasio rolled v0.6.35:
    /// its pinned TX and engine threads spun without yielding, and some
    /// per-CPU system work that must run on exactly those CPUs never ran
    /// (the same freeze as v0.6.28's Sleep(0)). A pinned thread's spun
    /// wait also lets a lower-priority thread on its CPU run.
    #[test]
    fn a_pinned_spun_wait_lets_a_lower_priority_thread_on_its_cpu_run() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadAffinityMask, SetThreadPriority,
        };
        /// THREAD_PRIORITY_TIME_CRITICAL and THREAD_PRIORITY_LOWEST.
        const TIME_CRITICAL: i32 = 15;
        const LOWEST: i32 = -2;
        /// Both threads on CPU 0 only.
        const CPU0: usize = 1;
        let pin = |priority: i32| {
            // SAFETY: the calling thread's pseudo handle.
            unsafe {
                assert_ne!(SetThreadAffinityMask(GetCurrentThread(), CPU0), 0);
                assert_ne!(SetThreadPriority(GetCurrentThread(), priority), 0);
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        let turns = Arc::new(AtomicU64::new(0));
        let busy = {
            let (stop, turns) = (Arc::clone(&stop), Arc::clone(&turns));
            std::thread::spawn(move || {
                pin(LOWEST);
                while !stop.load(Ordering::Relaxed) {
                    turns.fetch_add(1, Ordering::Relaxed);
                }
            })
        };
        while turns.load(Ordering::Relaxed) == 0 {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        pin(TIME_CRITICAL);
        // As the stack's own pin does (`media::tx::place`).
        super::super::set_pinned(true);
        let before = turns.load(Ordering::Relaxed);
        for _ in 0..100 {
            sleep_until_in(monotonic_ns() + 300_000, super::super::WaitMode::Spin);
        }
        let ran = turns.load(Ordering::Relaxed) - before;
        stop.store(true, Ordering::Relaxed);
        busy.join().unwrap();
        assert!(
            ran > 0,
            "the lower-priority thread never ran during 100 spun waits of a pinned thread"
        );
    }
}
