// SPDX-License-Identifier: MIT OR Apache-2.0

//! The TX slots' refusal flags (dstack #195): a send the NIC refused
//! ([`super::nic_refusal`]) raises its slot's flag on the TX thread, and
//! the control side takes the flags each tick for the traffic recorder's
//! dump ([`crate::traffic::recorder`]).

use std::sync::atomic::Ordering;

use super::{LaneStats, TxShared};

impl LaneStats {
    /// The NIC refused a send of this slot: the flag only, no allocation
    /// or lock (the TX thread's hot path).
    pub(super) fn note_refused(&self) {
        self.refused.store(true, Ordering::Relaxed);
    }
}

impl TxShared {
    /// The slots (1-based, as the logs name them) whose sends the NIC
    /// refused since the last call ([`super::nic_refusal`], dstack #195);
    /// each flag cleared. The control side's.
    pub fn take_refused(&self) -> Vec<usize> {
        self.lanes
            .iter()
            .enumerate()
            .filter(|(_, l)| l.refused.swap(false, Ordering::Relaxed))
            .map(|(slot, _)| slot + 1)
            .collect()
    }

    /// Raises slot `slot`'s refusal flag, as a send the NIC refused does
    /// (other modules' tests: the traffic recorder's dump, dstack #195).
    #[cfg(test)]
    pub(crate) fn note_refused(&self, slot: usize) {
        self.lanes[slot].note_refused();
    }
}
