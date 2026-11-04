// SPDX-License-Identifier: MIT OR Apache-2.0

//! The TX pass through a host's frame sender (dstack #1, Windows): the
//! application's own kernel send path, handed to the device before its
//! threads start ([`super::TxShared::use_frame_sender`]). On the FOH PC
//! on-time sends waited up to 6 ms below Winsock, while dasio's NDIS filter
//! sent the same stream with 0 losses.
//!
//! A pass gathers every due packet of a flow whose receiver the neighbour
//! table resolved as a whole Ethernet II frame (`write_frame`) in a
//! preallocated `Batch`, and hands the batch to the sender in one call at
//! the end of the pass (a pass with more than [`BATCH_FRAMES`] such packets
//! sends a full batch first). A frame the sender did not take, or every
//! frame of a batch it refused whole, goes out through the slot's UDP
//! socket with the usual retries and counts; a frame it took counts in the
//! lane's `direct`. Multicast and unresolved receivers never enter the
//! batch: they go out through the UDP socket at once, as without a sender.

use std::fmt;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use super::{Control, DSCP_EF, MAX_FLOWS, PassClock, Sent, TxShared, account, nanos, send_timed};
use crate::media::bypass::{self, ETHERNET_HEADER, FRAME_HEADROOM, FrameSender};
use crate::wire::MAX_DATAGRAM;

/// The most frames of one call to the host sender: two packets of every
/// flow (a pass after a short stall sends two of a flow).
pub const BATCH_FRAMES: usize = 2 * MAX_FLOWS;

/// The largest frame of a media packet: its headers and a whole datagram.
pub const FRAME_MAX: usize = FRAME_HEADROOM + MAX_DATAGRAM;

/// Our NIC, as the host sender's frames name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostNic {
    /// Its interface index: the neighbour table's entries on it count.
    pub ifindex: u32,
    /// Its MAC: the frames' source.
    pub mac: [u8; 6],
    /// Its name, for the start line.
    pub name: String,
}

/// The host's sender and our NIC.
pub(super) struct HostPath {
    pub(super) sender: Arc<dyn FrameSender>,
    pub(super) nic: HostNic,
}

impl fmt::Debug for HostPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostPath").field("nic", &self.nic).finish()
    }
}

/// The path a device's TX takes (the start line names it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxPath {
    /// The host's frame sender (Windows, dstack #1).
    HostSender,
    /// An `AF_PACKET` socket past the qdisc (Linux, [`bypass`]).
    AfPacket,
    /// The UDP sockets alone.
    UdpSocket,
}

impl TxPath {
    /// The path in words.
    pub fn describe(self) -> &'static str {
        match self {
            TxPath::HostSender => "host frame sender",
            TxPath::AfPacket => "AF_PACKET past the qdisc",
            TxPath::UdpSocket => "UDP socket",
        }
    }
}

/// The TX thread's start line: the `path` on the interface `nic`.
/// Multicast and unresolved receivers go through the UDP socket on every
/// path.
pub(crate) fn path_line(path: TxPath, nic: &str) -> String {
    format!(
        "TX path: {} on {nic} (multicast and unresolved receivers on the UDP socket)",
        path.describe()
    )
}

/// Writes the headers of a frame whose media packet of `n` bytes is at
/// `buf[FRAME_HEADROOM..]`: Ethernet II from our NIC's `src_mac` to
/// `dst_mac`, then IPv4 (DSCP EF, IPv4 ID `id`) and UDP from `src` to
/// `dest`. Returns the frame's length.
pub(crate) fn write_frame(
    buf: &mut [u8],
    dst_mac: [u8; 6],
    src_mac: [u8; 6],
    src: SocketAddrV4,
    dest: SocketAddrV4,
    id: u16,
    n: usize,
) -> usize {
    bypass::write_ethernet(buf, dst_mac, src_mac);
    ETHERNET_HEADER
        + bypass::write_headers(&mut buf[ETHERNET_HEADER..], src, dest, DSCP_EF << 2, id, n)
}

