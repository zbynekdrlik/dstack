// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.
// Protocol facts (byte layouts, opcodes, behaviour) checked against
// inferno (https://github.com/teodly/inferno) as a reference; the code is
// our own.

//! Our TX flows (Gen 3 phase B, plan Task 4): the flows receivers asked
//! for on UDP 4455 ([`crate::flows::server`]) and the thread that sends
//! them.
//!
//! - [`TxTable`] (the control side: the 4455 server, later the ARC lists):
//!   at most [`MAX_FLOWS`] flows, each to the one receiver that asked for
//!   it, of at most [`MAX_CHANNELS`] slots. A flow without a datagram from
//!   its receiver for [`KEEPALIVE_TIMEOUT_NS`] expires: it sends nothing
//!   until an update or a new request for the same destination revives it,
//!   and its slot is freed when a new destination needs one (inferno). A
//!   multicast flow (ARC 2.8.9's create, [`TxTable::add_multicast`], dstack
//!   #193) goes to a group ([`multicast_group`]) instead: no receiver keeps
//!   it alive, so it never expires, and only its delete
//!   ([`TxTable::remove_multicast`]) ends it.
//! - [`TxShared`]: what the control side and the TX thread share. The
//!   table sits behind a mutex only the control side takes; the TX thread
//!   reads each slot's [`Plan`] and each TX channel's [`Source`] from
//!   seqlocked atomics (never blocking: a read that meets a write keeps the
//!   last copy), and counts per slot what it sent and heard in atomics,
//!   and per TX channel the largest magnitude sent (the heartbeat's peaks,
//!   [`TxShared::take_peak`]).
//!   One UDP socket per slot, bound to the device's address: the flow's
//!   audio goes from it to the receiver, and the receiver's keepalives come
//!   back to it (only the receiver's own address counts, as a connected
//!   socket would see it).
//! - [`Schedule`]: a flow's packets in time. The packet stamped T carries
//!   the source's samples T…T+fpp−1 and goes out once the clock has passed
//!   T + fpp by the device's `tx_margin_ns` ([`TX_MARGIN_NS`] unless the
//!   config sets it); the first stamp is a multiple of fpp, a lag past the
//!   flow's bound (our advertised latency, or the receiver's when longer)
//!   or a next stamp further ahead of the clock than that bound (at least
//!   the block) and a packet (the clock went back,
//!   [`Schedule::ahead_limit`], dstack #440) starts it again
//!   (counted), and so does a re-anchor of the clock. A
//!   flow sends exactly rate / fpp packets a second and never more.
//! - [`Transmitter`] and [`spawn`]: the TX thread. Nothing leaves before the
//!   clock is ready. Every [`EVENTS_EVERY_NS`] it reads the plans, the
//!   sources and the keepalives; in between it sends what is due and
//!   sleeps until the next packet. Phase B sends the test sources of
//!   [`super::source`] (every channel silent at start); phase C adds
//!   [`Source::Engine`]: a TX channel the engine feeds has an engine ring
//!   ([`TxShared::engine_ring`]) that the engine's pump
//!   ([`crate::stream`]) writes and the TX thread reads at each packet's
//!   stamp, a hole counted in the slot's `holes` like the echo's.
//!
//! The hot path (a packet's samples, the packet, the send) allocates
//! nothing, takes no lock and logs nothing; its one system call is the send
//! (`tests/hot_path.rs`). Errors are counted per slot ([`LaneCounts`]), and
//! so is how late each packet left past its due time ([`LATE_EDGES_NS`]):
//! a stall inside the latency sends every packet, late, and a receiver's
//! Dante Controller shows it, so our counts must too.

use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering, fence};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Instant;

use tracing::{info, warn};

#[cfg(test)]
use super::bypass::HEADROOM;
use super::bypass::{self, Direct};
use super::packet::MEDIA_HEADER;
use super::ring::Ring;
use super::source::Source;
use crate::clock;
#[cfg(test)]
use crate::clock::PtpClock;
use crate::device::DeviceConfig;
#[cfg(test)]
use crate::flows::control::{self, FlowRequest, Handle, status};
use crate::wire::MAX_DATAGRAM;
#[cfg(test)]
use std::thread::JoinHandle;

/// The most TX flows at once (inferno's `MAX_FLOWS`).
pub const MAX_FLOWS: usize = 32;
/// The most channel slots of one flow (inferno's `MAX_CHANNELS_IN_FLOW`;
/// the mDNS `nchan` and ARC say so).
pub const MAX_CHANNELS: usize = crate::arc::MAX_CHANNELS_IN_FLOW as usize;
/// The most frames per packet (inferno's `FPP_MAX`).
pub const FPP_MAX: u16 = 256;
/// A flow with no datagram from its receiver for this long expires
/// (inferno's 4 s).
pub const KEEPALIVE_TIMEOUT_NS: u64 = 4_000_000_000;
/// How long after a packet's last sample it is sent at the earliest: our
/// clock may run up to a few tens of µs ahead of the grandmaster's. The
/// default of the device's `tx_margin_ns`.
pub const TX_MARGIN_NS: u64 = 250_000;
/// How often the TX thread reads the plans, the sources and the keepalives
/// (inferno's 33 ms).
pub const EVENTS_EVERY_NS: u64 = 33_000_000;

