// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The RX rings' one writer (dstack #326): the flow sockets read, each
//! packet parsed, judged and written into the rings of the RX channels
//! routed to its slot.
//!
//! Who runs it is chosen once, at the device's start
//! ([`RxShared::pump_intake`]):
//!
//! - the RX thread, inside its pass ([`super::Receiver::pass`]), for the
//!   slots its poll found readable: the low-latency profile, and any
//!   device without an engine pump (a daemon);
//! - the engine's pump in the normal profile, just before it reads each
//!   block ([`Intake::drain_into_rings`], [`crate::stream::Pump::media`]):
//!   a non-blocking read of every slot that has a flow, until its socket
//!   is empty (at most [`PUMP_DRAIN`] datagrams). Packets wait in their sockets' buffers whatever a helper
//!   thread does, and the pump reads by sample position, so its own
//!   lateness loses nothing; a helper thread the scheduler leaves behind
//!   (strih-snv, 15-16.11.2026: up to 17.5 ms against a 13 ms slack) can
//!   no longer write samples behind the reader. The RX thread keeps the
//!   groups' joins, the keepalives and the statistics, and polls no flow
//!   socket then.
//!
//! The ring writes stay single-writer: the choice is made before either
//! thread starts and never changes. A slot's multicast socket is the RX
//! thread's (a join is a system call); the writer reads it by its handle
//! ([`super::handles`]). Each datagram is one `recvmsg` with its kernel
//! receive stamp, up to [`DRAIN`] a slot a pass of the RX thread and
//! [`PUMP_DRAIN`] a block of the pump; a batched `recvmmsg` would save system calls only (about 8 datagrams a
//! flow a block), not time on the audio path.
//!
//! The counters keep their meaning under either writer. The wake delay is
//! the read's time less the kernel's receive time: under the pump it
//! includes the wait for the next block (at most a block, plus the pump's
//! own lateness), and the write delay likewise. `late`, `wire_late`,
//! `late_at_read` and the ring counts are judged as before, at the read.
//! The reported ages (the heartbeat's lateness, the peak, the judge and
//! the mean) are each packet's age at its ARRIVAL (dstack #350,
//! [`record_arrivals`]): the read's wait is reported apart.
//! Under an app's consumer (dstack #307, [`RxShared::app_intake`]: an
//! output callback that drains, then reads the rings up to its now less
//! P) the waits include the time to its next callback, and a packet is
//! `late` (a hole lost) past P plus the time since its last drain
//! ([`reads_apart`]) instead of P plus the pump's block: the reader has
//! read exactly that far. Two edges are counted loosely (counters only,
//! no audio effect): after the reader's clock stepped back (it waits at a
//! position past now less P) a packet between the two reads as not late,
//! and after a gap the reader skips (it starts again at now) the skipped
//! packets read as not late and their holes are lost at the next drain.
//! A datagram to a slot without a flow is read and counted `stray` by the
//! RX thread in both cases (under the pump up to
//! [`super::UNPLANNED_DRAIN`] at each of its wakes, at least every
//! [`super::WAKE_NS`]), so no backlog waits for the slot's next flow; the
//! pump's first drain of a new flow also drops what still waited
//! (`stray`): up to a block of the new flow's own first datagrams too,
//! where it started before that drain. A publish that lands between the RX
//! thread's plan read and its emptying of the slot may drop the new flow's
//! first datagrams the same way (a window of microseconds). Under the pump a slot reads its group's socket only once the
//! RX thread has joined the plan's group ([`pick`]), never the old one's.
//! The thread's reads go through its mio sockets (Windows re-arms a
//! socket's readiness only on mio's own read); the pump's through the
//! sockets' handles, which no poll registered.
//!
//! The per-packet path allocates nothing, takes no lock and logs nothing
//! (`tests/hot_path.rs`).

use std::io::{self, ErrorKind};
use std::net::SocketAddrV4;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::age::arrival_age_ns;
use super::counts::{RxStats, source_word};
use super::lane::{Lane, count_lost};
use super::plan::{RxPlan, RxRoute, new_flow};
use super::socket::{
    Received, handle_of, recv_handle, recv_stamped, wake_delay_ns, wall_now_ns, wire_late,
    wire_lateness,
};
use super::{BUFFER, DRAIN, FRAMES_MAX, MAX_FLOWS, RxShared, holes, netlat};
use crate::clock;
use crate::media::packet;
use crate::media::ring::Ring;
use crate::media::tx::{MAX_CHANNELS, ns_of};
use crate::wire::MAX_DATAGRAM;

