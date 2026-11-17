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
// The spin tail (`super::spin::SPIN_TAIL`, the device's `spin_tail_ns`,
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

/// QPC's frequency, ticks a second (fixed at boot). The RX receive stamps
/// are scaled with it (dstack #381).
pub(crate) fn qpc_frequency() -> i64 {
    static FREQ: OnceLock<i64> = OnceLock::new();
    *FREQ.get_or_init(|| {
        let mut f = 0i64;
        // SAFETY: a valid, writable i64.
        unsafe { QueryPerformanceFrequency(&mut f) };
        f.max(1)
    })
}

/// QPC now, ticks (an RX receive stamp's unit, dstack #381).
pub(crate) fn qpc_ticks() -> i64 {
    let mut t = 0i64;
    // SAFETY: a valid, writable i64.
    unsafe { QueryPerformanceCounter(&mut t) };
    t
}

/// QPC as ns since boot.
fn qpc_ns() -> i64 {
    super::qpc_ns_of(qpc_ticks(), qpc_frequency())
}

/// The app's clock (`clock::app_monotonic_ns`, dstack #295): raw QPC in
/// ns, never through the steer model, as OBS's `os_gettime_ns` reads it.
pub fn app_monotonic_ns() -> i64 {
    qpc_ns()
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
/// the last spin tail (`super::spin::SPIN_TAIL`), then a spin.
pub fn sleep_until_monotonic(target: i64) {
    sleep_until_in(
        target,
        monotonic_ns,
        super::wait_mode(),
        escape_period(),
        super::no_yield_tail_on(),
    );
}

/// Sleeps until the app's clock ([`app_monotonic_ns`], raw QPC) reaches
/// `target`, the same wait on it (dstack #328: the test clock's deadline is
/// on the app's clock, and the steered clock is the wall until a poll).
pub fn sleep_until_app(target: i64) {
    sleep_until_in(
        target,
        app_monotonic_ns,
        super::wait_mode(),
        escape_period(),
        super::no_yield_tail_on(),
    );
}

/// The process's bounded-escape period, ns (0: off; `super::spin::SPIN_ESCAPE`).
fn escape_period() -> u64 {
    super::spin::SPIN_ESCAPE.load(std::sync::atomic::Ordering::Relaxed)
}

thread_local! {
    /// Whether this thread's waits make the bounded escape ([`allow_escape`]).
    static ALLOWED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Lets this thread's low-latency waits make the bounded escape (dstack
/// #1, `spin_escape_ms`): the TX thread (its relay covers the block) and an
/// engine pump whose margin covers the block ([`super::pump_may_escape`]).
/// Any other thread spins yielding, as without the escape.
pub fn allow_escape(allowed: bool) {
    ALLOWED.with(|a| a.set(allowed));
}

thread_local! {
    /// Whether this thread's capped give is a timer wait ([`give_on_timer`]).
    static TIMER_GIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Makes this thread's capped give (dstack #351, `wait::GIVE_CAP_NS`) a
/// timer wait instead of a yield: an engine pump whose W absorbs the
/// timer's late wake ([`super::pump_gives_on_timer`]). Any other thread
/// yields. Read only with the device's `spin_no_yield_tail` on: off, no
/// wait has a capped give.
pub fn give_on_timer(on: bool) {
    TIMER_GIVE.with(|t| t.set(on));
}

thread_local! {
    /// This thread's bounded escape (dstack #1).
    static ESCAPE: std::cell::Cell<super::EscapeState> =
        const { std::cell::Cell::new(super::EscapeState::new()) };
}

/// Whether this thread's next low-latency wait makes a bounded escape
/// (dstack #1, `spin_escape_ms`): TX asks after its pass and hands its
/// passes to its relay thread first (dstack #141). From its first ask the
/// thread's waits escape only when an ask said so
/// ([`super::EscapeState::ask`]).
pub fn ask_escape() -> bool {
    let period = escape_period();
    if period == 0 || super::wait_mode() != super::WaitMode::Spin {
        return false;
    }
    ESCAPE.with(|e| {
        let mut s = e.get();
        let go = s.ask(app_monotonic_ns(), period);
        e.set(s);
        go
    })
}

/// Puts off this thread's armed escape: TX's relay did not take over, or
/// the clock re-anchored ([`super::EscapeState::defer`]); `false` when the escape still
/// happens (nothing armed, or already overdue).
pub fn defer_escape() -> bool {
    let period = escape_period();
    ESCAPE.with(|e| {
        let mut s = e.get();
        let put_off = s.defer(app_monotonic_ns(), period);
        e.set(s);
        put_off
    })
}

/// Blocks this thread's armed bounded escape now (dstack #141): TX handed
/// its passes to its relay thread first. `false`: nothing armed.
pub fn block_escape() -> bool {
    let period = escape_period();
    let go = ESCAPE.with(|e| {
        let mut s = e.get();
        let go = s.take(app_monotonic_ns(), period);
        e.set(s);
        go
    });
    if go {
        block_for(super::ESCAPE_BLOCK_NS as i64);
        // Bookkeeping only: TX's waits under the escape pause uncapped; a
        // later wait without the escape counts from this block.
        GIVES.with(|g| {
            let mut gives = g.get();
            gives.gave(app_monotonic_ns());
            g.set(gives);
        });
    }
    go
}

/// Makes this thread's low-latency waits spin without yielding and never
/// escape (TX's relay thread, dstack #141): it spins only while it covers
/// the TX thread's block, at most once a period.
pub fn relay_spin() {
    ALLOWED.with(|a| a.set(true));
    ESCAPE.with(|e| e.set(super::EscapeState::relay()));
}

/// Blocks the calling thread on its high-resolution timer for `ns`
/// (relative), no spin; `std::thread::sleep` without the timer.
fn block_for(ns: i64) {
    let h = timer();
    if !h.is_null() {
        // Relative due time: negative, in 100 ns units, at least one (0
        // would be an absolute time in the past: no block at all).
        let due = -(ns / 100).max(1);
        // SAFETY: a valid timer handle, a valid due time, no callback.
        let set = unsafe { SetWaitableTimer(h, &due, 0, None, std::ptr::null(), 0) };
        if set != 0 {
            // SAFETY: a valid handle.
            unsafe { WaitForSingleObject(h, INFINITE) };
        }
    } else {
        no_timer_warning();
        std::thread::sleep(std::time::Duration::from_nanos(ns as u64));
    }
}

/// The wait's OS on the clock `now` (raw QPC or the steered clock).
struct Os {
    now: fn() -> i64,
    escape_ns: u64,
}

impl super::wait::WaitOs for Os {
    fn now_ns(&mut self) -> i64 {
        (self.now)()
    }

    fn app_ns(&mut self) -> i64 {
        app_monotonic_ns()
    }

    fn take_escape(&mut self) -> bool {
        let escape_ns = self.escape_ns;
        ESCAPE.with(|e| {
            let mut s = e.get();
            let go = s.take(app_monotonic_ns(), escape_ns);
            e.set(s);
            go
        })
    }

    fn block_for(&mut self, ns: i64) {
        block_for(ns);
    }

    fn spin_turn(&mut self, turn: super::wait::Turn) {
        // The spin yields, on every thread (dstack #1). SwitchToThread
        // (`std::thread::yield_now`) lets ANY ready thread on the CPU run,
        // lower priority included, for the rest of its slice: on the FOH
        // PC (5.11.2026) a priority-10 thread kept the band TX thread's CPU
        // for 1.66 ms and the TX schedule started again. Every other spin
        // froze a Windows PC: Sleep(0) in v0.6.28 (why is not known: on
        // windows-2025 a Sleep(0) spin does let a lower priority run), and
        // the no-yield spin of pinned threads in v0.6.34/35 (FOH, 5.11.2026:
        // per-CPU system work that must run on that exact CPU never ran).
        // Without a CPU of its own it also starves its neighbours (v0.6.34
        // on dasio's runner). Never make this spin starve lower priorities.
        // The bounded escape (off by default, a trial) is the one
        // exception: its spin does not yield at all between the blocks
        // (Sleep(0) is no shelter: on windows-2025 a lower-priority thread
        // ran 21 million turns in 0.3 s of a Sleep(0) spin, and the waits
        // went 3 ms late), and its block once a period lets lower
        // priorities run. Which turn is `wait::until`'s decision: with the
        // device's `spin_no_yield_tail` (dstack #351, opt-in, off by
        // default) no yield in the last `wait::NO_YIELD_NS` (Defender kept
        // dsg's pump's CPU 1.4 ms after a late yield), with a give at least
        // every `wait::GIVE_CAP_NS` so lower priorities still run.
        match turn {
            super::wait::Turn::Yield => std::thread::yield_now(),
            super::wait::Turn::Pause => std::hint::spin_loop(),
        }
    }
}

thread_local! {
    /// When this thread last gave its CPU away (dstack #351).
    static GIVES: std::cell::Cell<super::wait::Gives> =
        const { std::cell::Cell::new(super::wait::Gives::new()) };
}

/// A wait until the clock `now` reaches `target` in the wait `mode`, with a
/// bounded escape every `escape_ns` (0: none; the spin wait only), and the
/// no-yield tail when `no_yield_tail` (dstack #351, the device's
/// `spin_no_yield_tail`; off: the v0.6.157 wait).
fn sleep_until_in(
    target: i64,
    now: fn() -> i64,
    mode: super::WaitMode,
    escape_ns: u64,
    no_yield_tail: bool,
) {
    // The bounded escape (dstack #1, `spin_escape_ms`): once a period the
    // thread blocks one timer tick, whatever its deadline. The timer's
    // expiry preempts a lower priority at once, so per-CPU system work
    // that must run on this CPU gets it each period, and the spin needs
    // no SwitchToThread. TX's relay thread covers its block
    // (`block_escape`, dstack #141); a pump's W absorbs the block.
    let escape = mode == super::WaitMode::Spin && escape_ns != 0 && ALLOWED.with(|a| a.get());
    // The timer mode (problem A's measurement): the whole wait on the
    // timer, no core spun.
    let tail = super::spin::SPIN_TAIL.load(std::sync::atomic::Ordering::Relaxed);
    let spin = i64::try_from(super::spin_tail_for(mode, tail)).unwrap_or(i64::MAX);
    let mut gives = GIVES.with(|g| g.get());
    super::wait::until(
        &mut Os { now, escape_ns },
        target,
        super::wait::Plan {
            spin_tail_ns: spin,
            escape,
            escape_block_ns: super::ESCAPE_BLOCK_NS as i64,
            guard: super::wait::guard_of(no_yield_tail),
            give_on_timer: TIMER_GIVE.with(|t| t.get()),
        },
        &mut gives,
    );
    GIVES.with(|g| g.set(gives));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests that pin a TIME_CRITICAL thread to CPU 0 run one at a
    /// time: two such spinners would hand CPU 0 to each other.
    static CPU0_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A busy LOWEST-priority thread pinned to CPU 0 while `f` runs on this
    /// thread, pinned there at TIME_CRITICAL; `f`'s result and how many
    /// turns the busy thread ran meanwhile. This thread stays pinned (libtest
    /// ends it with the test); the busy thread stops even when `f` panics.
    fn beside_a_busy_lower_thread<T>(f: impl FnOnce() -> T) -> (T, u64) {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadAffinityMask, SetThreadPriority,
        };
        /// THREAD_PRIORITY_TIME_CRITICAL and THREAD_PRIORITY_LOWEST.
        const TIME_CRITICAL: i32 = 15;
        const LOWEST: i32 = -2;
        /// Stops the busy thread when dropped.
        struct Stop(Arc<AtomicBool>);
        impl Drop for Stop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        /// CPU 0 only.
        const CPU0: usize = 1;
        let _one = CPU0_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let pin = |priority: i32| {
            // SAFETY: the calling thread's pseudo handle.
            unsafe {
                assert_ne!(SetThreadAffinityMask(GetCurrentThread(), CPU0), 0);
                assert_ne!(SetThreadPriority(GetCurrentThread(), priority), 0);
            }
        };
        let stop = Stop(Arc::new(AtomicBool::new(false)));
        let turns = Arc::new(AtomicU64::new(0));
        let busy = {
            let (stop, turns) = (Arc::clone(&stop.0), Arc::clone(&turns));
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
        let out = f();
        let ran = turns.load(Ordering::Relaxed) - before;
        drop(stop);
        busy.join().unwrap();
        (out, ran)
    }

    /// The calling thread's timer state (#339): the OS's timer resolution
    /// and whether this thread got its high-resolution waitable timer.
    fn timer_state() -> super::super::timer_state::TimerState {
        use windows_sys::Wdk::System::SystemInformation::NtQueryTimerResolution;
        let (mut coarsest, mut finest, mut current) = (0u32, 0u32, 0u32);
        // SAFETY: three valid, writable u32s; the call only writes them.
        let status = unsafe { NtQueryTimerResolution(&mut coarsest, &mut finest, &mut current) };
        super::super::timer_state::from_nt(status, coarsest, finest, current, !timer().is_null())
    }

    /// dlink (#68): the normal profile's waits end on the high-resolution
    /// timer, not on the 15.6 ms system tick (SongPlayer 31.10: peaks of two
    /// ticks). The median of 200 waits of 1 ms oversleeps under 1 ms.
    #[test]
    fn a_timer_wait_of_1_ms_ends_within_1_ms_at_the_median() {
        let mut late: Vec<i64> = (0..200)
            .map(|_| {
                let target = monotonic_ns() + 1_000_000;
                sleep_until_in(
                    target,
                    monotonic_ns,
                    super::super::WaitMode::Timer,
                    0,
                    false,
                );
                monotonic_ns() - target
            })
            .collect();
        late.sort_unstable();
        let median = late[late.len() / 2];
        assert!(
            (0..1_000_000).contains(&median),
            "median oversleep {median} ns; p90 {} ns, max {} ns; {}",
            late[late.len() * 9 / 10],
            late[late.len() - 1],
            super::super::timer_state::describe(median, true, &timer_state())
        );
    }

    /// The test clock's waits (dstack #328): a wait on the app's clock
    /// (raw QPC) ends on it, never before its target, in both wait modes,
    /// whatever the wall clock (the media threads' monotonic clock until a
    /// poll) does meanwhile. Before, the deadline was on the wall.
    #[test]
    fn a_wait_on_the_app_clock_never_ends_before_its_target_on_it() {
        for mode in [super::super::WaitMode::Spin, super::super::WaitMode::Timer] {
            let mut late: Vec<i64> = (0..100)
                .map(|_| {
                    let target = app_monotonic_ns() + 1_000_000;
                    sleep_until_in(target, app_monotonic_ns, mode, 0, false);
                    app_monotonic_ns() - target
                })
                .collect();
            late.sort_unstable();
            assert!(late[0] >= 0, "{mode:?}: ended {} ns early", -late[0]);
            let median = late[late.len() / 2];
            assert!(
                median < 1_000_000,
                "{mode:?}: median late {median} ns; {}",
                super::super::timer_state::describe(
                    median,
                    mode == super::super::WaitMode::Timer,
                    &timer_state()
                )
            );
        }
        let target = app_monotonic_ns() + 2_000_000;
        sleep_until_app(target);
        assert!(app_monotonic_ns() >= target);
    }

    /// FOH froze (5.11.2026, ~04:48Z) right after dasio rolled v0.6.35:
    /// its pinned TX and engine threads spun without yielding, and some
    /// per-CPU system work that must run on exactly those CPUs never ran
    /// (the same freeze as v0.6.28's Sleep(0)); in v0.6.34 unpinned
    /// spinners starved dasio's runner. A spun wait, pinned or not, lets a
    /// lower-priority thread on its CPU run: with the no-yield tail off (a
    /// yield every turn) and on (dstack #351: the capped give).
    #[test]
    fn a_spun_wait_lets_a_lower_priority_thread_on_its_cpu_run() {
        for no_yield_tail in [false, true] {
            let ((), ran) = beside_a_busy_lower_thread(|| {
                for _ in 0..100 {
                    sleep_until_in(
                        monotonic_ns() + 300_000,
                        monotonic_ns,
                        super::super::WaitMode::Spin,
                        0,
                        no_yield_tail,
                    );
                }
            });
            assert!(
                ran > 0,
                "the lower-priority thread never ran during 100 spun waits \
                 (no-yield tail {no_yield_tail})"
            );
        }
    }

    /// dsg (dstack #351, dasio#120): the pump's spin yielded right up to its
    /// due time, and Defender (priority 6-8) kept its CPU 1.4 ms on a
    /// process start; the pump re-bootstrapped. With the device's
    /// `spin_no_yield_tail` on (opt-in since the ROZHODNUTÉ of 17.11.2026;
    /// this test runs it on), a wait that starts inside the no-yield tail
    /// (`wait::NO_YIELD_NS`) yields only as its capped
    /// give (`wait::GIVE_CAP_NS`): with a busy lower-priority thread on its
    /// CPU, ~1 s of 150 µs waits stay on time (median late under 100 µs, p90
    /// under 500 µs; a yield's victim keeps the CPU for its slice: median
    /// 2.49 ms on windows-2025), and the busy thread still runs, in the
    /// capped gives, a yield or a timer wait ([`give_on_timer`]).
    #[test]
    fn a_wait_inside_the_no_yield_tail_never_hands_a_lower_priority_its_slice() {
        const WAIT_NS: i64 = 150_000;
        const _: () = assert!(WAIT_NS < super::super::wait::NO_YIELD_NS);
        // Both capped gives: a yield (any thread), the timer (a pump whose
        // W absorbs its late wake).
        for on_timer in [false, true] {
            let (mut late, ran) = beside_a_busy_lower_thread(|| {
                give_on_timer(on_timer);
                let end = monotonic_ns() + 1_000_000_000;
                let mut late = Vec::with_capacity(8_000);
                while monotonic_ns() < end {
                    let target = monotonic_ns() + WAIT_NS;
                    sleep_until_in(target, monotonic_ns, super::super::WaitMode::Spin, 0, true);
                    late.push(monotonic_ns() - target);
                }
                give_on_timer(false);
                late
            });
            late.sort_unstable();
            let (median, p90) = (late[late.len() / 2], late[late.len() * 9 / 10]);
            let report = format!(
                "give on the timer {on_timer}: {} waits: median late {median} ns, p90 {p90} ns, \
                 p99 {} ns, max {} ns, over 0.5 ms: {}; the lower thread ran {ran}",
                late.len(),
                late[late.len() * 99 / 100],
                late[late.len() - 1],
                late.iter().filter(|&&l| l > 500_000).count(),
            );
            assert!(median < 100_000 && p90 < 500_000, "{report}");
            assert!(ran > 0, "the lower-priority thread never ran: {report}");
        }
    }

    /// The bounded escape (dstack #1, `spin_escape_ms`, a trial): the spin
    /// does not yield between the blocks, so a lower priority gets nothing there, and the
    /// thread blocks one timer tick each period. Over about 1 s of spun
    /// waits at a 100 ms period, a busy lower-priority thread on the same
    /// CPU still runs (in the blocks), and the waits stay on time: the
    /// median is late under 100 µs (a yield's victim kept the CPU for its
    /// slice: median 2.49 ms on windows-2025, v0.6.34's test).
    #[test]
    fn the_bounded_escape_lets_a_lower_priority_thread_run_and_keeps_waits_on_time() {
        let _one = CPU0_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadAffinityMask, SetThreadPriority,
        };
        const TIME_CRITICAL: i32 = 15;
        const LOWEST: i32 = -2;
        const CPU0: usize = 1;
        const PERIOD_NS: u64 = 100_000_000;
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
        // As the TX thread does (`media::tx::run`).
        allow_escape(true);
        // The spin alone first (a period past the run: one block at the
        // start, then the spin only), for the failure message: whether the
        // lower thread gets the CPU from the spin or only in the blocks.
        let (spin_ran, spin_late) = {
            let before = turns.load(Ordering::Relaxed);
            let end = monotonic_ns() + 300_000_000;
            let mut late = Vec::with_capacity(1_000);
            while monotonic_ns() < end {
                let target = monotonic_ns() + 300_000;
                sleep_until_in(
                    target,
                    monotonic_ns,
                    super::super::WaitMode::Spin,
                    10_000_000_000,
                    false,
                );
                late.push(monotonic_ns() - target);
            }
            late.sort_unstable();
            (turns.load(Ordering::Relaxed) - before, late)
        };
        let before = turns.load(Ordering::Relaxed);
        let end = monotonic_ns() + 1_050_000_000;
        let mut late = Vec::with_capacity(4_000);
        while monotonic_ns() < end {
            let target = monotonic_ns() + 300_000;
            sleep_until_in(
                target,
                monotonic_ns,
                super::super::WaitMode::Spin,
                PERIOD_NS,
                false,
            );
            late.push(monotonic_ns() - target);
        }
        let ran = turns.load(Ordering::Relaxed) - before;
        stop.store(true, Ordering::Relaxed);
        busy.join().unwrap();
        late.sort_unstable();
        let median = late[late.len() / 2];
        assert!(
            ran > 0,
            "the lower-priority thread never ran during {} escaping waits",
            late.len()
        );
        assert!(
            median < 100_000,
            "median late {median} ns over {} waits; p99 {} ns, max {} ns; over 0.5 ms: {}; \
             the spin alone: {} waits, median {} ns, max {} ns, lower thread ran {}",
            late.len(),
            late[late.len() * 99 / 100],
            late[late.len() - 1],
            late.iter().filter(|&&l| l > 500_000).count(),
            spin_late.len(),
            spin_late[spin_late.len() / 2],
            spin_late[spin_late.len() - 1],
            spin_ran
        );
    }
}
