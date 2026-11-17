// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The pump's lost time split by cause (dstack #354): a late wake or an
//! overrun read by measurement, not by reasoning.
//!
//! A pump that wakes behind its block ([`super::wake`]) or ends a block
//! past its period (an overrun) lost the time in one of four places, and
//! each has its own peak (since the start in [`super::PumpView`] and the
//! status, that second's in the pump watch's warning):
//!
//! - **The sleep returned late** (`oversleep`): the media clock read right
//!   after the sleep returned less the time it was asked to end
//!   ([`oversleep_ns`], [`super::PumpStats::slept`]). Large here and the
//!   late wake was the scheduler's: the thread was not run when its timer
//!   fired.
//! - **The drain ran long** (`drain`, `drain_datagrams`): each socket drain
//!   before a block ([`crate::media::rx::Intake::drain_into_rings`]), its
//!   wall time and the datagrams it read. It grows with the backlog (about
//!   2 us a datagram on dev1, #354).
//! - **The block's work** (`max_process_cpu`): a block's own thread CPU
//!   time ([`thread_cpu_ns`], Linux), beside its wall time
//!   (`max_process_us`). A block with a long wall time and a short CPU time
//!   was not run in its middle (preempted), not slow. The converse does
//!   not hold: CPU time near the wall time proves only that the thread
//!   was the one current on its CPU, not that it worked. A kernel without
//!   `CONFIG_PARAVIRT_TIME_ACCOUNTING` (Ubuntu's generic and azure kernels)
//!   or a hypervisor that gives no steal clock (Hyper-V, under GitHub's
//!   Azure runners) counts the time the host took the virtual CPU as the
//!   thread's, and one without `CONFIG_IRQ_TIME_ACCOUNTING` (the same
//!   kernels) counts the interrupts and softirqs that ran meanwhile. No
//!   reading inside such a guest separates them from work (dstack #354:
//!   obs's 1.5-3.2 ms `cause=none` blocks with CPU = wall on a runner,
//!   against at most 56 us of CPU for the same block shape on dev1 under
//!   `chrt -f 10`, re-bootstraps, resyncs and 1 s stalls included).
//! - **The blocks before the sleep**: a wake whose sleep was asked for a
//!   time already past (the blocks and drains ran into the next block's
//!   time) records no oversleep; its lateness is the work's, seen in the
//!   two peaks above.
//!
//! The pump thread only records ([`Peak::record`]: two relaxed
//! `fetch_max`); the pump watch takes each second's peak once a second.

use std::sync::atomic::{AtomicU64, Ordering};

/// A peak since the start and since the pump watch's last look (once a
/// second): written by the pump thread, read by the control side.
#[derive(Debug, Default)]
pub struct Peak {
    max: AtomicU64,
    second: AtomicU64,
}

impl Peak {
    /// A new value (the pump thread): kept when larger than either peak.
    pub fn record(&self, value: u64) {
        self.max.fetch_max(value, Ordering::Relaxed);
        self.second.fetch_max(value, Ordering::Relaxed);
    }

    /// The largest value since the start.
    pub fn max(&self) -> u64 {
        self.max.load(Ordering::Relaxed)
    }

    /// The largest value since the last call, and a fresh start for the
    /// next (the pump watch, once a second).
    pub fn take_second(&self) -> u64 {
        self.second.swap(0, Ordering::Relaxed)
    }
}

/// How late a sleep returned, ns: the wake's clock read `woke_ns` less the
/// sleep's `target_ns`, when the sleep was asked at `asked_ns` before its
/// target; `None` when the target was not after the ask (the thread asked
/// for a time already past: no sleep, its lateness is the work before it).
/// A wake before the target (a clock read that moved back) is 0.
pub fn oversleep_ns(target_ns: u64, asked_ns: u64, woke_ns: u64) -> Option<u64> {
    (target_ns > asked_ns).then(|| woke_ns.saturating_sub(target_ns))
}

/// The CPU time a thread spent between two reads of [`thread_cpu_ns`] on
/// it, ns; `None` when either read is unmeasured.
pub fn cpu_between(before: Option<u64>, after: Option<u64>) -> Option<u64> {
    Some(after?.saturating_sub(before?))
}

/// The calling thread's CPU time, ns (`CLOCK_THREAD_CPUTIME_ID`); `None`
/// when it cannot be read.
#[cfg(target_os = "linux")]
pub fn thread_cpu_ns() -> Option<u64> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable timespec; clock_gettime only
    // writes it.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    timespec_ns(rc, ts.tv_sec, ts.tv_nsec)
}

/// A clock read's time, ns: `secs` and `nanos` of a read that returned
/// `rc` (0: read; else `None`, as for a negative part).
pub fn timespec_ns(rc: i32, secs: i64, nanos: i64) -> Option<u64> {
    let secs = u64::try_from(secs).ok()?;
    let nanos = u64::try_from(nanos).ok()?;
    if rc != 0 {
        return None;
    }
    secs.checked_mul(1_000_000_000)?.checked_add(nanos)
}

#[cfg(not(target_os = "linux"))]
pub use unmeasured_thread_cpu as thread_cpu_ns;

/// The calling thread's CPU time: unmeasured off Linux (Windows counts a
/// thread's time only per scheduler tick, about 15.6 ms, too coarse for a
/// 1 ms block). Excluded from the mutation gate, which never compiles it.
#[cfg(not(target_os = "linux"))]
pub fn unmeasured_thread_cpu() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests;
