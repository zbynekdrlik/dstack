// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The Windows media threads' wait (dstack #1 problem A, #328), on the
//! clock it is given: the timer for all but the spin tail, then a spin
//! that yields (or, under the bounded escape, a pause). Cross-platform so
//! Linux tests it with a fake OS clock; only Windows waits with it.

/// What a wait needs from the OS.
pub(crate) trait WaitOs {
    /// The clock the deadline is on, ns.
    fn now_ns(&mut self) -> i64;
    /// Whether this wait makes the bounded escape now (the thread's escape
    /// state; asked only when the plan has the escape).
    fn take_escape(&mut self) -> bool;
    /// Blocks the thread on its timer for `ns`.
    fn block_for(&mut self, ns: i64);
    /// One turn of the spin: a yield, or a pause under the escape.
    fn spin_turn(&mut self, escape: bool);
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
}

/// Waits until `os`'s clock reaches `target`: the escape's block when it is
/// due, the timer for all but the spin tail, then the spin.
pub(crate) fn until(os: &mut impl WaitOs, target: i64, plan: Plan) {
    if plan.escape && os.take_escape() {
        os.block_for(plan.escape_block_ns);
    }
    let left = target.saturating_sub(os.now_ns());
    if left > plan.spin_tail_ns {
        os.block_for(left - plan.spin_tail_ns);
    }
    if plan.spin_tail_ns == 0 {
        return;
    }
    while target > os.now_ns() {
        os.spin_turn(plan.escape);
    }
}

#[cfg(test)]
mod tests;
