// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The TX thread's lost time split by cause (dstack #359), as the pump's
//! is ([`crate::stream::split`], dstack #354).
//!
//! The TX schedule starts again ([`super::Schedule::due`]) when a pass
//! reads the clock more than the latency past a packet's due time. Three
//! things put the TX thread there, and each is measured:
//!
//! - **The sleep returned late** (`oversleep`): the media clock read right
//!   after the TX thread's sleep less the time it asked for, read on one
//!   clock epoch ([`crate::stream::split::oversleep_ns`]). Large here and
//!   the thread was scheduled late: not run when its timer fired.
//! - **The pass before ran long** (`pass`): a pass's own wall time
//!   ([`Transmitter::timed_step`]: the plans, the keepalives and every
//!   packet due). A pass that ends past the next packet's time asks its
//!   sleep for a time already past, which records no oversleep.
//! - **The pass was preempted** (`pass_cpu`, Linux and Windows x86_64,
//!   dstack #485): the TX thread's CPU time over the same pass
//!   ([`crate::stream::split::thread_cpu_ns`]). A pass far longer than its
//!   CPU time was not run in its middle; one whose CPU time is near its
//!   wall time was current on its CPU, which on a VM does not prove work
//!   (dstack #354: the host's steal and the interrupts count as the
//!   thread's; Windows leaves the interrupts' and DPCs' cycles out).
//!
//! Each figure is a peak since the start (the status `tx_thread`) and
//! since the late watch's last look (its line, [`TxThreadLook`], dstack
//! #485), and the last of each goes into the restart line ([`Before`]), so
//! a restart reads scheduled late, pass long or preempted. The TX thread only
//! records (relaxed `fetch_max`, no lock, no allocation); the thread CPU
//! clock is read around a pass, two system calls on Linux (no vDSO path).

use std::time::Instant;

use super::{Transmitter, nanos};
use crate::clock::{self, PtpClock};
use crate::stream::split::{Peak, cpu_between, oversleep_ns, thread_cpu_ns};

/// The TX thread's peaks since the start (the status) and since the late
/// watch's last look (dstack #485, its line): written by the TX thread,
/// read by the control side.
#[derive(Debug, Default)]
pub struct TxThreadStats {
    oversleep_ns: Peak,
    pass_ns: Peak,
    /// A pass's CPU time plus 1, 0 while none was read: "measured" rides
    /// in the peak itself, so a look takes both in one swap (a flag beside
    /// it could pair a taken 0 with a measured mark).
    pass_cpu_ns_1: Peak,
}

/// [`TxThreadStats`] as the status shows it, µs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TxThreadView {
    /// The largest return of the TX thread's sleep past its target.
    pub oversleep_max_us: u64,
    /// The longest pass, wall time.
    pub pass_max_us: u64,
    /// The largest CPU time of a pass; `None` where unmeasured (off Linux
    /// and Windows x86_64, and a Windows process's first 100 ms).
    pub pass_max_cpu_us: Option<u64>,
}

/// The TX thread's peaks since the late watch's last look, ns (dstack
/// #485): the late line says from them whether the pass that sent the late
/// packets ran long (CPU time near its wall time) or was held (wall time
/// far above its CPU time: a DPC, an ISR or another thread), or whether
/// the sleep before it returned late.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TxThreadLook {
    /// The longest pass, wall time.
    pub pass_ns: u64,
    /// The largest CPU time of a pass; `None` when no pass's was read.
    pub pass_cpu_ns: Option<u64>,
    /// The largest return of the sleep past its target.
    pub oversleep_ns: u64,
}

impl TxThreadStats {
    /// A sleep that returned `ns` past its target.
    pub fn overslept(&self, ns: u64) {
        self.oversleep_ns.record(ns);
    }

    /// A pass that took `wall_ns`, of it `cpu_ns` on the CPU (`None`:
    /// unmeasured).
    pub fn passed(&self, wall_ns: u64, cpu_ns: Option<u64>) {
        self.pass_ns.record(wall_ns);
        if let Some(cpu) = cpu_ns {
            self.pass_cpu_ns_1.record(cpu.saturating_add(1));
        }
    }

    /// The peaks since the start.
    pub fn view(&self) -> TxThreadView {
        TxThreadView {
            oversleep_max_us: self.oversleep_ns.max() / 1_000,
            pass_max_us: self.pass_ns.max() / 1_000,
            pass_max_cpu_us: self.pass_cpu_ns_1.max().checked_sub(1).map(|ns| ns / 1_000),
        }
    }