impl TxShared {
    /// Each slot's MAC for the host sender from the Windows neighbour
    /// `rows` on our NIC ([`bypass::neighbour_mac_words`]); nothing without
    /// a host sender. A cell is written only when its MAC changed.
    pub(super) fn set_host_macs(&self, c: &Control, rows: &[bypass::Neighbour]) {
        let Some(host) = self.host.get() else {
            return;
        };
        let dests = (0..MAX_FLOWS).map(|slot| c.table.plan(slot).dest);
        let words = bypass::neighbour_mac_words(rows, host.nic.ifindex, dests);
        for (cell, w) in self.macs.iter().zip(words) {
            if cell.peek()[0] != w {
                cell.write([w]);
            }
        }
    }

    /// The neighbour table is `rows` now: each slot's MAC for the host
    /// sender follows it (the control tick reads the table on Windows once
    /// a second; nothing without a host sender).
    pub fn set_neighbours(&self, rows: &[bypass::Neighbour]) {
        let c = self.lock();
        self.set_host_macs(&c, rows);
    }

    /// Sends this device's TX through `sender` (dstack #1) from our NIC
    /// `nic`: once, before the TX thread starts (a later call is refused,
    /// `false`). `Media::set_frame_sender` finds the NIC on Windows; on its
    /// own this works on any OS, so tests drive the host path on Linux.
    pub fn use_frame_sender(&self, sender: Arc<dyn FrameSender>, nic: HostNic) -> bool {
        // Under the lock the TX thread's state is built in: a sender is
        // either seen by it or refused, never set unused.
        let _built = self.lock();
        !self.tx_built.load(Ordering::Relaxed) && self.host.set(HostPath { sender, nic }).is_ok()
    }

    /// The host path for the TX thread's state being built now; from then
    /// on [`TxShared::use_frame_sender`] refuses a sender.
    pub(super) fn host_for_tx(&self) -> Option<&HostPath> {
        let _built = self.lock();
        self.tx_built.store(true, Ordering::Relaxed);
        self.host.get()
    }

    /// The path our TX takes: the host's sender, AF_PACKET, or the UDP
    /// sockets (unicast flows to unresolved neighbours and multicast go
    /// through the UDP sockets on every path).
    pub fn tx_path(&self) -> TxPath {
        if self.host.get().is_some() {
            TxPath::HostSender
        } else if self.direct.is_some() {
            TxPath::AfPacket
        } else {
            TxPath::UdpSocket
        }
    }

    /// The interface the TX path names in the start line: the host
    /// sender's NIC, the direct socket's, else the device's address.
    pub(super) fn tx_nic(&self) -> String {
        match (self.host.get(), &self.direct) {
            (Some(h), _) => h.nic.name.clone(),
            (None, Some((_, nic))) => nic.clone(),
            (None, None) => self.ip.to_string(),
        }
    }

    /// Batches the host sender refused whole since the device started,
    /// and the last one's raw OS error (0 for none, -1 for an error without
    /// one).
    pub fn host_refusals(&self) -> (u64, i32) {
        (
            self.host_refused.load(Ordering::Relaxed),
            self.host_error.load(Ordering::Relaxed),
        )
    }

    /// Slot `slot`'s destination MAC for the host sender, when there is
    /// one and the neighbour table resolved the slot's receiver.
    pub(super) fn host_mac(&self, slot: usize) -> Option<[u8; 6]> {
        self.host.get()?;
        self.slot_mac(slot)
    }
}

/// One packet of a batch, as the pass gave it.
#[derive(Debug, Clone, Copy)]
pub(super) struct Queued {
    pub(super) slot: usize,
    pub(super) stamp: u64,
    pub(super) fpp: u16,
    /// Our slot's address and the receiver's.
    pub(super) src: SocketAddrV4,
    pub(super) dest: SocketAddrV4,
    /// The receiver's MAC.
    pub(super) mac: [u8; 6],
}

impl Queued {
    const NONE: Queued = Queued {
        slot: 0,
        stamp: 0,
        fpp: 0,
        src: SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0),
        dest: SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0),
        mac: [0; 6],
    };
}

/// The frames of a pass for the host sender, allocated with the TX
/// thread's state: the hot path only fills them.
pub(super) struct Batch {
    sender: Arc<dyn FrameSender>,
    /// Our NIC's MAC.
    mac: [u8; 6],
    frames: Box<[[u8; FRAME_MAX]]>,
    /// Each frame's packet and its datagram's length.
    queued: Box<[(Queued, usize)]>,
    /// What the sender took.
    sent: Box<[bool]>,
    len: usize,
}

