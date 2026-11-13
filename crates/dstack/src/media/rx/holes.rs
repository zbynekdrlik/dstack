// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The open holes of an RX flow (dstack #278): runs of frames between the
//! packets come so far that no packet has filled yet.
//!
//! Dante packets carry no sequence number; their stamps are the sequence.
//! A packet that begins past the end of the furthest one so far leaves a
//! hole. That is not yet a loss: a link that reorders (a USB NIC, 13.11.2026
//! on stream-snv: about 3000 reorders in 1.8 M packets, the late packet
//! overtaken by at most 6 others, none missing) brings the overtaken packet
//! next. A packet that lands in a hole fills it (trimmed, split or
//! removed) and is a reorder. A hole is lost only once it is wholly behind
//! the playout point ([`Holes::expire`]), when it is the oldest of a full
//! set ([`HOLES`]) and another opens, or when the flow ends
//! ([`Holes::clear`]).
//!
//! The RX thread owns it: a fixed array, no allocation, no atomics.

/// The most open holes a flow keeps; opening one more loses the oldest.
pub const HOLES: usize = 16;

/// Holes given up as lost, and their frames.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Lost {
    pub holes: u64,
    pub frames: u64,
}

impl Lost {
    fn add(&mut self, (start, end): (u64, u64)) {
        self.holes += 1;
        self.frames += end - start;
    }
}

/// A flow's open holes, each `start..end` in samples, oldest first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Holes {
    open: [(u64, u64); HOLES],
    len: usize,
}

impl Holes {
    /// The open holes, oldest first.
    pub fn open_holes(&self) -> &[(u64, u64)] {
        &self.open[..self.len]
    }

    /// Opens the hole `start..end` (an empty one is none). With
    /// [`HOLES`] open already, the oldest is lost to make room.
    pub fn open(&mut self, start: u64, end: u64) -> Lost {
        let mut lost = Lost::default();
        if start < end {
            self.insert(self.len, (start, end), &mut lost);
        }
        lost
    }

    /// A packet of `start..end` landed: whatever part of an open hole it
    /// covers is filled. Whether it filled any frame (a reorder), and the
    /// oldest hole when a split needed its room.
    pub fn fill(&mut self, start: u64, end: u64) -> (bool, Lost) {
        let mut lost = Lost::default();
        let mut filled = false;
        let mut i = 0;
        while i < self.len {
            let (hs, he) = self.open[i];
            if start >= he || end <= hs {
                i += 1;
                continue;
            }
            filled = true;
            match (hs < start, end < he) {
                (false, false) => self.remove(i),
                (true, false) => {
                    self.open[i].1 = start;
                    i += 1;
                }
                (false, true) => {
                    self.open[i].0 = end;
                    i += 1;
                }
                // Inside this hole alone (holes never overlap): split it.
                (true, true) => {
                    self.open[i].1 = start;
                    self.insert(i + 1, (end, he), &mut lost);
                    break;
                }
            }
        }
        (filled, lost)
    }

    /// Every hole that ends at or before `before` (the playout point: the
    /// flow's now less its latency and block) is lost.
    pub fn expire(&mut self, before: u64) -> Lost {
        let mut lost = Lost::default();
        let mut i = 0;
        while i < self.len {
            if self.open[i].1 <= before {
                lost.add(self.open[i]);
                self.remove(i);
            } else {
                i += 1;
            }
        }
        lost
    }

    /// The flow ended: every open hole is lost.
    pub fn clear(&mut self) -> Lost {
        let mut lost = Lost::default();
        for &h in self.open_holes() {
            lost.add(h);
        }
        self.len = 0;
        lost
    }

    fn remove(&mut self, i: usize) {
        self.open.copy_within(i + 1..self.len, i);
        self.len -= 1;
    }

    /// Puts `hole` at index `at` (at least 1 when the set is full: the
    /// callers insert after a hole or at the end); with the set full the oldest
    /// is lost first.
    fn insert(&mut self, at: usize, hole: (u64, u64), lost: &mut Lost) {
        let mut at = at;
        if self.len == HOLES {
            lost.add(self.open[0]);
            self.remove(0);
            at -= 1;
        }
        self.open.copy_within(at..self.len, at + 1);
        self.open[at] = hole;
        self.len += 1;
    }
}

/// What a packet did to its flow's holes ([`place`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Placed {
    /// It began before the end of the furthest packet so far.
    pub out_of_order: bool,
    /// It filled (part of) an open hole in time: overtaken, not lost.
    pub reordered: bool,
    /// Holes given up to make room.
    pub lost: Lost,
}

/// A packet of `start..end` (samples) arrived, `in_time` when it was not
/// past its flow's playout plus block (not `late`); `furthest` is the end
/// of the furthest packet so far, moved on by it. A jump past `furthest`
/// opens a hole; a packet before it fills what it covers, a reorder only
/// in time (a late one's frames were read already: lost, counted `late`).
/// The flow's first packet opens nothing.
pub fn place(
    furthest: &mut Option<u64>,
    holes: &mut Holes,
    start: u64,
    end: u64,
    in_time: bool,
) -> Placed {
    let mut placed = Placed::default();
    match *furthest {
        Some(e) if start < e => {
            placed.out_of_order = true;
            let (filled, lost) = holes.fill(start, end);
            placed.reordered = filled && in_time;
            placed.lost = lost;
            *furthest = Some(e.max(end));
        }
        Some(e) => {
            placed.lost = holes.open(e, start);
            *furthest = Some(end);
        }
        None => *furthest = Some(end),
    }
    placed
}

#[cfg(test)]
mod tests;
