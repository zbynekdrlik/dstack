// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The Windows media threads' wait (dstack #1 problem A, #328), on the
//! clock it is given: the timer for all but the spin tail, then a spin
//! that yields (or, under the bounded escape, a pause; with the device's
//! `spin_no_yield_tail`, dstack #351, a pause near the due time).
//! Cross-platform so Linux tests it with a fake OS clock; only Windows
//! waits with it.

/// No yield in the last this much before a wait's due time, ns (dstack
/// #351). A yield hands the CPU to any ready thread on it, lower priority
/// included, for the rest of that thread's slice (dsg: Defender kept the
/// pump's CPU 1416, 682 and 381 µs), so a yield with `left` to go is a late
/// wake of the slice less `left`. A pump re-bootstraps past W late
/// (`stream::Blocks::next`): at dsg's W (96 at 96 kHz, 1 ms) a slice up to
/// W + 500 µs (the trace's 1416 µs included) costs it nothing. That pump's
/// waits (333 µs a period) all start inside these 500 µs, so it gives its
/// CPU only through [`GIVE_CAP_NS`]; a thread with longer waits still
/// yields on every turn before them. Only with the device's
/// `spin_no_yield_tail` ([`guard_of`]).
pub(crate) const NO_YIELD_NS: i64 = 500_000;

/// The longest a spun wait outside the bounded escape goes without the
/// thread giving its CPU away, ns (dstack #351): a wait that would yield
/// nothing (it starts inside [`NO_YIELD_NS`]) and would end this long or
/// more after the thread's last give (a yield or a timer block) gives at
/// its first turn: a yield, or, for a thread whose W absorbs the timer's
/// late wake ([`super::pump_gives_on_timer`]), the timer for its whole
/// length (its expiry preempts a lower priority at once). Under one
/// Windows clock interval (15.625 ms): a ready lower-priority thread on a
/// busy CPU waits a quantum anyway. Per-CPU system work that must run on
/// this CPU therefore runs at least this often (v0.6.28 and v0.6.35 froze
/// FOH: no give, ever). A wait already past its due time makes no turn
/// and gives nothing, as before. Only with the device's
/// `spin_no_yield_tail` ([`guard_of`]).
pub(crate) const GIVE_CAP_NS: i64 = 10_000_000;

/// How far past a timer wait's due time its fallback wake lands, ms
/// (dstack #475): the high-resolution timer normally wakes the thread
/// within a few hundred microseconds of the due time, so the fallback
/// (the wait's own timeout, expiring on the waiting CPU) only fires when
/// the timer's expiry is held.
pub(crate) const FALLBACK_SLACK_MS: u32 = 1;

/// The timeout of a timer wait with `left_ns` to go, ms (dstack #475,
/// `sys_windows::wait_armed`): `left_ns` rounded up to whole ms, plus
/// [`FALLBACK_SLACK_MS`]; never `INFINITE` (u32::MAX), so a held timer
/// expiry costs a wait about two ms, not the hold.
pub(crate) fn fallback_ms(left_ns: i64) -> u32 {
    let ms = u64::try_from(left_ns.max(0))
        .unwrap_or(0)
        .div_ceil(1_000_000);
    let ms = ms.saturating_add(u64::from(FALLBACK_SLACK_MS));
    u32::try_from(ms).unwrap_or(u32::MAX - 1).min(u32::MAX - 1)
}

/// What a wait needs from the OS.
pub(crate) trait WaitOs {
    /// The clock the deadline is on, ns.
    fn now_ns(&mut self) -> i64;
    /// The app's clock, ns: the one every thread's [`Gives`] is counted on,
    /// whatever clock its deadlines are on (dstack #351).
    fn app_ns(&mut self) -> i64;
    /// Whether this wait makes the bounded escape now (the thread's escape
    /// state; asked only when the plan has the escape).
    fn take_escape(&mut self) -> bool;
    /// Blocks the thread on its timer for `ns`.
    fn block_for(&mut self, ns: i64);
    /// One turn of the spin.
    fn spin_turn(&mut self, turn: Turn);
}

/// One turn of the spin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Turn {
    /// Gives the CPU to any ready thread on it (SwitchToThread).
    Yield,
    /// A CPU pause; the thread keeps its CPU.
    Pause,
}

/// How a wait runs.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Plan {
    /// The spin tail, ns: the last this much is spun (0: no spin).
    pub spin_tail_ns: i64,
    /// Whether this wait may make the bounded escape (dstack #1).
    pub escape: bool,
    /// The escape's block, ns.
    pub escape_block_ns: i64,
    /// The no-yield tail and its give cap (dstack #351), or `None`: the
    /// spin yields on every turn and counts no gives (v0.6.157's wait,
    /// [`guard_of`]).
    pub guard: Option<Guard>,
    /// The capped give is the timer for the wait's whole length, not a
    /// yield ([`super::pump_gives_on_timer`]; read only with a guard).
    pub give_on_timer: bool,
}