impl fmt::Debug for Batch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Batch")
            .field("mac", &self.mac)
            .field("len", &self.len)
            .finish()
    }
}

impl Batch {
    /// An empty batch for `host`'s sender.
    pub(super) fn new(host: &HostPath) -> Batch {
        Batch {
            sender: Arc::clone(&host.sender),
            mac: host.nic.mac,
            frames: vec![[0; FRAME_MAX]; BATCH_FRAMES].into_boxed_slice(),
            queued: vec![(Queued::NONE, 0); BATCH_FRAMES].into_boxed_slice(),
            sent: vec![false; BATCH_FRAMES].into_boxed_slice(),
            len: 0,
        }
    }

    /// Adds the packet `q` (its datagram written by `payload` into the
    /// buffer it gets, returning its length) as a frame with the IPv4 ID
    /// taken from `ip_id`. A full batch is sent first ([`Batch::flush`]).
    pub(super) fn push(
        &mut self,
        clock: &PassClock,
        shared: &TxShared,
        q: Queued,
        ip_id: &mut u16,
        payload: impl FnOnce(&mut [u8]) -> usize,
    ) {
        if self.len == BATCH_FRAMES {
            self.flush(clock, shared);
        }
        let frame = &mut self.frames[self.len];
        let n = payload(&mut frame[FRAME_HEADROOM..]);
        write_frame(frame, q.mac, self.mac, q.src, q.dest, *ip_id, n);
        *ip_id = ip_id.wrapping_add(1);
        self.queued[self.len] = (q, n);
        self.len += 1;
    }

    /// Sends the batch in one call to the host sender, then each frame it
    /// did not take (all of a batch refused whole, counted in
    /// [`TxShared::host_refusals`]) through its slot's UDP socket, with
    /// the retries; counts every packet in its lane. Nothing to send: no
    /// call.
    pub(super) fn flush(&mut self, clock: &PassClock, shared: &TxShared) {
        let len = std::mem::take(&mut self.len);
        if len == 0 {
            return;
        }
        let Batch {
            sender,
            frames,
            queued,
            sent,
            ..
        } = self;
        let mut refs: [&[u8]; BATCH_FRAMES] = [&[]; BATCH_FRAMES];
        for (r, (frame, (_, n))) in refs
            .iter_mut()
            .zip(frames.iter().zip(queued.iter()))
            .take(len)
        {
            *r = &frame[..FRAME_HEADROOM + n];
        }
        let sent = &mut sent[..len];
        sent.fill(false);
        let started = Instant::now();
        let whole = sender.send(&refs[..len], sent);
        let took = nanos(started.elapsed());
        if let Err(e) = &whole {
            // Refused whole: none taken, whatever the sender marked.
            sent.fill(false);
            shared
                .host_error
                .store(e.raw_os_error().unwrap_or(-1), Ordering::Relaxed);
            shared.host_refused.fetch_add(1, Ordering::Relaxed);
        }
        // The frames taken first: they left with the call, before any
        // fallback send.
        let taken = Sent {
            result: Ok(true),
            retries: 0,
            took,
        };
        for ((q, n), _) in queued.iter().zip(sent.iter()).filter(|(_, s)| **s) {
            let receiver_ns = shared.receiver_latency_ns(q.slot, q.dest);
            account(
                &shared.lanes[q.slot],
                clock,
                q.stamp,
                q.fpp,
                &taken,
                *n,
                receiver_ns,
            );
        }
        for ((q, n), frame) in queued
            .iter()
            .zip(frames.iter())
            .zip(sent.iter())
            .filter_map(|(qf, s)| (!*s).then_some(qf))
        {
            let datagram = &frame[FRAME_HEADROOM..FRAME_HEADROOM + n];
            let socket = &shared.sockets[q.slot];
            let out = send_timed(clock, q.stamp, q.fpp, || {
                socket.send_to(datagram, q.dest).map(|_| false)
            });
            let receiver_ns = shared.receiver_latency_ns(q.slot, q.dest);
            account(
                &shared.lanes[q.slot],
                clock,
                q.stamp,
                q.fpp,
                &out,
                *n,
                receiver_ns,
            );
        }
    }
}

#[cfg(test)]
mod tests;