/// The lower edges (ns past due) of the lateness bins after the first:
/// < 0.25, < 0.5, < 1, < 2, < 5 and >= 5 ms, the steps a receiver's
/// latency reads in. A packet is due at its last sample plus the margin
/// ([`Schedule::wake_ns`]); its lateness is read when its send returns.
pub const LATE_EDGES_NS: [u64; 5] = [250_000, 500_000, 1_000_000, 2_000_000, 5_000_000];
/// How many lateness bins a lane counts.
pub const LATE_BINS: usize = LATE_EDGES_NS.len() + 1;

/// The bin of a packet `ns` past due: the count of edges at or below it.
pub fn late_bin(ns: u64) -> usize {
    LATE_EDGES_NS.iter().take_while(|&&edge| ns >= edge).count()
}
/// The most datagrams read from one flow's socket a time (a flood never
/// holds the thread).
pub const KEEPALIVE_DRAIN: usize = 64;
/// The TX thread's `SCHED_FIFO` priority when `RLIMIT_RTPRIO` allows it:
/// below dsync's 50, so the box's clock is never delayed by our sends,
/// and above the RX thread's, so a received flood on a shared core never
/// holds a send (gen3 at 250 µs, 31.10.2026: 168 k packets/s received at
/// one priority delayed sends by up to 247 µs).
pub const TX_PRIORITY: i32 = 47;
/// The DSCP of our audio packets: Expedited Forwarding, as Dante hardware
/// marks its audio (PTP is CS7, 56; dsync's). A switch with QoS ranks
/// them above bulk traffic.
pub const DSCP_EF: u8 = 46;
/// The TX flow sockets' `SO_PRIORITY` on Linux: pfifo_fast's first band,
/// the highest a socket takes without `CAP_NET_ADMIN`.
pub const SOCKET_PRIORITY: u32 = 6;

/// The send buffer asked for each TX flow socket (Linux gives at most its
/// `wmem_max`). It rides out a stall of the NIC's send completions: on the
/// FOH PC a 16–32 ms receive DPC holds them every ~96 s, and with the
/// default buffer every send failed meanwhile (WSAENOBUFS, dasio#1). A
/// packet held through the stall goes out late, which the receiver plays
/// if it is still within its latency, instead of failing here.
pub const SEND_BUFFER_BYTES: usize = 1 << 20;

/// Marks a TX flow socket: DSCP EF and `SO_PRIORITY` [`SOCKET_PRIORITY`]
/// on Linux. Windows sets a packet's DSCP only through a QoS policy
/// (`IP_TOS` from a program is ignored by default), so there it is the
/// box's policy, not ours.
#[cfg(target_os = "linux")]
fn mark(s: &UdpSocket) -> io::Result<()> {
    let r = socket2::SockRef::from(s);
    r.set_tos_v4(u32::from(DSCP_EF) << 2)?;
    r.set_priority(SOCKET_PRIORITY)
}

/// Windows: nothing (see the Linux one).
#[cfg(not(target_os = "linux"))]
fn mark(_: &UdpSocket) -> io::Result<()> {
    Ok(())
}

/// Asks for a send buffer of `bytes` on `s`; returns what the OS gave
/// (Linux doubles what it grants, up to its `wmem_max`; 0 when it cannot
/// say). A refusal leaves the default.
fn set_send_buffer(s: &UdpSocket, bytes: usize) -> usize {
    let sock = socket2::SockRef::from(s);
    let _refused = sock.set_send_buffer_size(bytes);
    sock.send_buffer_size().unwrap_or(0)
}

/// The tries of a seqlocked read before it keeps the last copy.
const READ_TRIES: usize = 16;

/// Whether a flow of `nch` slots, `bps` bytes a sample and `fpp` frames a
/// packet can be sent: at most [`MAX_CHANNELS`] slots, 2, 3 or 4 bytes, 1
/// to [`FPP_MAX`] frames, and a packet that fits one datagram
/// ([`MAX_DATAGRAM`], so it is never fragmented).
pub fn sendable(nch: usize, bps: usize, fpp: u16) -> bool {
    nch <= MAX_CHANNELS
        && (2..=4).contains(&bps)
        && (1..=FPP_MAX).contains(&fpp)
        && MEDIA_HEADER + nch * bps * usize::from(fpp) <= MAX_DATAGRAM
}

/// What the TX thread sends for one slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Plan {
    /// The receiver; `None`: send nothing.
    pub dest: Option<SocketAddrV4>,
    pub fpp: u16,
    pub bps: u8,
    /// The slots used, at most [`MAX_CHANNELS`].
    pub nch: u8,
    /// Each slot's TX channel number (from 1), 0 for an empty slot.
    pub channels: [u16; MAX_CHANNELS],
}

impl Plan {
    /// The plan as four words (the seqlocked cell's).
    pub fn words(&self) -> [u64; 4] {
        let Some(dest) = self.dest else {
            return [0; 4];
        };
        [
            (u64::from(dest.ip().to_bits()) << 32)
                + (u64::from(dest.port()) << 16)
                + u64::from(self.fpp),
            (1 << 63) + (u64::from(self.bps) << 8) + u64::from(self.nch),
            pack(&self.channels[..4]),
            pack(&self.channels[4..]),
        ]
    }

