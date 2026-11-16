// SPDX-License-Identifier: MIT OR Apache-2.0

//! Each TX slot's receiver latency (dstack #1): the latency the receiver
//! states ([`crate::flows::receivers`] reads it), kept per slot with the
//! destination it was read for, so the TX thread counts lateness against
//! it only while the slot still sends there.

use std::net::SocketAddrV4;
use std::sync::atomic::Ordering;

use super::{Plan, TxShared};

/// One flow's receiver and how late it judged our packets, for an app's
/// status ([`TxShared::take_app_view`], dstack #325).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiverLateness {
    /// The flow's slot.
    pub slot: usize,
    /// The receiving device's name.
    pub receiver: String,
    /// The latency it states, ns (0: unknown).
    pub latency_ns: u64,
    /// The slot's packets over that latency since the device started
    /// ([`super::LaneCounts::over_latency`]).
    pub over_latency: u64,
}

/// What an app's status loop reads of the TX side ([`TxShared::take_app_view`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppView {
    /// The oldest packet of any slot since the app's last view, ns past its
    /// stamp (0: none sent).
    pub age_peak_ns: u64,
    /// Each flow sending now to a receiver.
    pub receivers: Vec<ReceiverLateness>,
}

impl TxShared {
    /// Slot `slot`'s receiver at `dest` states its latency as `ns` (0:
    /// unknown); the TX thread counts the slot's late packets against it
    /// from its next pass while the slot still sends to `dest`. The value
    /// stands until the next one is set: the reader keeps the last one
    /// through a failed read, so a receiver that changed its latency and
    /// then stopped answering keeps its old one until a read succeeds.
    pub fn set_receiver_latency(&self, slot: usize, dest: SocketAddrV4, ns: u64) {
        let _writer = self.lock();
        if let Some(cell) = self.receivers.get(slot) {
            cell.write([dest_word(dest), ns]);
        }
    }

    /// Slot `slot`'s receiver latency, ns, when the receiver at `dest`
    /// stated it; 0: unknown (never read, or read for another
    /// destination). No lock: the TX thread's.
    pub fn receiver_latency_ns(&self, slot: usize, dest: SocketAddrV4) -> u64 {
        match self.receivers.get(slot).and_then(|c| c.read()) {
            Some((_, [word, ns])) if word == dest_word(dest) => ns,
            _ => 0,
        }
    }

    /// Slot `slot`'s receiver as its published plan has it.
    pub(super) fn plan_dest(&self, slot: usize) -> Option<SocketAddrV4> {
        Plan::from_words(self.plans.get(slot)?.read()?.1).dest
    }

    /// The flows sending now to a receiver (not expired, not multicast:
    /// a group has no device to read): each one's slot, receiver and
    /// receiving device's name, for the receiver latency reader.
    pub fn receiver_flows(&self) -> Vec<(usize, SocketAddrV4, String)> {
        self.lock()
            .table
            .flows()
            .filter(|(_, f)| !f.expired && !f.multicast)
            .map(|(slot, f)| (slot, f.dest, f.rx_host.clone()))
            .collect()
    }

    /// What an app's status loop reads of the TX side (dstack #325, the
    /// ASIO bridge's receiver figure): the oldest packet of any slot since
    /// its last call, taken (the app's own peak, never the late watch's),
    /// and each flow sending now to a receiver (as
    /// [`TxShared::receiver_flows`]) with its stated latency and its slot's
    /// late count. It locks the table: call it from a status loop, never
    /// the pump or the TX thread. One reader per device: a second caller
    /// would take the first's peak.
    pub fn take_app_view(&self) -> AppView {
        let age_peak_ns = self
            .lanes
            .iter()
            .map(|l| l.age_app_peak_ns.swap(0, Ordering::Relaxed))
            .max()
            .unwrap_or(0);
        let receivers = self
            .receiver_flows()
            .into_iter()
            .map(|(slot, dest, receiver)| ReceiverLateness {
                slot,
                receiver,
                latency_ns: self.receiver_latency_ns(slot, dest),
                over_latency: self.lane(slot).over_latency,
            })
            .collect();
        AppView {
            age_peak_ns,
            receivers,
        }
    }
}

/// `dest` as one word for [`TxShared::receiver_latency_ns`]: its address,
/// port and a 1, never 0 (an unwritten cell).
fn dest_word(dest: SocketAddrV4) -> u64 {
    let [a, b, c, d] = dest.ip().octets();
    let [p, q] = dest.port().to_be_bytes();
    u64::from_be_bytes([a, b, c, d, p, q, 0, 1])
}