/// The most datagrams the pump's drain reads from one slot before a block:
/// twice the largest block's frames even at one frame a packet, so a drain
/// always keeps up with its flow (the RX thread's [`DRAIN`] a pass is
/// followed by another pass at once; the pump's is not).
pub const PUMP_DRAIN: usize = 8 * DRAIN;
const _: () = assert!(PUMP_DRAIN as u64 >= 2 * crate::stream::BLOCKS[4]);

/// The RX rings' writer (see the module): everything it needs, allocated
/// before its first read.
#[derive(Debug)]
pub struct Intake {
    shared: Arc<RxShared>,
    pub(super) lanes: Vec<Lane>,
    /// The routes' version read last.
    routes_seen: Option<u32>,
    /// Each RX channel's route word the last refresh read.
    routes_had: Vec<u64>,
    buf: Box<[u8; BUFFER]>,
    /// One packet's samples, channel slot by channel slot.
    frames: Box<[[i32; FRAMES_MAX]; MAX_CHANNELS]>,
    /// The app's consumer drains (dstack #307, [`RxShared::app_intake`]):
    /// its reads are apart by its own period, not the pump's block.
    app: bool,
    /// Under the app: the last drain's PTP time, samples (`None`: none
    /// yet, or the clock was not ready).
    last_drain: Option<u64>,
    /// Under the app: the samples since the drain before this one, the
    /// reader's step ([`reads_apart`]).
    gap: Option<u64>,
    /// The packets one slot's read placed, for their arrival ages once the
    /// read is done ([`record_arrivals`], dstack #350).
    seen: Box<[Seen; PUMP_DRAIN]>,
    /// The datagrams the last drain read, every one (placed or not; dstack
    /// #354, [`Intake::datagrams`]).
    datagrams: usize,
}

/// A packet placed by a read: its first frame and its wake delay (the
/// kernel's receive time to the read; `None` without a stamp).
#[derive(Debug, Clone, Copy, Default)]
struct Seen {
    pos: u64,
    wake: Option<u64>,
}

const _: () = assert!(DRAIN <= PUMP_DRAIN);

impl Intake {
    pub(super) fn new(shared: Arc<RxShared>) -> Self {
        let channels = shared.rings.len();
        Intake {
            lanes: (0..MAX_FLOWS).map(|_| Lane::new(channels)).collect(),
            routes_seen: None,
            routes_had: vec![0; channels],
            buf: Box::new([0; BUFFER]),
            frames: Box::new([[0; FRAMES_MAX]; MAX_CHANNELS]),
            shared,
            app: false,
            last_drain: None,
            gap: None,
            seen: Box::new([Seen::default(); PUMP_DRAIN]),
            datagrams: 0,
        }
    }

    /// The writer of an app that drains on its own consumer's thread
    /// (dstack #307): a packet is late, and a hole lost, against the time
    /// since its last drain instead of the pump's block.
    pub(super) fn for_app(shared: Arc<RxShared>) -> Self {
        Intake {
            app: true,
            ..Intake::new(shared)
        }
    }

    /// The pump's drain (normal profile), at PTP time `now_ns` (`None`:
    /// the clock is not ready; packets are read and counted `unplaced`):
    /// the plans and routes that changed, then every slot that has a flow
    /// read until its socket is empty (at most [`PUMP_DRAIN`] datagrams),
    /// the holes behind the playout given up. A slot whose flow is new is
    /// emptied first: what waited there came before the flow (counted
    /// `stray`). Never blocks. Returns `true` when every socket read was
    /// left empty (`false`: a slot hit [`PUMP_DRAIN`] and has more; the
    /// pump drains it again at its next block, an idle drain at once).
    pub fn drain_into_rings(&mut self, now_ns: Option<u64>) -> bool {
        self.datagrams = 0;
        if self.app {
            let now = now_ns.map(|ns| clock::to_samples(ns, self.shared.rate));
            self.gap = since(self.last_drain, now);
            self.last_drain = now;
        }
        let moves = self.shared.handles.moves();
        let wall = self.shared.stamps().then(wall_now_ns).flatten();
        let mono = self.shared.now_ns();
        // A new flow's slot is emptied at this drain only, from the socket
        // it reads now: a group socket the RX thread joins later is new
        // and holds nothing of an older flow.
        let fresh = self.refresh();
        self.expire(now_ns);
        let mut emptied = true;
        for (slot, fresh) in fresh.into_iter().enumerate() {
            let Some(lane) = self.lanes.get(slot).filter(|l| l.plan.src.is_some()) else {
                continue;
            };
            let own = self.shared.sockets.get(slot).map(handle_of);
            let Some(handle) = self
                .shared
                .handles
                .get(slot)
                .and_then(|(handle, of)| pick(lane.plan.group, handle, of, own))
            else {
                continue;
            };
            let read = |buf: &mut [u8]| recv_handle(handle, buf);
            if fresh && let Some(stats) = self.shared.lanes.get(slot) {
                self.datagrams += discard(stats, &mut self.buf[..], PUMP_DRAIN, read);
            }
            emptied &= self.receive(slot, (now_ns, mono, wall), handle, PUMP_DRAIN, read);
        }
        self.expire(now_ns);
        self.shared.handles.ack(moves);
        emptied
    }

