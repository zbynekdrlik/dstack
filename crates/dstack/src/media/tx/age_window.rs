// SPDX-License-Identifier: MIT OR Apache-2.0

//! Each TX lane's oldest packet over the last 10 s (dstack #500, dasio#237's
//! readiness line): a figure any number of readers look at without taking
//! it.
//!
//! The late watch takes each lane's age peak once a second
//! (`TxShared::take_age_peaks`) and writes the taken peaks into a fixed
//! ring of the last [`AGE_WINDOW_LOOKS`] looks; [`TxShared::tx_age_view`]
//! only loads that ring, so a second reader never steals the late watch's
//! second, and two readers see the same window (the rule of
//! `.claude/rules/rx-counters.md` for a figure several readers look at,
//! dstack #342).
//!
//! A device without a late watch (a `TxShared` built alone, with no daemon
//! `Driver` ticking it) never writes the ring: its view reads
//! `age_max_10s_ns: None`, never a 0 that would claim "no old packet".

use std::sync::atomic::{AtomicU64, Ordering};

use super::{MAX_FLOWS, TxShared};

/// The looks the window holds: the late watch looks once a second
/// ([`crate::media::late::EVERY_NS`]), so 10 s.
pub const AGE_WINDOW_LOOKS: usize = 10;

/// The last [`AGE_WINDOW_LOOKS`] taken peaks of every slot, ns, and the
/// looks written since the device started. One writer (whoever takes the
/// peaks); readers only load.
#[derive(Debug, Default)]
pub(super) struct AgeWindow {
    peaks: [[AtomicU64; AGE_WINDOW_LOOKS]; MAX_FLOWS],
    looks: AtomicU64,
}

impl AgeWindow {
    /// One look's taken peaks, over the oldest look in the ring. No
    /// allocation (`tests/hot_path.rs`).
    fn record(&self, taken: &[u64; MAX_FLOWS]) {
        let looks = self.looks.load(Ordering::Relaxed);
        // The ring's index: below AGE_WINDOW_LOOKS, so the cast is exact.
        let at = (looks % AGE_WINDOW_LOOKS as u64) as usize;
        for (cells, &ns) in self.peaks.iter().zip(taken) {
            cells[at].store(ns, Ordering::Relaxed);
        }
        self.looks.store(looks.wrapping_add(1), Ordering::Release);
    }

    /// Slot `slot`'s oldest packet over the window, ns; `None` before the
    /// first look. A read during a write may see the look being replaced
    /// or its successor: one look off at the window's far edge, never a
    /// value no look took.
    fn max(&self, slot: usize) -> Option<u64> {
        if self.looks.load(Ordering::Acquire) == 0 {
            return None;
        }
        let cells = self.peaks.get(slot)?;
        Some(
            cells
                .iter()
                .map(|c| c.load(Ordering::Relaxed))
                .max()
                .unwrap_or(0),
        )
    }

    /// The looks written since the device started.
    fn looks(&self) -> u64 {
        self.looks.load(Ordering::Acquire)
    }
}

/// One TX lane's age figures for an app's status ([`TxShared::tx_age_view`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxAgeLane {
    /// The lane's slot (from 0, below [`MAX_FLOWS`]).
    pub slot: usize,
    /// The receiving device's name; empty for a slot sending to no one
    /// receiver now (no flow, an expired one, or a multicast flow: a group
    /// has no one receiver).
    pub receiver: String,
    /// The latency that receiver states, ns (0: unknown, or no one
    /// receiver), read as [`TxShared::take_app_view`] reads it.
    pub receiver_latency_ns: u64,
    /// The oldest packet the lane sent over the last [`AGE_WINDOW_LOOKS`]
    /// looks of the late watch (10 s), ns past its stamp: 0 when it sent
    /// none. `None`: no late watch ever looked (a device without one), so
    /// there is no window to read. The watch's first look covers the time
    /// from the device's start to it, not one second.
    pub age_max_10s_ns: Option<u64>,
    /// The lane's packets over their receiver's latency since the device
    /// started ([`super::LaneCounts::over_latency`]).
    pub over_latency: u64,
}

impl TxShared {
    /// Every slot's oldest packet since the last call, taken (as
    /// [`TxShared::take_age_peak`] on each), and written into the 10 s
    /// window [`TxShared::tx_age_view`] reads. The late watch's once a
    /// second ([`crate::media::late::LateWatch::watch`]); one caller per
    /// device, at a steady second, or the window is not 10 s: crate-only, so
    /// no app can take the late watch's peaks or skew the window. No
    /// allocation. [`TxShared::take_age_peak`] (one slot) does not feed it.
    pub(crate) fn take_age_peaks(&self) -> [u64; MAX_FLOWS] {
        let taken = std::array::from_fn(|s| self.take_age_peak(s));
        self.age_window.record(&taken);
        taken
    }

    /// The looks `TxShared::take_age_peaks` wrote since the device
    /// started (0: no late watch): a reader that sees it stand still
    /// across two views knows the window is no longer fed.
    pub fn tx_age_looks(&self) -> u64 {
        self.age_window.looks()
    }

    /// Each TX lane (every slot, below [`MAX_FLOWS`]) with its receiver,
    /// that receiver's latency, the lane's oldest packet over the last 10 s
    /// and its lifetime late count (dasio#237, dstack #500). It takes
    /// nothing: any number of readers may call it and see the same window.
    /// It locks the table: call it from a status loop, never the pump or
    /// the TX thread.
    pub fn tx_age_view(&self) -> Vec<TxAgeLane> {
        // The receivers as `take_app_view` has them: one filter for both.
        let mut flows: [Option<(String, std::net::SocketAddrV4)>; MAX_FLOWS] =
            std::array::from_fn(|_| None);
        for (slot, dest, receiver) in self.receiver_flows() {
            if let Some(f) = flows.get_mut(slot) {
                *f = Some((receiver, dest));
            }
        }
        flows
            .into_iter()
            .enumerate()
            .map(|(slot, flow)| {
                let (receiver, receiver_latency_ns) = match flow {
                    Some((host, dest)) => (host, self.receiver_latency_ns(slot, dest)),
                    None => (String::new(), 0),
                };
                TxAgeLane {
                    slot,
                    receiver,
                    receiver_latency_ns,
                    age_max_10s_ns: self.age_window.max(slot),
                    over_latency: self.lane(slot).over_latency,
                }
            })
            .collect()
    }
}
