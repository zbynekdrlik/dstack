//! The media clock's OS clocks on Windows. dantesync steers the system
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
/// Waits up to this long are spun (yielding), never slept: on the FOH PC
/// the media threads, alone on their CPUs at priority 31, still woke past
/// 2.7 ms now and then while the CPUs themselves were never held over
/// 0.2 ms (`dasio jitter`): the waitable timer's late wakes. Longer
/// waits (the clock not ready) sleep on the timer, the last 50 ms spun.
const SPIN_NS: i64 = 50_000_000;

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

impl Drop for Timer {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle is this thread's timer, closed once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// Sleeps until [`monotonic_ns`] reaches `target`: the timer for all but
/// the last [`SPIN_NS`], then a spin.
pub fn sleep_until_monotonic(target: i64) {
    let left = target - monotonic_ns();
    // The timer mode (problem A's measurement): the whole wait on the
    // timer, no core spun.
    let spin = match super::wait_mode() {
        super::WaitMode::Spin => SPIN_NS,
        super::WaitMode::Timer => 0,
    };
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
            std::thread::sleep(std::time::Duration::from_nanos((left - spin) as u64));
        }
    }
    // The spin yields: the media threads share cores on Windows (the FOH
    // PC gives the device two), and a plain spin kept the RX thread off its
    // core (RX packets up to 48 ms late, 2026-10-29).
    if spin == 0 {
        return;
    }
    while monotonic_ns() < target {
        std::thread::yield_now();
    }
}