    /// The datagrams the last [`Intake::drain_into_rings`] (or the RX
    /// thread's last pass) read from its sockets, every one (placed, stray,
    /// refused, or emptied from a new flow's slot): the drain's work
    /// (dstack #354).
    pub fn datagrams(&self) -> usize {
        self.datagrams
    }

    /// The RX thread's part of its pass, for the slots in `ready` (one bit
    /// each), each read from its group's socket in `joined` while it has
    /// one, else its own in `sockets` (the ones its poll registered): read
    /// as [`Intake::drain_into_rings`] reads; returns the slots that still
    /// have datagrams.
    pub(super) fn pass(
        &mut self,
        ready: u32,
        at: (Option<u64>, u64, Option<u64>),
        sockets: &[mio::net::UdpSocket],
        joined: &[Option<(SocketAddrV4, mio::net::UdpSocket)>],
    ) -> u32 {
        let now_ns = at.0;
        self.datagrams = 0;
        let moves = self.shared.handles.moves();
        self.refresh();
        self.expire(now_ns);
        let mut left = ready;
        let mut todo = ready;
        while todo != 0 {
            let slot = todo.trailing_zeros() as usize;
            todo &= todo - 1;
            let socket = match joined.get(slot) {
                Some(Some((_, s))) => Some(s),
                _ => sockets.get(slot),
            };
            let emptied = socket.is_none_or(|s| {
                self.receive(slot, at, handle_of(s), DRAIN, |buf| recv_stamped(s, buf))
            });
            if emptied {
                left &= !(1 << slot);
            }
        }
        // Holes behind the playout are lost before and after the reads: a
        // packet for a hole the pump already read is late, no reorder.
        self.expire(now_ns);
        self.shared.handles.ack(moves);
        left
    }

    /// At PTP time `now_ns` (`None`: our clock is not ready, nothing to
    /// do), each flow's holes wholly behind its playout point (its latency
    /// and the reader's step before now, [`reads_apart`]) are lost: the
    /// reader has read past them (dstack #278).
    fn expire(&mut self, now_ns: Option<u64>) {
        let Some(ns) = now_ns else { return };
        let now = clock::to_samples(ns, self.shared.rate);
        for (lane, stats) in self.lanes.iter_mut().zip(self.shared.lanes.iter()) {
            let limit = u64::from(lane.plan.latency) + reads_apart(lane.plan.block, self.gap);
            count_lost(stats, lane.holes.expire(now.saturating_sub(limit)));
        }
    }

    /// Reads the plans that changed (a new one resets its slot, a new flow
    /// restarts its rings: dstack #234) and the routes
    /// ([`Intake::retarget`]); returns the slots whose flow is new.
    fn refresh(&mut self) -> [bool; MAX_FLOWS] {
        let shared = &*self.shared;
        // dstack #234: the slots whose flow is new; their rings restart.
        let mut fresh = [false; MAX_FLOWS];
        for (slot, (lane, cell)) in self.lanes.iter_mut().zip(&shared.plans).enumerate() {
            if lane.seen != Some(cell.version())
                && let Some((version, w)) = cell.read()
            {
                let plan = RxPlan::from_words(w);
                if plan != lane.plan {
                    fresh[slot] = new_flow(lane.plan, plan);
                    lane.plan = plan;
                    lane.source = None;
                    lane.end = None;
                    lane.read_at = None;
                    if let Some(stats) = shared.lanes.get(slot) {
                        stats.source.store(0, Ordering::Relaxed);
                        stats.heard.store(0, Ordering::Release);
                        // The old flow's open holes never fill now.
                        count_lost(stats, lane.holes.clear());
                    }
                }
                lane.seen = Some(version);
            }
        }
        self.retarget();
        // The writer restarts the rings of a new flow itself, before this
        // pass writes: a restart of the publish's alone would be undone by
        // the old flow's packets written meanwhile (dstack #234).
        for (lane, _) in self.lanes.iter().zip(fresh).filter(|(_, f)| *f) {
            for &(_, rx) in &lane.targets {
                self.shared.rings[rx].restart();
            }
        }
        fresh
    }

