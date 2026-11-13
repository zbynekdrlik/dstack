// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! A flow slot's counts: the atomics the RX thread writes ([`RxStats`]) and
//! the copy the control side reads ([`RxCounts`]).

use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::atomic::AtomicU64;

use super::netlat;

/// A least lateness that holds none: no packet placed since it was last
/// taken. A lateness of `u64::MAX` ns (585 years) reads as none too.
pub(super) const NO_LATENESS: u64 = u64::MAX;

/// One flow slot's counts, written by the RX thread.
#[derive(Debug, Default)]
pub(super) struct RxStats {
    pub(super) packets: AtomicU64,
    pub(super) refused: AtomicU64,
    pub(super) stray: AtomicU64,
    pub(super) out_of_order: AtomicU64,
    /// Packets that filled an open hole (dstack #278).
    pub(super) reordered: AtomicU64,
    pub(super) gaps: AtomicU64,
    pub(super) gap_frames: AtomicU64,
    pub(super) late: AtomicU64,
    pub(super) late_at_read: AtomicU64,
    /// Packets already late when the kernel received them.
    pub(super) wire_late: AtomicU64,
    pub(super) unplaced: AtomicU64,
    pub(super) keepalives: AtomicU64,
    pub(super) keepalive_errors: AtomicU64,
    pub(super) recv_errors: AtomicU64,
    /// Multicast groups joined, and joins that failed.
    pub(super) joins: AtomicU64,
    pub(super) join_errors: AtomicU64,
    pub(super) max_lateness: AtomicU64,
    /// The largest lateness since the device started, samples.
    pub(super) lateness_peak: AtomicU64,
    /// What the last [`RxShared::take_max_lateness`] took.
    pub(super) last_lateness: AtomicU64,
    /// The largest wake delay since the last take, ns, what the last take
    /// took, and the largest since the device started.
    pub(super) max_wake_ns: AtomicU64,
    pub(super) last_wake_ns: AtomicU64,
    pub(super) wake_peak_ns: AtomicU64,
    /// The packets' judge: the least lateness in ns since the last take.
    pub(super) min_lateness_ns: Least,
    /// The measured network latency's window (dstack #244, Linux).
    pub(super) network: netlat::Window,
    /// What the last [`RxShared::take_max_lateness`] took of it.
    pub(super) last_min_lateness_ns: Least,
    /// Where the flow's audio comes from, as [`source_word`] packs it (0:
    /// none yet): the RX thread writes it only when it changes (dstack
    /// #266), the status and the loss watch read it.
    pub(super) source: AtomicU64,
}

/// `source` packed in one word for [`RxStats::source`]: a set byte, the
/// IPv4 address and the port; 0 for none.
pub(super) fn source_word(source: Option<SocketAddrV4>) -> u64 {
    source.map_or(0, |s| {
        let [a, b, c, d] = s.ip().octets();
        let [p0, p1] = s.port().to_be_bytes();
        u64::from_be_bytes([0, 1, a, b, c, d, p0, p1])
    })
}

/// The address a [`source_word`] holds.
pub(super) fn source_of(word: u64) -> Option<SocketAddrV4> {
    let [_, set, a, b, c, d, p0, p1] = word.to_be_bytes();
    (set == 1).then(|| SocketAddrV4::new(Ipv4Addr::new(a, b, c, d), u16::from_be_bytes([p0, p1])))
}

/// An atomic least, ns: [`NO_LATENESS`] until a value is put in
/// (`fetch_min`, one per placed packet).
#[derive(Debug)]
pub(super) struct Least(pub(super) AtomicU64);

impl Default for Least {
    fn default() -> Self {
        Least(AtomicU64::new(NO_LATENESS))
    }
}

/// One flow slot's counts since the device started (every flow it held).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RxCounts {
    /// Well-formed packets from the flow's transmitter.
    pub packets: u64,
    /// Datagrams from the transmitter that were no packet of the flow
    /// (shorter than the header, longer than a datagram, no whole frame).
    pub refused: u64,
    /// Datagrams from another address, or to a slot without a flow.
    pub stray: u64,
    /// Packets that began before the end of an earlier one: the reordered
    /// ones and any repeat or overlap.
    pub out_of_order: u64,
    /// Packets that filled (part of) an open hole: overtaken on the way,
    /// not lost (dstack #278).
    pub reordered: u64,
    /// Runs of frames that never came (Dante packets have no sequence
    /// number; the stamps are it): a hole past the end of the furthest
    /// packet that no packet filled before it was wholly behind the
    /// playout point (the flow's latency plus the block), before 16 newer
    /// holes opened, or before the slot's flow changed (dstack #278; until
    /// v0.6.124 every jump counted at once, a reorder too).
    pub gaps: u64,
    /// The frames of those runs.
    pub gap_frames: u64,
    /// Packets whose first frame was later than the flow's playout plus
    /// the pump's block: the most a block's read can wait.
    pub late: u64,
    /// Packets with a frame the pump had already read (a ring refused it
    /// late): what its channels count as holes and late samples. A packet
    /// past the playout but within the block counts here, never in `late`
    /// (iemmixer, 5.11.2026).
    pub late_at_read: u64,
    /// Packets whose first frame was already later than `late`'s limit
    /// when the kernel received them (dstack #234, Linux; 0 on Windows):
    /// late on the wire, not by a late RX-thread wake.
    pub wire_late: u64,
    /// The largest lateness of any packet since the device started,
    /// samples (`lateness` is the last second's only).
    pub lateness_peak: u64,
    /// The largest wake delay of any packet since the device started, µs:
    /// the RX pass's time less the kernel's receive time (dstack #234,
    /// Linux; 0 on Windows).
    pub wake_delay_peak_us: u64,
    /// Packets read before our clock was ready.
    pub unplaced: u64,
    pub keepalives: u64,
    pub keepalive_errors: u64,
    pub recv_errors: u64,
    /// Multicast groups the slot joined (each a new socket), and the joins
    /// that failed (the socket, the bind or the membership).
    pub joins: u64,
    pub join_errors: u64,
}
