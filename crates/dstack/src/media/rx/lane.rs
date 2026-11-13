// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! A flow slot as the RX thread sees it ([`Lane`]: its plan, routes,
//! source, keepalive schedule, furthest end and open holes), and how its
//! lost holes are counted.

use std::net::SocketAddrV4;
use std::sync::atomic::Ordering;

use super::counts::RxStats;
use super::holes::{Holes, Lost};
use super::plan::RxPlan;

/// One flow slot as the RX thread sees it.
#[derive(Debug)]
pub(super) struct Lane {
    /// The plan cell's version read last.
    pub(super) seen: Option<u32>,
    pub(super) plan: RxPlan,
    /// (channel slot, RX channel) for each RX channel routed here;
    /// allocated for every RX channel, so a rebuild never allocates.
    pub(super) targets: Vec<(usize, usize)>,
    /// Where the flow's audio comes from: the keepalives' destination.
    pub(super) source: Option<SocketAddrV4>,
    /// When its last packet came (the thread's clock, ns).
    pub(super) heard: Option<u64>,
    pub(super) next_keepalive: Option<u64>,
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
            heard: None,
            next_keepalive: None,
            end: None,
            holes: Holes::default(),
        }
    }
}

/// Holes given up as lost, counted as gaps of their frames.
pub(super) fn count_lost(stats: &RxStats, lost: Lost) {
    stats.gaps.fetch_add(lost.holes, Ordering::Relaxed);
    stats.gap_frames.fetch_add(lost.frames, Ordering::Relaxed);
}