/// The no-yield tail and its give cap (dstack #351).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Guard {
    /// No yield in the last this much, ns ([`NO_YIELD_NS`]).
    pub no_yield_ns: i64,
    /// The longest spin without a give, ns ([`GIVE_CAP_NS`]).
    pub give_cap_ns: i64,
}

/// The guard of the process's waits for the device's `spin_no_yield_tail`
/// (dstack #351, opt-in until its non-production soak passes): the shipped
/// [`NO_YIELD_NS`] and [`GIVE_CAP_NS`] when on; `None` when off, the
/// v0.6.157 wait (a yield on every spin turn, no cap).
pub(crate) const fn guard_of(on: bool) -> Option<Guard> {
    if on {
        Some(Guard {
            no_yield_ns: NO_YIELD_NS,
            give_cap_ns: GIVE_CAP_NS,
        })
    } else {
        None
    }
}

/// When a thread last gave its CPU away (a yield or a timer block), on the
/// app's clock (dstack #351). Kept per thread across its waits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Gives {
    /// The app clock's ns of the last give, or of the first look.
    last: Option<i64>,
}

impl Gives {
    pub(crate) const fn new() -> Self {
        Gives { last: None }
    }

    /// The thread gave its CPU away at `app_now`.
    pub(crate) fn gave(&mut self, app_now: i64) {
        self.last = Some(app_now);
    }

    /// Whether a wait with `left` to go at `app_now` that gives nothing
    /// would end `cap` or more after the last give; the first look only
    /// starts the count (as `EscapeState`'s).
    fn owed(&mut self, app_now: i64, left: i64, cap: i64) -> bool {
        match self.last {
            None => {
                self.last = Some(app_now);
                false
            }
            Some(last) => app_now.saturating_sub(last).saturating_add(left) >= cap,
        }
    }
}

/// Waits until `os`'s clock reaches `target`: the escape's block when it is
/// due, the timer for all but the spin tail, then the spin. Without a
/// [`Plan::guard`] it yields on every turn (v0.6.157). With one (dstack
/// #351) it yields until [`Guard::no_yield_ns`] before `target` and pauses
/// from there, and gives once when it would give nothing for
/// [`Guard::give_cap_ns`] ([`Gives::owed`]): a yield, or the timer for the
/// rest of the wait ([`Plan::give_on_timer`]). Under the bounded escape
/// it pauses throughout, uncapped (the escape's block is its give).
pub(crate) fn until(os: &mut impl WaitOs, target: i64, plan: Plan, gives: &mut Gives) {
    if plan.escape && os.take_escape() {
        os.block_for(plan.escape_block_ns);
        gave(os, plan, gives);
    }
    let left = target.saturating_sub(os.now_ns());
    if left > plan.spin_tail_ns {
        os.block_for(left - plan.spin_tail_ns);
        gave(os, plan, gives);
    }
    if plan.spin_tail_ns == 0 {
        return;
    }
    loop {
        let left = target.saturating_sub(os.now_ns());
        if left <= 0 {
            return;
        }
        match plan.guard {
            _ if plan.escape => os.spin_turn(Turn::Pause),
            None => os.spin_turn(Turn::Yield),
            Some(guard) => guarded_turn(os, left, guard, plan.give_on_timer, gives),
        }
    }
}

/// Counts a give at the app clock's now, only for a guarded wait (an
/// unguarded one keeps no count: v0.6.157's wait).
fn gave(os: &mut impl WaitOs, plan: Plan, gives: &mut Gives) {
    if plan.guard.is_some() {
        gives.gave(os.app_ns());
    }
}

/// One turn of a guarded spin (dstack #351) with `left` to go.
fn guarded_turn(
    os: &mut impl WaitOs,
    left: i64,
    guard: Guard,
    give_on_timer: bool,
    gives: &mut Gives,
) {
    if left > guard.no_yield_ns {
        os.spin_turn(Turn::Yield);
        gives.gave(os.app_ns());
    } else if gives.owed(os.app_ns(), left, guard.give_cap_ns) {
        if give_on_timer {
            os.block_for(left);
        } else {
            os.spin_turn(Turn::Yield);
        }
        gives.gave(os.app_ns());
    } else {
        os.spin_turn(Turn::Pause);
    }
}

#[cfg(test)]
mod tests;
