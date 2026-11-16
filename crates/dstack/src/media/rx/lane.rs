// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! A flow slot as the ring writer sees it ([`Lane`]: its plan, routes,
//! source, furthest end and open holes) and as the RX thread sees it
//! ([`Keep`]: its plan, for the group and the keepalives), and how its lost
//! holes are counted.

use std::net::SocketAddrV4;
use std::sync::atomic::Ordering;

use super::counts::RxStats;
use super::holes::{Holes, Lost};
use super::plan::RxPlan;

/// One flow slot as the ring writer ([`super::Intake`]) sees it.
#[derive(Debug)]
pub(super) struct Lane {
    /// The plan cell's version read last.
    pub(super) seen: Option<u32>,
    pub(super) plan: RxPlan,
    /// (channel slot, RX channel) for each RX channel routed here;
    /// allocated for every RX channel, so a rebuild never allocates.
    pub(super) targets: Vec<(usize, usize)>,
    /// Where the flow's audio comes from (also in the slot's stats, for
    /// the keepalives).
    pub(super) source: Option<SocketAddrV4>,
    /// The end of the furthest packet so far, samples.
    pub(super) end: Option<u64>,
    /// The runs of frames before `end` that no packet has filled yet
    /// (dstack #278): a gap only once lost ([`Holes`]).
    pub(super) holes: Holes,
}

impl Lane {
    pub(super) fn new(channels: usize) -> Self {
        Lane {
            seen: None,
            plan: RxPlan::default(),
            targets: Vec::with_capacity(channels),
            source: None,
            end: None,
            holes: Holes::default(),
        }
    }
}

/// One flow slot as the RX thread sees it: the plan's group and its
/// keepalives (dstack #326: the ring writer may be another thread).
#[derive(Debug, Default)]
pub(super) struct Keep {
    /// The plan cell's version read last.
    pub(super) seen: Option<u32>,
    pub(super) plan: RxPlan,
    pub(super) next_keepalive: Option<u64>,
}

/// Holes given up as lost, counted as gaps of their frames.
pub(super) fn count_lost(stats: &RxStats, lost: Lost) {
    stats.gaps.fetch_add(lost.holes, Ordering::Relaxed);
    stats.gap_frames.fetch_add(lost.frames, Ordering::Relaxed);
}