    /// The plan [`Plan::words`] wrote.
    pub fn from_words(w: [u64; 4]) -> Plan {
        if w[1] >> 63 == 0 {
            return Plan::default();
        }
        let mut channels = [0u16; MAX_CHANNELS];
        for (i, c) in channels.iter_mut().enumerate() {
            *c = (w[2 + i / 4] >> (16 * (i % 4))) as u16;
        }
        Plan {
            dest: Some(SocketAddrV4::new(
                Ipv4Addr::from_bits((w[0] >> 32) as u32),
                (w[0] >> 16) as u16,
            )),
            fpp: w[0] as u16,
            bps: (w[1] >> 8) as u8,
            nch: w[1] as u8,
            channels,
        }
    }

    /// Whether going from `before` to this plan starts the schedule again:
    /// another receiver (or none), fpp or width. Changed channels keep it.
    pub fn restarts(&self, before: &Plan) -> bool {
        self.dest != before.dest || self.fpp != before.fpp || self.bps != before.bps
    }
}

/// Four channel numbers in one word, the first lowest.
fn pack(ids: &[u16]) -> u64 {
    ids.iter()
        .enumerate()
        .map(|(i, &id)| u64::from(id) << (16 * i))
        .sum()
}

/// A source as three words (the seqlocked cell's).
fn source_words(s: Source) -> [u64; 3] {
    match s {
        Source::Silence => [0, 0, 0],
        Source::Counter => [1, 0, 0],
        Source::Sine { hz, dbfs } => [2, hz.to_bits(), dbfs.to_bits()],
        Source::Echo { rx, offset } => [3, rx as u64, u64::from(offset)],
        Source::Engine => [4, 0, 0],
    }
}

/// The source [`source_words`] wrote.
fn source_from(w: [u64; 3]) -> Source {
    match w[0] {
        1 => Source::Counter,
        2 => Source::Sine {
            hz: f64::from_bits(w[1]),
            dbfs: f64::from_bits(w[2]),
        },
        3 => Source::Echo {
            rx: usize::try_from(w[1]).unwrap_or(usize::MAX),
            offset: w[2] as u32,
        },
        4 => Source::Engine,
        _ => Source::Silence,
    }
}

/// `N` words behind a seqlock: one writer at a time (the control side,
/// holding [`TxShared`]'s lock), readers that never block.
#[derive(Debug)]
pub(crate) struct Cell<const N: usize> {
    /// Odd while a write is under way.
    seq: AtomicU32,
    words: [AtomicU64; N],
}