    /// The peaks since the last call, and a fresh start for the next (the
    /// late watch, once a second); the peaks since the start stay.
    pub fn take_look(&self) -> TxThreadLook {
        TxThreadLook {
            pass_ns: self.pass_ns.take_second(),
            pass_cpu_ns: self.pass_cpu_ns_1.take_second().checked_sub(1),
            oversleep_ns: self.oversleep_ns.take_second(),
        }
    }
}

/// What the TX thread measured of the time before its current pass, each
/// `None` when unmeasured: how late its last sleep returned (none when it
/// asked for a time already past, or the clock re-anchored meanwhile, or
/// the pass came after a bounded escape), and its last pass's wall and CPU
/// time. The restart line carries them ([`super::Schedule::due_after`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Before {
    pub oversleep_ns: Option<u64>,
    pub pass_ns: Option<u64>,
    pub pass_cpu_ns: Option<u64>,
}

/// A time in the restart line: whole µs, `-` when unmeasured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Us(pub Option<u64>);

impl std::fmt::Display for Us {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(ns) => write!(f, "{}", ns / 1_000),
            None => f.write_str("-"),
        }
    }
}

impl Transmitter {
    /// One pass ([`Transmitter::step`]) timed: `read` gives a wall time,
    /// ns on any fixed base, and the thread's CPU time (`None`:
    /// unmeasured), read before and after it. The pass's times become the
    /// next restart line's and count in the peaks; the sleep before it is
    /// its own (a restart in a later send without a sleep, the bounded
    /// escape's last pass, names none).
    pub fn timed_step(
        &mut self,
        now_ns: Option<u64>,
        anchor: impl Into<clock::Anchor>,
        mut read: impl FnMut() -> (u64, Option<u64>),
    ) {
        let (wall_before, cpu_before) = read();
        self.step(now_ns, anchor);
        self.before.oversleep_ns = None;
        let (wall_after, cpu_after) = read();
        let wall = wall_after.saturating_sub(wall_before);
        let cpu = cpu_between(cpu_before, cpu_after);
        self.before.pass_ns = Some(wall);
        self.before.pass_cpu_ns = cpu;
        self.shared.tx_thread.passed(wall, cpu);
    }

    /// The TX thread slept until PTP `target_ns`, reading the media clock
    /// just before the sleep (`asked_ns`) and just after it (`woke_ns`;
    /// `None`: not ready), with the clock's epoch unchanged across it
    /// (`same_epoch`): how late it returned becomes the next restart
    /// line's and counts in the peak; unmeasured when the sleep was asked
    /// for a time already past (the pass before ran into it).
    pub fn slept(
        &mut self,
        target_ns: u64,
        asked_ns: Option<u64>,
        woke_ns: Option<u64>,
        same_epoch: bool,
    ) {
        let late = match (asked_ns, woke_ns, same_epoch) {
            (Some(asked), Some(woke), true) => oversleep_ns(target_ns, asked, woke),
            _ => None,
        };
        self.before.oversleep_ns = late;
        if let Some(late) = late {
            self.shared.tx_thread.overslept(late);
        }
    }
}

/// One pass of a TX loop (the TX thread's or, while it covers a bounded
/// escape, the relay thread's) at `clock`'s time, timed on the calling
/// thread ([`Transmitter::timed_step`]; the wall time from `base`): the
/// clock's anchor and time it read.
pub(super) fn timed_pass(
    clock: &dyn PtpClock,
    tx: &mut Transmitter,
    base: Instant,
) -> (clock::Anchor, Option<u64>) {
    // The epoch before the time: the clock moves k before the epoch, so a
    // pass never sees a new epoch with the old time ([`super::shift`]).
    let anchor = clock::Anchor::of(clock);
    let now = clock.now_ns();
    tx.timed_step(now, anchor, || (nanos(base.elapsed()), thread_cpu_ns()));
    (anchor, now)
}

/// A TX loop's sleep until PTP `target_ns`, at most its distance from the
/// pass's own time read `read` (dstack #440: a re-anchor since that read
/// is the next pass's, never a sleep stretched by its step), the media
/// clock read right before and after it and its epoch checked against
/// `epoch` (the pass's): how late it returned is recorded
/// ([`Transmitter::slept`]).
pub(super) fn timed_sleep(
    clock: &dyn PtpClock,
    tx: &mut Transmitter,
    target_ns: u64,
    read: Option<u64>,
    epoch: u32,
) {
    let asked = clock.now_ns();
    match read {
        Some(read) => clock.sleep_until_within(target_ns, target_ns.saturating_sub(read)),
        None => clock.sleep_until(target_ns),
    }
    let woke = clock.now_ns();
    let same = clock::Anchor::of(clock).epoch == epoch;
    tx.slept(target_ns, asked, woke, same);
}