    /// After the routes changed (their version moves once
    /// [`RxShared::publish`] wrote them all), which RX channels each slot
    /// feeds; a channel routed anew restarts its ring (dstack #234).
    fn retarget(&mut self) {
        let shared = &*self.shared;
        let version = shared.routes_version.load(Ordering::Acquire);
        if self.routes_seen == Some(version) {
            return;
        }
        for lane in &mut self.lanes {
            lane.targets.clear();
        }
        for (rx, (cell, had)) in shared.routes.iter().zip(&mut self.routes_had).enumerate() {
            let word = cell.load(Ordering::Acquire);
            if let Some(r) = RxRoute::from_word(word)
                && r.slot < MAX_CHANNELS
                && let Some(lane) = self.lanes.get_mut(r.flow)
            {
                lane.targets.push((r.slot, rx));
                if *had != word {
                    shared.rings[rx].restart();
                }
            }
            *had = word;
        }
        self.routes_seen = Some(version);
    }

    /// Reads up to `cap` datagrams of flow slot `slot` with `read` from the
    /// socket whose [`handle_of`] is `socket`, at `at`: (PTP now, `mono` the
    /// reader's clock ([`RxShared::now_ns`]), `wall` the read's wall clock,
    /// ns since 1970); `true` when it read no more (none left, or a read
    /// error). The hot path: no allocation, no lock, no log.
    fn receive(
        &mut self,
        slot: usize,
        at: (Option<u64>, u64, Option<u64>),
        socket: u64,
        cap: usize,
        mut read: impl FnMut(&mut [u8]) -> io::Result<Received>,
    ) -> bool {
        debug_assert!(cap <= PUMP_DRAIN, "a read's packets fit `seen`");
        let (now_ns, mono, wall) = at;
        let Intake {
            shared,
            lanes,
            buf,
            frames,
            gap,
            seen,
            datagrams,
            ..
        } = self;
        let (Some(lane), Some(stats)) = (lanes.get_mut(slot), shared.lanes.get(slot)) else {
            return true;
        };
        let rate = shared.rate;
        let rings: &[Ring] = &shared.rings;
        let now = now_ns.map(|ns| (ns, clock::to_samples(ns, rate)));
        let count = |a: &AtomicU64| {
            a.fetch_add(1, Ordering::Relaxed);
        };
        // dstack #350: the time since the slot's last read that left this
        // same socket empty brackets the arrival of what this one reads (a
        // socket read first now, a group's, may hold older datagrams).
        let since = now_ns
            .zip(lane.read_at.filter(|&(_, s)| s == socket))
            .map(|(n, (t, _))| n.saturating_sub(t));
        let mut placed = 0;
        let mut done = false;
        let mut emptied = false;
        for _ in 0..cap {
            let received = read(&mut buf[..]);
            if received.is_ok() {
                *datagrams += 1;
            }
            let (n, from, at_ns) = match received {
                Ok(Received {
                    len,
                    from: Some(from),
                    at_ns,
                }) => (len, from, at_ns),
                Ok(_) => {
                    count(&stats.stray);
                    continue;
                }
                Err(e) => {
                    // Would block: none left. Anything else (an unconnected
                    // UDP socket reports none we know of) is counted and
                    // ends this slot's reads for the pass; only an empty
                    // socket brackets the next read's arrivals.
                    done = true;
                    emptied = e.kind() == ErrorKind::WouldBlock;
                    if !emptied {
                        count(&stats.recv_errors);
                    }
                    break;
                }
            };
            if lane.plan.src != Some(*from.ip()) {
                count(&stats.stray);
                continue;
            }
            let nslots = usize::from(lane.plan.nslots);
            let datagram = &buf[..n];
            let header = packet::read_header(datagram);
            let read = (n <= MAX_DATAGRAM && nslots <= MAX_CHANNELS)
                .then(|| {
                    packet::read_frames(datagram, nslots, usize::from(lane.plan.bps), |f, s, v| {
                        frames[s][f] = v;
                    })
                    .ok()
                })
                .flatten();
            let (Some((secs, sub)), Some(nframes @ 1..)) = (header, read) else {
                count(&stats.refused);
                continue;
            };
            count(&stats.packets);
            if lane.source != Some(from) {
                stats
                    .source
                    .store(source_word(Some(from)), Ordering::Relaxed);
                lane.source = Some(from);
            }
            stats.heard.store(mono.saturating_add(1), Ordering::Release);
            let wake = wake_delay_ns(wall, at_ns);
            if let Some(w) = wake {
                stats.max_wake_ns.fetch_max(w, Ordering::Relaxed);
                stats.wake_peak_ns.fetch_max(w, Ordering::Relaxed);
            }
            let Some((now_ns, now)) = now else {
                count(&stats.unplaced);
                continue;
            };
            let step = reads_apart(lane.plan.block, *gap);
            let pos = place(PlaceArgs {
                stats,
                lane,
                rings,
                rate,
                frames,
                now_ns,
                now,
                secs,
                sub,
                nframes,
                nslots,
                wake,
                at_ns,
                step,
            });
            if let Some(s) = seen.get_mut(placed) {
                *s = Seen { pos, wake };
                placed += 1;
            }
        }
        if let Some(ns) = now_ns {
            record_arrivals(stats, rate, ns, since, &seen[..placed]);
        }
        lane.read_at = now_ns.filter(|_| emptied).map(|t| (t, socket));
        done
    }
}