impl<const N: usize> Cell<N> {
    pub(crate) fn new() -> Self {
        Cell {
            seq: AtomicU32::new(0),
            words: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    /// Writes the words (the caller is the only writer).
    pub(crate) fn write(&self, w: [u64; N]) {
        let s = self.seq.load(Ordering::Relaxed);
        self.seq.store(s.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        for (word, value) in self.words.iter().zip(w) {
            word.store(value, Ordering::Relaxed);
        }
        self.seq.store(s.wrapping_add(2), Ordering::Release);
    }

    /// The version a reader compares to see a change.
    pub(crate) fn version(&self) -> u32 {
        self.seq.load(Ordering::Acquire)
    }

    /// The words and their version, all of one write; `None` when every
    /// try met a write under way.
    pub(crate) fn read(&self) -> Option<(u32, [u64; N])> {
        for _ in 0..READ_TRIES {
            let s = self.seq.load(Ordering::Acquire);
            if s.is_multiple_of(2) {
                let w = std::array::from_fn(|i| self.words[i].load(Ordering::Relaxed));
                fence(Ordering::Acquire);
                if self.seq.load(Ordering::Relaxed) == s {
                    return Some((s, w));
                }
            }
            std::hint::spin_loop();
        }
        None
    }

    /// The words as last written (for the writer).
    pub(crate) fn peek(&self) -> [u64; N] {
        std::array::from_fn(|i| self.words[i].load(Ordering::Relaxed))
    }
}

/// One slot's counts, written by the TX thread.
#[derive(Debug, Default)]
pub struct LaneStats {
    packets: AtomicU64,
    /// The OS error of the last failed send (its raw code; 0 for none).
    last_os_error: std::sync::atomic::AtomicI32,
    bytes: AtomicU64,
    send_errors: AtomicU64,
    send_retries: AtomicU64,
    keepalives: AtomicU64,
    rebootstraps: AtomicU64,
    holes: AtomicU64,
    late: [AtomicU64; LATE_BINS],
    late_max_ns: AtomicU64,
    send_max_ns: AtomicU64,
    age_max_ns: AtomicU64,
    /// The oldest packet since the late watch last took it ([`late`]).
    age_peak_ns: AtomicU64,
    /// The oldest packet since an app's status last took it
    /// ([`TxShared::take_app_view`], dstack #325): its own, so neither take
    /// starves the other.
    age_app_peak_ns: AtomicU64,
    over_latency: AtomicU64,
    /// Of those, the packets the relay thread sent (dstack #177).
    over_latency_relayed: AtomicU64,
    /// `u64::MAX` less the shortest time from the last bounded escape's
    /// start to a packet over its receiver's latency, since the late watch
    /// last took it; 0: none (dstack #177).
    since_escape: AtomicU64,
    direct: AtomicU64,
    /// The NIC refused a send of this slot since the control side last
    /// took it (`refused.rs`, dstack #195).
    refused: AtomicBool,
}

impl LaneStats {
    /// A packet over its receiver's latency left `ns` after the last
    /// bounded escape began: kept when it is the shortest since the last
    /// take ([`TxShared::take_since_escape`]).
    fn note_since_escape(&self, ns: u64) {
        self.since_escape
            .fetch_max(u64::MAX - ns, Ordering::Relaxed);
    }
}

/// One slot's counts since the device started (every flow it carried).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LaneCounts {
    /// Packets sent whole.
    pub packets: u64,
    /// Their bytes on the wire: the datagram plus [`WIRE_OVERHEAD`] (what
    /// the NIC's own counter counts, [`crate::traffic`]).
    pub bytes: u64,
    /// Packets the socket refused or cut (after their retries, when the
    /// refusal was transient: [`send_retry`]).
    pub send_errors: u64,
    /// Packets the NIC refused for a moment that went out on a retry
    /// ([`send_retry`], dstack #1): recovered, not lost.
    pub send_retries: u64,
    /// The OS error of the last failed send: its raw code (errno, WSA), 0
    /// for none, -1 for an error without one.
    pub last_os_error: i32,
    /// Datagrams from the flow's receiver.
    pub keepalives: u64,
    /// Schedules started again: a lag past the latency, a jump, a
    /// re-anchor.
    pub rebootstraps: u64,
    /// Echo samples the RX ring did not hold, and engine samples the
    /// engine ring did not (sent as silence).
    pub holes: u64,
    /// Packets sent (whole or not) by how late they left past due, one
    /// count a bin of [`LATE_EDGES_NS`].
    pub late_bins: [u64; LATE_BINS],
    /// The largest lateness of any packet since the device started, ns
    /// (a maximum: it never goes down; the bins are the counts).
    pub late_max_ns: u64,
    /// The longest single send since the device started, ns (a maximum).
    pub send_max_ns: u64,
    /// The oldest any packet was when its send returned, ns since its
    /// first sample's time (its stamp): what a receiver measures, less the
    /// network (a maximum).
    pub age_max_ns: u64,
    /// Packets whose age past their stamp was over the latency their
    /// receiver plays at (its own as it states it, at least our advertised
    /// latency and the packet's floor): it counts each one late (Dante
    /// Controller's Latency tab), whatever bin it fell in. Never counted
    /// while the receiver's latency is unknown (dstack #1).
    pub over_latency: u64,
    /// The latency the slot's receiver states now (ARC 0x1100), ns; 0:
    /// unknown.
    pub receiver_latency_ns: u64,
    /// Packets sent past the qdisc ([`bypass`]); the rest of `packets`
    /// went through the UDP socket.
    pub direct: u64,
}

/// The control side's half of [`TxShared`]: the table, and the keepalive
/// counts it has seen per slot.
#[derive(Debug)]
pub struct Control {
    pub table: TxTable,
    seen: [u64; MAX_FLOWS],
    /// The flow sockets' DSCP marks (Windows), following the table.
    marks: super::dscp::Marks,
    /// The neighbour tables as last read (`/proc/net/arp`'s text, the
    /// Windows rows): a flow made between two ticks takes its receiver's MAC
    /// from them at once ([`TxShared::publish`]), not 100 ms later on the UDP
    /// socket (dstack #141).
    arp: String,
    neighbours: Vec<bypass::Neighbour>,
}

/// What the control side and the TX thread share.
#[derive(Debug)]
pub struct TxShared {
    pub cfg: DeviceConfig,
    control: Mutex<Control>,
    plans: [Cell<4>; MAX_FLOWS],
    sources: Box<[Cell<3>]>,
    sockets: Box<[UdpSocket]>,
    /// The send buffer the OS gave the flow sockets, bytes (0: unknown).
    send_buffer: usize,
    /// Each slot's socket's port: a direct packet's source port.
    ports: [u16; MAX_FLOWS],
    /// The device's address: a direct packet's source.
    ip: Ipv4Addr,
    /// The socket past the qdisc and its interface; `None`: every flow
    /// goes through its UDP socket ([`bypass`]).
    direct: Option<(Direct, String)>,
    /// Each slot's destination MAC for a direct send ([`bypass::mac_word`];
    /// 0: through the UDP socket), from the control tick: the ARP table's
    /// past the qdisc, the Windows neighbour table's for a host sender.
    macs: [Cell<1>; MAX_FLOWS],
    /// The host's frame sender and our NIC (dstack #1, Windows), set once
    /// before the TX thread starts ([`TxShared::use_frame_sender`]).
    host: OnceLock<host::HostPath>,
    /// Batches the host sender refused whole, and the last one's OS error
    /// (the send watch logs them, [`super::late::HostWatch`]).
    host_refused: AtomicU64,
    host_error: AtomicI32,
    /// The TX thread's state was built (it read `host` then): a sender
    /// set later is refused. Written under the control lock.
    tx_built: AtomicBool,
    /// The TX thread's last pass had its clock ready (it can send); the
    /// flow server grants no flow before (dstack #20).
    clock_ready: AtomicBool,
    rings: Arc<[Ring]>,
    /// Each TX channel's engine ring (phase C): the engine's pump writes
    /// it, [`Source::Engine`] sends it; `None` for a channel the engine
    /// does not feed.
    engine: Box<[Option<Ring>]>,
    /// The engine rings' line of positions across clock re-anchors
    /// ([`shift`]): the pump and the TX thread read S here.
    pub engine_line: EngineLine,
    lanes: [LaneStats; MAX_FLOWS],
    /// Each slot's receiver latency as the receiver states it (ARC 0x1100,
    /// [`crate::flows::receivers`]): `[dest_word(dest), ns]`, written by
    /// the reader under the control lock, read by the TX thread for the
    /// late check (dstack #1). A word for another destination than the
    /// slot's plan is not this flow's: unknown.
    receivers: [Cell<2>; MAX_FLOWS],
    /// Each TX channel's largest magnitude sent since the last take (the
    /// heartbeat's peaks).
    peaks: Box<[AtomicU32]>,
    /// The latency our TX channels advertise now, ns: the config's, and the
    /// device latency Dante Controller sets when the config gives no
    /// `tx_latency_ns` ([`TxShared::set_device_latency`]).
    advertised_ns: AtomicU64,
    /// The device's media threads' scheduling (dstack #281): the TX and
    /// relay threads and the pump record here, the RX thread through its
    /// own handle ([`TxShared::scheduling`]).
    scheduling: Arc<Scheduling>,
    /// The TX thread's sleep and pass peaks (dstack #359, [`split`]).
    tx_thread: split::TxThreadStats,
    /// The TX pull handed over for the TX thread (dstack #363,
    /// [`TxShared::claim_fill`], `fill.rs`), taken when its state is built.
    fill: Mutex<Option<TxFill>>,
    /// The claimed TX pull's counts, for the status.
    fill_stats: OnceLock<Arc<TxFillStats>>,
    stop: AtomicBool,
    base: Instant,
}

/// The TX flow ports for the start log: `first-last` when they run in a
/// row (a `tx_port_base`), else each one.
fn port_list(ports: &[u16]) -> String {
    match ports {
        [] => String::new(),
        [only] => only.to_string(),
        [first, ..]
            if ports
                .iter()
                .enumerate()
                .all(|(i, &p)| usize::from(p) == usize::from(*first) + i) =>
        {
            format!("{first}-{}", ports[ports.len() - 1])
        }
        _ => ports
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(","),
    }
}

impl TxShared {
    /// The shared state of the device `cfg` at `ip`: an empty table
    /// (cookies from `seed`), every TX channel silent, the RX channels'
    /// `rings` for the echo, an engine ring for each TX channel index in
    /// `engine` (from 0; the standalone device passes none), and one socket
    /// per slot bound to `ip`. An `engine` index the device does not have
    /// is refused (`InvalidInput`).
    pub fn new(
        cfg: DeviceConfig,
        ip: Ipv4Addr,
        rings: Arc<[Ring]>,
        seed: u64,
        engine: &[usize],
    ) -> io::Result<Self> {
        if engine.iter().any(|&tx| tx >= cfg.tx.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "an engine channel past the device's {} TX channels",
                    cfg.tx.len()
                ),
            ));
        }
        let slots = cfg.flow_slots;
        let base = cfg.tx_port_base;
        // Before any bind: a bind in order could fail first on a port
        // Windows excludes, naming that port instead of the base's range.
        if base.is_some_and(|b| super::rx::last_port(b).is_none()) {
            return Err(io::Error::other("tx_port_base: past port 65535"));
        }
        let mut send_buffer = 0;
        let mut multicast_refused = None;
        let sockets = (0..slots.min(MAX_FLOWS))
            .map(|slot| {
                // In range: checked above, before any port is bound.
                let port = base.map_or(0, |b| b + slot as u16);
                let s = UdpSocket::bind(SocketAddrV4::new(ip, port))
                    .map_err(|e| io::Error::new(e.kind(), format!("TX flow port {port}: {e}")))?;
                s.set_nonblocking(true)?;
                crate::winsock::no_connreset(&s)?;
                mark(&s)?;
                // Unicast never depends on it: a refusal leaves multicast
                // flows (dstack #193) on the routing table's interface.
                if let Err(e) = multicast_out(&s, ip) {
                    multicast_refused = Some(e);
                }
                send_buffer = set_send_buffer(&s, SEND_BUFFER_BYTES);
                Ok(s)
            })
            .collect::<io::Result<Box<[UdpSocket]>>>()?;
        if let Some(e) = multicast_refused {
            warn!(
                error = %e,
                %ip,
                "the flow sockets' multicast interface and TTL could not be set: \
                 multicast flows go out on the routing table's interface"
            );
        }
        let mut ports = [0; MAX_FLOWS];
        for (port, s) in ports.iter_mut().zip(&sockets) {
            *port = s.local_addr()?.port();
        }
        info!(
            %ip,
            ports = %port_list(&ports[..sockets.len()]),
            fixed = base.is_some(),
            "TX flow sockets bound"
        );
        Ok(TxShared {
            scheduling: Arc::new(Scheduling::new()),
            send_buffer,
            ports,
            ip,
            direct: bypass::open_for(ip),
            macs: std::array::from_fn(|_| Cell::new()),
            host: OnceLock::new(),
            host_refused: AtomicU64::new(0),
            host_error: AtomicI32::new(0),
            tx_built: AtomicBool::new(false),
            clock_ready: AtomicBool::new(false),
            sources: (0..cfg.tx.len()).map(|_| Cell::new()).collect(),
            peaks: (0..cfg.tx.len()).map(|_| AtomicU32::new(0)).collect(),
            advertised_ns: AtomicU64::new(cfg.advertised_latency_ns()),
            engine: (0..cfg.tx.len())
                .map(|tx| engine.contains(&tx).then(Ring::new))
                .collect(),
            engine_line: EngineLine::default(),
            cfg,
            control: Mutex::new(Control {
                table: TxTable::with_slots(seed, slots),
                seen: [0; MAX_FLOWS],
                marks: super::dscp::Marks::new(),
                arp: String::new(),
                neighbours: Vec::new(),
            }),
            plans: std::array::from_fn(|_| Cell::new()),
            sockets,
            rings,
            lanes: std::array::from_fn(|_| LaneStats::default()),
            receivers: std::array::from_fn(|_| Cell::new()),
            tx_thread: split::TxThreadStats::default(),
            fill: Mutex::new(None),
            fill_stats: OnceLock::new(),
            stop: AtomicBool::new(false),
            base: Instant::now(),
        })
    }

    /// The device's address: where every flow is sent from.
    pub fn ip(&self) -> Ipv4Addr {
        self.ip
    }

    /// Each flow slot's socket's port, one per slot the device opens
    /// (`tx_port_base + n` when set, dstack #262).
    pub fn ports(&self) -> &[u16] {
        &self.ports[..self.sockets.len()]
    }

    /// The control side's lock (never taken by the TX thread).
    pub fn lock(&self) -> MutexGuard<'_, Control> {
        self.control.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The control side's clock: ns since this state was built.
    pub fn now_ns(&self) -> u64 {
        u64::try_from(self.base.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    /// Whether the TX thread's clock was ready at its last pass: it sends
    /// a granted flow's packets ([`Transmitter::step`]).
    pub fn clock_ready(&self) -> bool {
        self.clock_ready.load(Ordering::Relaxed)
    }

    /// The TX thread's clock is `ready`: its [`Transmitter::step`] sets it
    /// each pass (written only when it changes); a host or a test that
    /// steps the TX itself may set it first.
    pub fn set_clock_ready(&self, ready: bool) {
        if self.clock_ready.load(Ordering::Relaxed) != ready {
            self.clock_ready.store(ready, Ordering::Relaxed);
        }
    }

    /// Hands the table's plans to the TX thread (the slots that changed).
    /// The caller holds the lock it got `c` from, so it is the only writer.
    pub fn publish(&self, c: &Control) {
        for (slot, cell) in self.plans.iter().enumerate() {
            let w = c.table.plan(slot).words();
            if cell.peek() != w {
                cell.write(w);
            }
        }
        // Each slot's MAC from the tables as last read: a new flow goes on
        // the direct path from its first packet when its receiver is there.
        if let Some((_, nic)) = &self.direct {
            self.set_macs(c, &c.arp, nic);
        }
        self.set_host_macs(c, &c.neighbours);
    }

    /// The control side's tick: a slot whose receiver was heard since the
    /// last tick is kept alive, flows not heard from for
    /// [`KEEPALIVE_TIMEOUT_NS`] expire, and the plans are handed over.
    /// Returns the slots expired now, one bit each.
    pub fn sync(&self, now: u64) -> u32 {
        let mut c = self.lock();
        for (slot, lane) in self.lanes.iter().enumerate() {
            let heard = lane.keepalives.load(Ordering::Relaxed);
            if heard != c.seen[slot] {
                c.seen[slot] = heard;
                c.table.keepalive(slot, now);
            }
        }
        let expired = c.table.expire(now);
        let dests: [Option<SocketAddrV4>; MAX_FLOWS] =
            std::array::from_fn(|slot| c.table.plan(slot).dest);
        c.marks.follow(&self.sockets, &dests);
        if self.direct.is_some() {
            c.arp = std::fs::read_to_string("/proc/net/arp").unwrap_or_default();
        }
        #[cfg(windows)]
        if self.host.get().is_some() {
            c.neighbours = bypass::windows::neighbours();
        }
        self.publish(&c);
        expired
    }

    /// Ends the live flow in `slot` ([`TxTable::end`]) and hands the plans
    /// to the TX thread, which sends nothing more for it; `false` for an
    /// empty slot or a flow expired already (nothing changed).
    pub fn end_flow(&self, slot: usize) -> bool {
        let mut c = self.lock();
        let ended = c.table.end(slot);
        self.publish(&c);
        ended
    }

    /// Each slot's direct MAC from the neighbour `table` (the text of
    /// `/proc/net/arp`) on `nic` ([`bypass::mac_words`]); a cell is written
    /// only when its MAC changed.
    fn set_macs(&self, c: &Control, table: &str, nic: &str) {
        let dests = (0..MAX_FLOWS).map(|slot| c.table.plan(slot).dest);
        for (cell, w) in self.macs.iter().zip(bypass::mac_words(table, nic, dests)) {
            if cell.peek()[0] != w {
                cell.write([w]);
            }
        }
    }

    /// Slot `slot`'s destination MAC for a direct send, as the control
    /// tick last set it.
    fn slot_mac(&self, slot: usize) -> Option<[u8; 6]> {
        bypass::word_mac(self.macs.get(slot)?.read()?.1[0])
    }

    /// Slot `slot`'s direct socket and MAC, when it goes past the qdisc.
    fn direct_mac(&self, slot: usize) -> Option<(&Direct, [u8; 6])> {
        let (d, _) = self.direct.as_ref()?;
        Some((d, self.slot_mac(slot)?))
    }

    /// The device latency is `ns` now (Dante Controller set it): our TX
    /// channels advertise it unless the config sets `tx_latency_ns`. The
    /// TX thread takes it at its next events pass.
    pub fn set_device_latency(&self, ns: u64) {
        if self.cfg.tx_latency_ns.is_none() {
            self.advertised_ns.store(ns, Ordering::Relaxed);
        }
    }

    /// The latency our TX channels advertise now, ns.
    pub fn advertised_latency_ns(&self) -> u64 {
        self.advertised_ns.load(Ordering::Relaxed)
    }

    /// Sets TX channel `tx`'s source (from 0); `false` when there is no
    /// such channel, or for [`Source::Engine`] on a channel without an
    /// engine ring. Runtime only: every channel is silent at start.
    pub fn set_source(&self, tx: usize, source: Source) -> bool {
        let _writer = self.lock();
        let Some(cell) = self.sources.get(tx) else {
            return false;
        };
        if source == Source::Engine && self.engine_ring(tx).is_none() {
            return false;
        }
        cell.write(source_words(source));
        true
    }

    /// The device's media threads' scheduling (dstack #281): what each
    /// thread got of its priority; the RX thread is handed it at its spawn.
    pub fn scheduling(&self) -> &Arc<Scheduling> {
        &self.scheduling
    }

    /// TX channel `tx`'s engine ring (from 0; phase C): what the engine's
    /// pump writes and [`Source::Engine`] sends. `None` for a channel the
    /// engine does not feed.
    pub fn engine_ring(&self, tx: usize) -> Option<&Ring> {
        self.engine.get(tx)?.as_ref()
    }

    /// TX channel `tx`'s source (from 0).
    pub fn source(&self, tx: usize) -> Option<Source> {
        Some(source_from(self.sources.get(tx)?.read()?.1))
    }

    /// TX channel `tx`'s largest magnitude sent since the last call (from
    /// 0; 0 when nothing was sent, or no such channel).
    pub fn take_peak(&self, tx: usize) -> u32 {
        self.peaks
            .get(tx)
            .map_or(0, |p| p.swap(0, Ordering::Relaxed))
    }

    /// Slot `slot`'s oldest packet since the last call, ns (0: none sent).
    pub fn take_age_peak(&self, slot: usize) -> u64 {
        self.lanes
            .get(slot)
            .map_or(0, |l| l.age_peak_ns.swap(0, Ordering::Relaxed))
    }

    /// Slot `slot`'s packets over their receiver's latency that the relay
    /// thread sent, covering the TX thread's bounded escapes, since the
    /// device started (dstack #177).
    pub fn over_latency_relayed(&self, slot: usize) -> u64 {
        self.lanes
            .get(slot)
            .map_or(0, |l| l.over_latency_relayed.load(Ordering::Relaxed))
    }

    /// The shortest time from the last bounded escape's start to a packet
    /// of slot `slot` over its receiver's latency, since the last call, ns
    /// (`None`: no such packet, or no escape before it; dstack #177).
    pub fn take_since_escape(&self, slot: usize) -> Option<u64> {
        let kept = self
            .lanes
            .get(slot)
            .map_or(0, |l| l.since_escape.swap(0, Ordering::Relaxed));
        (kept != 0).then(|| u64::MAX - kept)
    }

    /// Slot `slot`'s counts.
    pub fn lane(&self, slot: usize) -> LaneCounts {
        let Some(l) = self.lanes.get(slot) else {
            return LaneCounts::default();
        };
        LaneCounts {
            packets: l.packets.load(Ordering::Relaxed),
            bytes: l.bytes.load(Ordering::Relaxed),
            send_errors: l.send_errors.load(Ordering::Relaxed),
            send_retries: l.send_retries.load(Ordering::Relaxed),
            last_os_error: l.last_os_error.load(Ordering::Relaxed),
            keepalives: l.keepalives.load(Ordering::Relaxed),
            rebootstraps: l.rebootstraps.load(Ordering::Relaxed),
            holes: l.holes.load(Ordering::Relaxed),
            late_bins: std::array::from_fn(|i| l.late[i].load(Ordering::Relaxed)),
            late_max_ns: l.late_max_ns.load(Ordering::Relaxed),
            send_max_ns: l.send_max_ns.load(Ordering::Relaxed),
            age_max_ns: l.age_max_ns.load(Ordering::Relaxed),
            over_latency: l.over_latency.load(Ordering::Relaxed),
            receiver_latency_ns: self
                .plan_dest(slot)
                .map_or(0, |dest| self.receiver_latency_ns(slot, dest)),
            direct: l.direct.load(Ordering::Relaxed),
        }
    }

    /// The TX thread's sleep and pass peaks since the start (dstack #359).
    pub fn tx_thread(&self) -> split::TxThreadView {
        self.tx_thread.view()
    }

    /// Every slot's bytes on the wire since the device started.
    pub fn bytes_sent(&self) -> u64 {
        self.lanes
            .iter()
            .map(|l| l.bytes.load(Ordering::Relaxed))
            .sum()
    }

    /// The TX thread's peaks, to record as the TX thread does (other
    /// modules' tests: the status's `tx_thread`).
    #[cfg(test)]
    pub(crate) fn tx_thread_stats(&self) -> &split::TxThreadStats {
        &self.tx_thread
    }

    /// Counts a sent packet of `len` datagram bytes on slot `slot`, as the
    /// TX thread does (other modules' tests: the status's `tx_bytes`).
    #[cfg(test)]
    pub(crate) fn count_sent(&self, slot: usize, len: usize) {
        count_send(&self.lanes[slot], &Ok(false), len);
    }

    /// Where slot `slot`'s audio comes from and its keepalives go.
    pub fn local_addr(&self, slot: usize) -> io::Result<SocketAddr> {
        self.sockets
            .get(slot)
            .ok_or_else(|| io::Error::other("no such slot"))?
            .local_addr()
    }

    /// The RX channels' rings the echo reads.
    pub fn rings(&self) -> &Arc<[Ring]> {
        &self.rings
    }

    /// Asks the TX thread to end (within one of its sleeps, at most
    /// [`EVENTS_EVERY_NS`]).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    /// The send buffer the OS gave the flow sockets, bytes (0: unknown).
    pub fn send_buffer(&self) -> usize {
        self.send_buffer
    }

    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

/// A duration in whole ns, saturating.
fn nanos(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

/// An audio packet's bytes on the wire past its datagram: the Ethernet,
/// IPv4 and UDP headers (14 + 20 + 8), as the NIC's counter counts them.
pub const WIRE_OVERHEAD: u64 = 42;

/// Counts one send's outcome in `stats`: a packet of `len` datagram bytes
/// (and a direct one when it went past the qdisc), or a send error.
fn count_send(stats: &LaneStats, sent: &io::Result<bool>, len: usize) {
    match sent {
        Ok(direct) => {
            stats.packets.fetch_add(1, Ordering::Relaxed);
            stats
                .bytes
                .fetch_add(len as u64 + WIRE_OVERHEAD, Ordering::Relaxed);
            if *direct {
                stats.direct.fetch_add(1, Ordering::Relaxed);
            }
        }
        Err(e) => {
            stats
                .last_os_error
                .store(e.raw_os_error().unwrap_or(-1), Ordering::Relaxed);
            stats.send_errors.fetch_add(1, Ordering::Relaxed);
        }
    }
}

mod priority;
pub mod relay;
pub mod split;
#[cfg(all(test, windows))]
use priority::ask_priority;
#[cfg(test)]
use priority::pin_to_cpu;
pub use priority::{
    MediaThread, MmcssError, NO_MMCSS, NO_MMCSS_TASK, Priority, Scheduled, Scheduling,
    WINDOWS_HIGHEST, WINDOWS_TIME_CRITICAL, join_mmcss, joined_task, thread_priority,
    windows_priority_for, windows_thread_priority,
};
pub(crate) use priority::{place, take_priority};
mod schedule;
pub use schedule::{Schedule, flow_latency_ns, ns_of, over_latency, receiver_lag, send_clock};
mod table;
pub use table::{TxFlow, TxTable};
mod thread;
pub use thread::{RELAY_TAKE_MAX, Transmitter, spawn};
#[cfg(test)]
use thread::{escape_through, run_relay, run_with, send_one};
pub mod shift;
pub use shift::{EngineLine, Shift};
pub mod host;
pub use host::{HostNic, TxPath};
mod fill;
pub use fill::{
    BUILT, CLAIMED, FillWatch, NO_RING, Refused, TX_PULL_WARNING, TxFill, TxFillStats, TxFillView,
    engine_channels,
};
mod pass;
use pass::{PassClock, Sent, account, send_timed};
mod refused;
mod retry;
pub use retry::{
    SEND_RETRIES, SEND_RETRY_HEADROOM_NS, SEND_RETRY_WAIT_NS, nic_refusal, send_retry,
    transient_refusal,
};

mod direct;
mod multicast;
mod receivers;
use multicast::multicast_out;
pub use multicast::{MULTICAST_PORT, MULTICAST_TTL, multicast_group};
pub use receivers::{AppView, ReceiverLateness};

#[cfg(test)]
mod tests;
