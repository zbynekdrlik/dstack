// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The process-wide spin switches (dstack #369 moved them out of
//! `clock.rs`): the spin tail, the bounded escape's period and #351's
//! no-yield tail, each set from the device's config at its start (the last
//! device set wins). The Windows low-latency wait (`sys_windows`) reads the
//! values; [`escape_on`] is also read on every OS (the TX loop and the
//! pump's thread start). Re-exported at `clock::`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::{WaitMode, spin_escape_of};

/// The spin tail of the low-latency wait on Windows unless the device sets
/// one, ns: every wait shorter than this is spun ([`WaitMode::Spin`]).
pub const SPIN_TAIL_NS: u64 = 50_000_000;
/// The longest spin tail a device may set, ns.
pub const SPIN_TAIL_MAX_NS: u64 = 50_000_000;

/// The process's spin tail, ns ([`set_spin_tail_ns`]).
pub(super) static SPIN_TAIL: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(SPIN_TAIL_NS);

/// The spin tail for a device's `spin_tail_ns`: it, else
/// [`SPIN_TAIL_NS`], at most [`SPIN_TAIL_MAX_NS`].
pub fn spin_tail_of(configured: Option<u64>) -> u64 {
    configured.unwrap_or(SPIN_TAIL_NS).min(SPIN_TAIL_MAX_NS)
}

/// Sets the process's spin tail ([`spin_tail_of`] the device's).
pub fn set_spin_tail_ns(configured: Option<u64>) {
    SPIN_TAIL.store(
        spin_tail_of(configured),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// The spin tail of a wait in `mode` with the process's tail `tail_ns`:
/// none for the timer's wait.
pub fn spin_tail_for(mode: WaitMode, tail_ns: u64) -> u64 {
    match mode {
        WaitMode::Spin => tail_ns,
        WaitMode::Timer => 0,
    }
}

/// The process's bounded-escape period, ns; 0: off
/// ([`set_spin_escape_ms`]; process-wide, the last device set wins, as
/// the spin tail). Read by the Windows wait only.
pub(super) static SPIN_ESCAPE: AtomicU64 = AtomicU64::new(0);

/// Sets the process's bounded-escape period ([`spin_escape_of`] the
/// device's). Only the Windows low-latency wait reads it.
pub fn set_spin_escape_ms(configured_ms: Option<u64>) {
    SPIN_ESCAPE.store(spin_escape_of(configured_ms), Ordering::Relaxed);
}

/// Whether the process runs the bounded escape (`spin_escape_ms` set).
pub fn escape_on() -> bool {
    SPIN_ESCAPE.load(Ordering::Relaxed) != 0
}

/// Whether the process's low-latency spin keeps #351's no-yield tail
/// ([`set_spin_no_yield_tail`]; process-wide, the last device set wins, as
/// the spin tail). Off by default: the v0.6.157 wait. Read by the Windows
/// wait only.
static SPIN_NO_YIELD_TAIL: AtomicBool = AtomicBool::new(false);

/// Sets the process's no-yield tail (the device's `spin_no_yield_tail`,
/// dstack #351, opt-in until its non-production soak passes). Only the
/// Windows low-latency wait reads it.
pub fn set_spin_no_yield_tail(on: bool) {
    SPIN_NO_YIELD_TAIL.store(on, Ordering::Relaxed);
}

/// Whether the process's spin keeps the no-yield tail
/// ([`set_spin_no_yield_tail`]).
pub fn no_yield_tail_on() -> bool {
    SPIN_NO_YIELD_TAIL.load(Ordering::Relaxed)
}
