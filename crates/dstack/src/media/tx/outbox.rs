// SPDX-License-Identifier: MIT OR Apache-2.0

//! The socket sends of a frame-sender host's TX pass, sent by the relay
//! thread (dstack #485). On the FOH PC one Winsock send to an on-PC
//! receiver took ~1.4 ms (afd -> tcpip -> WFP -> Ndu -> npcap, dasio's
//! trace of 26.11.2026), and every NIC flow of that pass, handed to the
//! host's sender at the pass's end ([`super::host::Batch`]), left that
//! late against its 250 us receiver. So on such a host the pass never
//! makes a socket send itself while a relay thread serves this queue:
//!
//! - The pass writes each socket packet (an on-PC, multicast or unresolved
//!   receiver's) into a preallocated entry with its slot, stamp, receiver
//!   and the pass's clock ([`Outbox::push`]), and publishes the pass's
//!   entries after the NIC batch's call ([`Outbox::publish`]): the NIC
//!   flows go first.
//! - The relay thread sends them in order ([`Outbox::send_next`]) with the
//!   usual retries, and counts each one when it leaves, with the pass's
//!   clock plus the time since (its age, lateness and send time as a
//!   packet sent at once would count them); a late one counts in its
//!   slot's queued late count ([`Outbox::late`], the late watch's
//!   `queued`), never as the escape relay's (`relayed`, `since_escape`,
//!   dstack #177).
//! - While the relay thread covers a bounded escape it runs the passes and
//!   sends none of the queue: the packets wait for the hand-back, so a slow
//!   socket send never holds a pass there either. The TX thread's last pass
//!   before an escape queues without publishing, so the relay thread is
//!   not inside a send when the offer comes.
//! - A flow with a packet still queued sends its next ones through the
//!   queue too ([`Outbox::pending`]), also once its receiver is resolved
//!   for the NIC, so a flow never passes its own packets.
//! - A full queue (the relay stuck behind sends for a whole queue): the
//!   packet goes out at once on the pass, counted ([`OutboxView::full`]);
//!   it may then pass its flow's packets still queued: never dropped,
//!   never waited for.
//!
//! One producer (the thread that holds the [`super::Transmitter`]: the TX
//! thread, or the relay covering its escape) and one consumer (the relay
//! thread). Each entry has a lock no one ever waits on: the producer
//! writes only entries the consumer is done with, the consumer reads only
//! published ones. No allocation after the start.

use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::thread::Thread;
use std::time::Instant;

use super::{MAX_FLOWS, PassClock, TxShared, account, nanos, send_timed};
use crate::wire::MAX_DATAGRAM;

/// The packets the queue holds: eight of every flow. A stalled send of
/// ~1.4 ms holds about 4 packets of a flow at 96 kHz and fpp 32, 8 at fpp
/// 16; FOH has one or two socket flows, so the queue fills only behind a
/// relay thread stuck for several ms. About 380 KB, allocated with the
/// host path.
pub const OUTBOX_LEN: usize = 8 * MAX_FLOWS;

/// One queued packet.
struct Entry {
    slot: usize,
    stamp: u64,
    fpp: u16,
    dest: SocketAddrV4,
    /// The datagram's length.
    n: usize,
    /// The pass that queued it: its time, read again when it leaves.
    clock: PassClock,
    /// When it was queued.
    queued: Instant,
    datagram: [u8; MAX_DATAGRAM],
}

/// The relay thread's queue of socket sends (see the module).
pub(super) struct Outbox {
    entries: Box<[Mutex<Entry>]>,
    /// The producer's next entry (not yet published).
    written: AtomicUsize,
    /// The entries the consumer may send: up to here.
    published: AtomicUsize,
    /// The consumer's next entry.
    sent: AtomicUsize,
    /// Each slot's packets queued and not yet sent.
    pending: [AtomicU32; MAX_FLOWS],
    /// The thread that sends them; unset: no one does, the pass sends.
    server: OnceLock<Thread>,
    relayed: AtomicU64,
    full: AtomicU64,
    wait_max_ns: AtomicU64,
    wait_peak_ns: AtomicU64,
    /// Each slot's queued packets over their receiver's latency.
    late: [AtomicU64; MAX_FLOWS],
}

impl std::fmt::Debug for Outbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Outbox")
            .field("view", &self.view())
            .finish()
    }
}

/// The queue's counts since the device started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutboxView {
    /// Whether a relay thread sends the socket packets (else the pass).
    pub serving: bool,
    /// Socket packets the relay thread sent from the queue.
    pub relayed: u64,
    /// Socket packets the pass sent itself because the queue was full.
    pub full: u64,
    /// The longest a packet waited in the queue, ns (a maximum).
    pub wait_max_ns: u64,
}

