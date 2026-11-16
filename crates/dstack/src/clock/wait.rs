// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The Windows media threads' wait (dstack #1 problem A, #328), on the
//! clock it is given: the timer for all but the spin tail, then a spin
//! that yields (or, under the bounded escape, a pause). Cross-platform so
//! Linux tests it with a fake OS clock; only Windows waits with it.

/// No yield in the last this much before a wait's due time, ns (dstack
/// #351). A yield hands the CPU to any ready thread on it, lower priority
/// included, for the rest of that thread's slice (dsg: Defender kept the
/// pump's CPU 1416, 682 and 381 µs), so a yield with `left` to go is a late
/// wake of the slice less `left`. With no yield in the last 250 µs a wait
/// is late by a slice less 250 µs at most, and a slice of 250 µs or less
/// costs nothing: the trace's longest (1416 µs) stays under the pump's
/// budget at block 32 and W 96 at 96 kHz (1.333 ms), and every period of
/// that pump (333 µs) still has a yield before its tail.
pub(crate) const NO_YIELD_NS: i64 = 250_000;

/// The longest a media thread spins without giving its CPU away, ns
/// (dstack #351): a spun wait that would yield nothing (it starts inside
/// [`NO_YIELD_NS`]) and would end this long or more after the thread's last
/// give (a yield or a timer block) goes on the timer instead, whose expiry
/// preempts a lower priority at once (late by the timer's late wake at
/// most, [`super::TIMER_LATE_NS`], never a slice). Under one Windows clock
/// interval (15.625 ms): a ready lower-priority thread on a busy CPU waits a
/// quantum anyway. Per-CPU system work that must run on this CPU therefore
/// runs at least this often (v0.6.28 and v0.6.35 froze FOH: no give, ever).
pub(crate) const GIVE_CAP_NS: i64 = 10_000_000;

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
    /// No yield in the last this much, ns ([`NO_YIELD_NS`]).
    pub no_yield_ns: i64,
    /// The longest spin without a give, ns ([`GIVE_CAP_NS`]).
    pub give_cap_ns: i64,
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
/// due, the timer for all but the spin tail, then the spin: it yields
/// until [`Plan::no_yield_ns`] before `target` and pauses from there
/// (dstack #351), and goes on the timer instead when it would give nothing
/// for [`Plan::give_cap_ns`] ([`Gives::owed`]). Under the bounded escape
/// it pauses throughout, uncapped (the escape's block is its give).
pub(crate) fn until(os: &mut impl WaitOs, target: i64, plan: Plan, gives: &mut Gives) {
    if plan.escape && os.take_escape() {
        os.block_for(plan.escape_block_ns);
        gives.gave(os.app_ns());
    }
    let left = target.saturating_sub(os.now_ns());
    if left > plan.spin_tail_ns {
        os.block_for(left - plan.spin_tail_ns);
        gives.gave(os.app_ns());
    }
    if plan.spin_tail_ns == 0 {
        return;
    }
    loop {
        let left = target.saturating_sub(os.now_ns());
        if left <= 0 {
            return;
        }
        if plan.escape {
            os.spin_turn(Turn::Pause);
        } else if left > plan.no_yield_ns {
            os.spin_turn(Turn::Yield);
            gives.gave(os.app_ns());
        } else if gives.owed(os.app_ns(), left, plan.give_cap_ns) {
            os.block_for(left);
            gives.gave(os.app_ns());
        } else {
            os.spin_turn(Turn::Pause);
        }
    }
}

#[cfg(test)]
mod tests;
