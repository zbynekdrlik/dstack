// SPDX-License-Identifier: MIT OR Apache-2.0

//! Each TX slot's receiver latency (dstack #1): the latency the receiver
//! states ([`crate::flows::receivers`] reads it), kept per slot with the
//! destination it was read for, so the TX thread counts lateness against
//! it only while the slot still sends there.

use std::net::SocketAddrV4;

use super::{Plan, TxShared};

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

    /// The flows sending now (not expired): each one's slot, receiver and
    /// receiving device's name, for the receiver latency reader.
    pub fn receiver_flows(&self) -> Vec<(usize, SocketAddrV4, String)> {
        self.lock()
            .table
            .flows()
            .filter(|(_, f)| !f.expired)
            .map(|(slot, f)| (slot, f.dest, f.rx_host.clone()))
            .collect()
    }
}

/// `dest` as one word for [`TxShared::receiver_latency_ns`]: its address,
/// port and a 1, never 0 (an unwritten cell).
fn dest_word(dest: SocketAddrV4) -> u64 {
    let [a, b, c, d] = dest.ip().octets();
    let [p, q] = dest.port().to_be_bytes();
    u64::from_be_bytes([a, b, c, d, p, q, 0, 1])
}