impl Outbox {
    /// An empty queue, every entry allocated.
    pub(super) fn new() -> Outbox {
        let entries = (0..OUTBOX_LEN)
            .map(|_| {
                Mutex::new(Entry {
                    slot: 0,
                    stamp: 0,
                    fpp: 0,
                    dest: SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0),
                    n: 0,
                    clock: PassClock {
                        now_ns: 0,
                        pass: Instant::now(),
                        rate: 0,
                        margin_ns: 0,
                        latency_ns: 0,
                        relayed: false,
                        escaped_ns: None,
                    },
                    queued: Instant::now(),
                    datagram: [0; MAX_DATAGRAM],
                })
            })
            .collect();
        Outbox {
            entries,
            written: AtomicUsize::new(0),
            published: AtomicUsize::new(0),
            sent: AtomicUsize::new(0),
            pending: std::array::from_fn(|_| AtomicU32::new(0)),
            server: OnceLock::new(),
            relayed: AtomicU64::new(0),
            full: AtomicU64::new(0),
            wait_max_ns: AtomicU64::new(0),
            wait_peak_ns: AtomicU64::new(0),
            late: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    /// The calling thread sends the queue's packets from now on (the relay
    /// thread, once, before its loop); `false` when another thread does.
    pub(super) fn serve_here(&self) -> bool {
        self.server.set(std::thread::current()).is_ok()
    }

    /// Whether a thread sends the queue's packets: only then does the pass
    /// queue them.
    pub(super) fn serving(&self) -> bool {
        self.server.get().is_some()
    }

    /// Whether slot `slot` has packets queued and not yet sent.
    pub(super) fn pending(&self, slot: usize) -> bool {
        self.pending[slot].load(Ordering::Acquire) != 0
    }

    /// Queues the packet stamped `stamp` (`fpp` frames) of slot `slot` to
    /// `dest`, its datagram written by `write` into the buffer it gets
    /// (returning its length), sent later with the pass's `clock`. `false`:
    /// the queue is full, nothing queued (the caller sends it and counts it,
    /// [`Outbox::count_full`]). The producer's; seen by the relay thread
    /// at the next [`Outbox::publish`].
    pub(super) fn push(
        &self,
        clock: &PassClock,
        slot: usize,
        stamp: u64,
        fpp: u16,
        dest: SocketAddrV4,
        write: impl FnOnce(&mut [u8]) -> usize,
    ) -> bool {
        let w = self.written.load(Ordering::Relaxed);
        if w.wrapping_sub(self.sent.load(Ordering::Acquire)) == OUTBOX_LEN {
            return false;
        }
        {
            let mut e = self.entries[w % OUTBOX_LEN]
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            e.n = write(&mut e.datagram);
            e.slot = slot;
            e.stamp = stamp;
            e.fpp = fpp;
            e.dest = dest;
            e.clock = *clock;
            e.queued = Instant::now();
        }
        self.pending[slot].fetch_add(1, Ordering::AcqRel);
        self.written.store(w.wrapping_add(1), Ordering::Relaxed);
        true
    }

    /// A packet the pass sent itself because the queue was full.
    pub(super) fn count_full(&self) {
        self.full.fetch_add(1, Ordering::Relaxed);
    }

    /// Hands the pass's queued packets to the relay thread (after the NIC
    /// batch's call) and wakes it when there are new ones.
    pub(super) fn publish(&self) {
        let w = self.written.load(Ordering::Relaxed);
        if self.published.swap(w, Ordering::Release) != w
            && let Some(server) = self.server.get()
        {
            server.unpark();
        }
    }

    /// The consumer's step: sends the oldest published packet through its
    /// slot's UDP socket of `shared` and counts it in its lane; `false`
    /// when there is none.
    pub(super) fn send_next(&self, shared: &TxShared) -> bool {
        let h = self.sent.load(Ordering::Relaxed);
        if h == self.published.load(Ordering::Acquire) {
            return false;
        }
        let slot = {
            let e = self.entries[h % OUTBOX_LEN]
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let waited = nanos(e.queued.elapsed());
            self.wait_max_ns.fetch_max(waited, Ordering::Relaxed);
            self.wait_peak_ns.fetch_max(waited, Ordering::Relaxed);
            // The pass's clock, read again now; no escape's time (#177
            // counts the escape relay's late packets, not the queue's).
            let clock = PassClock {
                escaped_ns: None,
                ..e.clock
            };
            let socket = &shared.sockets[e.slot];
            let datagram = &e.datagram[..e.n];
            let out = send_timed(&clock, e.stamp, e.fpp, || {
                shared.serve_socket_stall();
                socket.send_to(datagram, e.dest).map(|_| false)
            });
            let receiver_ns = shared.receiver_latency_ns(e.slot, e.dest);
            let over = account(
                &shared.lanes[e.slot],
                &clock,
                e.stamp,
                e.fpp,
                &out,
                e.n,
                receiver_ns,
            );
            if over {
                self.late[e.slot].fetch_add(1, Ordering::Relaxed);
            }
            e.slot
        };
        self.relayed.fetch_add(1, Ordering::Relaxed);
        self.sent.store(h.wrapping_add(1), Ordering::Release);
        self.pending[slot].fetch_sub(1, Ordering::AcqRel);
        true
    }

    /// Sends every published packet; how many (tests, and
    /// [`TxShared::send_queued`]).
    pub(super) fn send_all(&self, shared: &TxShared) -> usize {
        let mut n = 0;
        while self.send_next(shared) {
            n += 1;
        }
        n
    }

    /// The counts since the device started.
    pub(super) fn view(&self) -> OutboxView {
        OutboxView {
            serving: self.serving(),
            relayed: self.relayed.load(Ordering::Relaxed),
            full: self.full.load(Ordering::Relaxed),
            wait_max_ns: self.wait_max_ns.load(Ordering::Relaxed),
        }
    }

    /// Slot `slot`'s queued packets over their receiver's latency since
    /// the device started.
    pub(super) fn late(&self, slot: usize) -> u64 {
        self.late.get(slot).map_or(0, |n| n.load(Ordering::Relaxed))
    }

    /// The longest wait since the last call, ns (the late watch's look).
    pub(super) fn take_wait_peak(&self) -> u64 {
        self.wait_peak_ns.swap(0, Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests;
