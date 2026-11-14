// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The RX multicast flows' groups joined again after an interface change
//! (dstack #282). The daemon's tick keeps the interface watch
//! ([`crate::membership::Membership`]) and asks; the RX thread acts at its
//! next pass (one atomic load a pass otherwise) and reports a join that
//! failed, so the daemon asks again.

use std::sync::atomic::Ordering;

use super::{Receiver, RxShared};

impl RxShared {
    /// Our address's interface changed: the RX thread's next pass joins
    /// every slot's group again on it.
    pub fn rejoin_groups(&self) {
        self.rejoins.fetch_add(1, Ordering::Release);
    }

    /// Whether the RX thread, acting on the last [`RxShared::rejoin_groups`],
    /// left a slot's group unjoined (its join failed); told once, so the
    /// daemon asks again at its next look.
    pub fn take_groups_missing(&self) -> bool {
        self.groups_missing.swap(false, Ordering::AcqRel)
    }
}

impl Receiver {
    /// A rejoin asked since the last pass: the slots whose plan names a
    /// group, one bit each; `None` when none was asked.
    pub(super) fn rejoins_asked(&mut self) -> Option<u64> {
        let asked = self.shared.rejoins.load(Ordering::Acquire);
        if asked == self.rejoins_seen {
            return None;
        }
        self.rejoins_seen = asked;
        // Distinct bits: a sum (an `|` would leave its `^` mutant).
        Some(
            self.lanes
                .iter()
                .enumerate()
                .filter(|(_, lane)| lane.plan.group.is_some())
                .map(|(slot, _)| 1u64 << slot)
                .sum(),
        )
    }

    /// After acting on a rejoin: whether a slot's group stayed unjoined,
    /// for [`RxShared::take_groups_missing`].
    pub(super) fn tell_missing(&self) {
        let missing = self
            .lanes
            .iter()
            .zip(&self.joined)
            .any(|(lane, joined)| lane.plan.group.is_some() && joined.is_none());
        self.shared.groups_missing.store(missing, Ordering::Release);
    }
}
