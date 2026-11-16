// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The joined multicast sockets as the ring writer reads them (dstack
//! #326). The RX thread opens and closes a slot's group socket (a join is
//! a system call and an allocation, never the pump's), and the pump's
//! drain ([`super::Intake::drain_into_rings`]) reads it by its OS handle,
//! one atomic word a slot (and the group it is of), with no lock.
//!
//! A socket the RX thread replaces is closed only once the writer has
//! acknowledged a drain that began after the swap ([`Handles::released`]):
//! a writer still reading the old handle never reads a closed one, nor a
//! handle number the OS gave to another socket meanwhile.

use std::sync::atomic::{AtomicU64, Ordering};

use super::MAX_FLOWS;

/// Each flow slot's group socket handle (`socket::handle_of`, 0: none)
/// and the group it is of, the swaps' count and the writer's
/// acknowledgement.
#[derive(Debug)]
pub(super) struct Handles {
    slots: Box<[AtomicU64]>,
    /// Each slot's handle's group, as `counts::source_word` packs an
    /// address (0: none); stored before the handle.
    groups: Box<[AtomicU64]>,
    /// Moves once per [`Handles::set`], after the handle is stored.
    moves: AtomicU64,
    /// The [`Handles::moves`] the writer read at the start of its last
    /// finished drain.
    acked: AtomicU64,
}

impl Handles {
    pub(super) fn new() -> Self {
        Handles {
            slots: (0..MAX_FLOWS).map(|_| AtomicU64::new(0)).collect(),
            groups: (0..MAX_FLOWS).map(|_| AtomicU64::new(0)).collect(),
            moves: AtomicU64::new(0),
            acked: AtomicU64::new(0),
        }
    }

    /// The RX thread: slot `slot`'s group socket is `handle` now (0: none),
    /// of the group `group` (its packed word, 0: none). Returns the swap's
    /// number: the socket it replaced may close once [`Handles::released`]
    /// says so. The group is stored first, so a writer that reads the new
    /// handle reads its group too (one that reads the old handle with the
    /// new group sees them disagree and reads neither).
    pub(super) fn set(&self, slot: usize, handle: u64, group: u64) -> u64 {
        if let (Some(s), Some(g)) = (self.slots.get(slot), self.groups.get(slot)) {
            g.store(group, Ordering::Release);
            s.store(handle, Ordering::Release);
        }
        self.moves.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Slot `slot`'s group socket handle (0: none) and the group it is of.
    pub(super) fn get(&self, slot: usize) -> (u64, u64) {
        match (self.slots.get(slot), self.groups.get(slot)) {
            (Some(s), Some(g)) => {
                let handle = s.load(Ordering::Acquire);
                (handle, g.load(Ordering::Acquire))
            }
            _ => (0, 0),
        }
    }

    /// The writer, before a drain reads any handle: the swaps so far.
    pub(super) fn moves(&self) -> u64 {
        self.moves.load(Ordering::Acquire)
    }

    /// The writer, after that drain: it read no handle older than `moves`.
    pub(super) fn ack(&self, moves: u64) {
        self.acked.store(moves, Ordering::Release);
    }

    /// Whether the socket that swap number `moved` replaced may close: the
    /// writer finished a drain that began after it.
    pub(super) fn released(&self, moved: u64) -> bool {
        self.acked.load(Ordering::Acquire) >= moved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replaced_socket_is_released_only_after_a_later_drain() {
        let h = Handles::new();
        let before = h.moves();
        assert_eq!(h.get(3), (0, 0));
        let moved = h.set(3, 42, 9);
        assert_eq!(h.get(3), (42, 9));
        assert_eq!(h.moves(), moved);
        assert!(!h.released(moved), "no drain yet");
        // A drain that began before the swap may still read the old one.
        h.ack(before);
        assert!(!h.released(moved));
        let began = h.moves();
        h.ack(began);
        assert!(h.released(moved));
        // The next swap counts on.
        let next = h.set(3, 0, 0);
        assert_eq!(next, moved + 1);
        assert_eq!(h.get(3), (0, 0));
        assert!(!h.released(next));
        // No such slot: nothing stored, the swap still counted.
        assert_eq!(h.set(MAX_FLOWS, 7, 7), next + 1);
        assert_eq!(h.get(MAX_FLOWS), (0, 0));
    }
}