/// The reader's step, samples: how far past a flow's playout point it may
/// have read when the writer places a packet. The pump's (and the RX
/// thread's) is the plan's `block`; an app that drains on its own
/// consumer's thread (dstack #307) reads up to its drain's now less P, so
/// its step is the time since its last drain (`gap`; the block until it
/// has one).
pub(super) fn reads_apart(block: u16, gap: Option<u64>) -> u64 {
    gap.unwrap_or(u64::from(block))
}

/// Samples from `last` to `now` (`None` when either is unknown or the
/// clock went back).
pub(super) fn since(last: Option<u64>, now: Option<u64>) -> Option<u64> {
    now?.checked_sub(last?)
}

/// Which socket the pump reads for a slot whose plan names `group`: the
/// slot's own (`own`) without a group; with one, the RX thread's group
/// socket `handle` when it is that group's (`of`, the group's
/// [`source_word`]), the own socket while no group socket is open (a join
/// that failed reads as the RX thread does), and none while the RX thread
/// still holds another group's socket (it has not regrouped yet: those
/// datagrams are not this flow's).
pub(super) fn pick(
    group: Option<SocketAddrV4>,
    handle: u64,
    of: u64,
    own: Option<u64>,
) -> Option<u64> {
    match group {
        None => own,
        Some(_) if handle == 0 => own,
        Some(g) if of == source_word(Some(g)) => Some(handle),
        Some(_) => None,
    }
}

/// Reads and drops up to `cap` datagrams with `read` (they came before
/// the slot's flow, or to a slot without one), each counted `stray`; a
/// failure other than an empty socket counts `recv_errors` and ends it.
/// Returns how many it dropped.
pub(super) fn discard(
    stats: &RxStats,
    buf: &mut [u8],
    cap: usize,
    mut read: impl FnMut(&mut [u8]) -> io::Result<Received>,
) -> usize {
    for n in 0..cap {
        match read(buf) {
            Ok(_) => {
                stats.stray.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                if e.kind() != ErrorKind::WouldBlock {
                    stats.recv_errors.fetch_add(1, Ordering::Relaxed);
                }
                return n;
            }
        }
    }
    cap
}

/// One well-formed packet of a flow, read at PTP time `now_ns` (`now` in
/// samples), to place.
struct PlaceArgs<'a> {
    stats: &'a RxStats,
    lane: &'a mut Lane,
    rings: &'a [Ring],
    rate: u32,
    frames: &'a [[i32; FRAMES_MAX]; MAX_CHANNELS],
    now_ns: u64,
    now: u64,
    secs: u32,
    sub: u32,
    nframes: usize,
    nslots: usize,
    wake: Option<u64>,
    at_ns: Option<u64>,
    /// The reader's step, samples ([`reads_apart`]).
    step: u64,
}

/// A packet judged and placed at the read: `late` and `wire_late`, the
/// network latency and the minute's windows, its holes, its samples
/// written into the rings routed to its slots, and its write delay;
/// returns its first frame's position. Its ages are recorded once the
/// slot's read is done ([`record_arrivals`], dstack #350). No allocation,
/// no lock, no log.
#[cfg_attr(iem_rtsan, sanitize(realtime = "nonblocking"))]
fn place(p: PlaceArgs<'_>) -> u64 {
    let PlaceArgs {
        stats,
        lane,
        rings,
        rate,
        frames,
        now_ns,
        now,
        secs,
        sub,
        nframes,
        nslots,
        wake,
        at_ns,
        step,
    } = p;
    let count = |a: &AtomicU64| {
        a.fetch_add(1, Ordering::Relaxed);
    };
    let pos = clock::from_stamp(secs, sub, rate, now);
    // `late` is the reader's verdict, judged at the read.
    let lateness = now.saturating_sub(pos);
    let limit = u64::from(lane.plan.latency) + step;
    let late = lateness > limit;
    if late {
        count(&stats.late);
    }
    if let Some(w) = wake {
        if wire_late(now_ns, w, pos, rate, limit) {
            count(&stats.wire_late);
        }
        let network = netlat::network_latency_ns(now_ns, w, ns_of(pos, rate));
        stats.network.record(network);
        stats.network_minute.record(network);
        // The minute's two windows hold the same packets.
        stats.wake_minute.record(w);
    }
    let end = pos.saturating_add(nframes as u64);
    // dstack #278: holes, reorders and gaps.
    let placed = holes::place(&mut lane.end, &mut lane.holes, pos, end, !late);
    if placed.out_of_order {
        count(&stats.out_of_order);
    }
    if placed.reordered {
        count(&stats.reordered);
    }
    count_lost(stats, placed.lost);
    let mut read_before = false;
    let mut wrote = false;
    for &(s, rx) in &lane.targets {
        if s < nslots
            && let Some(ring) = rings.get(rx)
        {
            read_before |= ring.write_at(pos, &frames[s][..nframes]).late > 0;
            wrote = true;
        }
    }
    if read_before {
        count(&stats.late_at_read);
    }
    // dstack #234: the kernel's receive time to the writes' end (one more
    // wall-clock read, only for a stamped packet that reached a ring).
    if wrote
        && wake.is_some()
        && let Some(delay) = wake_delay_ns(wall_now_ns(), at_ns)
    {
        stats.write_minute.record(delay);
    }
    pos
}

/// The packets one slot's read placed at PTP time `now_ns`, `since` the
/// time since its last read that left the socket empty, and each one's
/// age at its arrival ([`arrival_age_ns`], dstack #350): the heartbeat's
/// lateness (0x8003) and peak, samples, the judge (the least, ns) and the
/// mean age; the rest of its age at the read is its read wait (summed,
/// counted and peaked apart). No allocation, no lock, no log.
#[cfg_attr(iem_rtsan, sanitize(realtime = "nonblocking"))]
fn record_arrivals(stats: &RxStats, rate: u32, now_ns: u64, since: Option<u64>, seen: &[Seen]) {
    let at_read = |s: &Seen| now_ns.saturating_sub(ns_of(s.pos, rate));
    let Some(newest) = seen.iter().map(at_read).min() else {
        return;
    };
    for s in seen {
        let read_age = at_read(s);
        let age = arrival_age_ns(read_age, s.wake, since, newest);
        let wait = read_age - age;
        // The arrival on our clock, in samples past the first frame, and
        // the read's: the peak at the read bounds a burst the bracket
        // reads as on time (Windows).
        let lateness = wire_lateness(now_ns, wait, s.pos, rate);
        stats.max_lateness.fetch_max(lateness, Ordering::Relaxed);
        stats.lateness_peak.fetch_max(lateness, Ordering::Relaxed);
        let at_read_samples = wire_lateness(now_ns, 0, s.pos, rate);
        stats
            .lateness_at_read_peak
            .fetch_max(at_read_samples, Ordering::Relaxed);
        // The judge: the packet's own stamp in ns against its arrival on
        // our clock; the same age summed and counted for its mean (#342).
        stats.min_lateness_ns.0.fetch_min(age, Ordering::Relaxed);
        stats.ages.record(age);
        stats.read_waits.record(wait);
        stats.read_wait_peak_ns.fetch_max(wait, Ordering::Relaxed);
    }
}
