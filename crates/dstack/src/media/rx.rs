// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.
// Protocol facts (byte layouts, opcodes, behaviour) checked against
// inferno (https://github.com/teodly/inferno) as a reference; the code is
// our own.

//! Our RX flows (Gen 3 phase B, plan Task 5): the thread that receives the
//! audio transmitters send us, and what it shares with the control side.
//!
//! - [`RxShared`]: [`MAX_FLOWS`] flow slots, each a UDP socket on our
//!   address (an ephemeral port, a [`RECV_BUFFER_BYTES`] receive buffer
//!   asked for) whose port the flow request names. The control side
//!   ([`crate::subscribe::Manager`]) hands each slot's [`RxPlan`] (the
//!   transmitter, the width, the channel slots, the latency, whether it was
//!   granted) and each RX channel's [`RxRoute`] (which flow slot and channel
//!   slot feed its ring) over through seqlocked atomics
//!   ([`RxShared::publish`]); the thread never blocks on them.
//! - The rings' one writer, [`Intake`] (dstack #326, `rx/intake.rs`): the
//!   RX thread's pass in the low-latency profile and on a device without
//!   an engine pump; the engine's pump in the normal profile, which drains
//!   the flow sockets itself just before it reads each block
//!   ([`RxShared::pump_intake`]), so a helper thread the scheduler leaves
//!   behind can no longer write samples behind the reader; or the app's
//!   own consumer (dstack #307, [`RxShared::app_intake`]: an output
//!   callback that drains and reads the rings by position itself, dlink's
//!   `bridge::RxPull`). Chosen once at
//!   the start; the RX thread then keeps the groups' joins, the keepalives
//!   and the rejoins, and polls no flow socket. What follows about reading
//!   and placing a packet is the writer's, whichever thread runs it.
//! - [`Receiver`] and [`spawn`]: the thread. While it writes the rings it
//!   waits on every flow socket at once (mio's poll), at most [`WAKE_NS`]
//!   at a time (under the pump only for its keepalives). A datagram counts
//!   only from its slot's transmitter (by address; anything else is
//!   `stray`); a well-formed one is a packet: its stamp, read as the sample
//!   index nearest our clock's now ([`crate::clock::from_stamp`], across the 32-bit
//!   seconds wrap and any uptime), places each channel slot's samples in
//!   the rings of the RX channels routed to it ([`super::ring::Ring`]: a
//!   missing packet reads as a hole, a late one is the ring's to count). Its
//!   lateness (our now less its first sample) is measured, the largest kept
//!   until read; one later than the flow's latency counts `late`, one
//!   before the end of the last `out_of_order`. Before our clock is ready a
//!   packet is read and counted but placed nowhere (`unplaced`).
//! - Holes, reorders and gaps (dstack #278, `rx/holes.rs`): a packet that
//!   begins past the furthest end opens a hole; one that lands in an open
//!   hole fills it and counts `reordered` (overtaken on the way, not lost).
//!   A hole counts as a gap (`gaps`, `gap_frames`) only once it is wholly
//!   behind the flow's playout point (each pass looks), the oldest of 16
//!   when another opens, or when the slot's flow changes.
//! - Wire lateness against the wake (dstack #234, Linux): each datagram
//!   comes with the kernel's receive time (`rx/socket.rs`), so its wake delay
//!   (the pass's wall clock less that time) is kept, the largest until
//!   read and since the start, and a packet already late on the wire (by
//!   `late`'s rule, at the pass's time less its wake delay) counts
//!   `wire_late`. Windows has no stamp: both stay 0.
//! - The measured network latency (dstack #244, Linux): each such packet's
//!   arrival on our PTP clock (the pass's now less its wake delay) less its
//!   own stamp, as min, p99 and max of each heartbeat second, from a fixed
//!   histogram of the slot (`rx/netlat.rs`;
//!   [`RxShared::last_network_latency`]). Windows records none.
//! - The minute (dstack #234, Linux): the wake delays (`rx/wake.rs`) and
//!   network latencies of each slot in windows the RX minute watch takes
//!   once a minute ([`RxShared::take_minute`]) and logs, for apps without
//!   a status socket; and each packet's write delay (the kernel's receive
//!   time to the end of its ring writes, one more wall-clock read per
//!   stamped packet), which also sees a stall inside a pass
//!   ([`RxShared::take_write_minute`]).
//! - Start holes (dstack #234): a publish of a new flow in a slot (another
//!   transmitter or generation, `new_flow`), or of a channel routed
//!   anew, restarts the channel's ring ([`Ring::restart`]), so the holes
//!   read before that flow's first sample count apart from a running
//!   flow's ([`RxShared::flow_rings`]). The publish restarts it before the
//!   route is stored (for the reader), the thread again at its next
//!   refresh, before it writes (an old flow's packet written in between
//!   would start it early).
//! - The packets' judge (phase D, plan Task 3): the least lateness of each
//!   flow slot's placed packets in ns (each packet's arrival on our clock
//!   in ns less its own stamp in ns: dstack #350, never the wait to its
//!   read, `intake::record_arrivals`), kept until read, so each heartbeat second
//!   has its own ([`RxShared::last_min_lateness_ns`]). Every Dante packet
//!   carries its sender's PTP-locked media time, so the least of a second
//!   measures the box's clock against the grandmaster's from the audio
//!   itself, with no tool of either side in between.
//! - Keepalives: `13 37` from the slot's socket to the address its audio
//!   comes from, every [`KEEPALIVE_NS`] (four a second, never more: a late
//!   one moves the schedule on, it never catches up), only for a flow its
//!   transmitter granted and whose audio came within [`LIVE_NS`]. Nothing
//!   is ever sent to a source that sent no audio of a granted flow.
//!
//! The per-packet path (the receive, the parse, the ring writes, the
//! judge's one `fetch_min`, a keepalive's send) allocates nothing, takes no
//! lock and logs nothing (`tests/hot_path.rs`, the pump's drain too; the
//! packet's placing under rtsan, `tests/rtsan.rs`); every count is an
//! atomic of its slot ([`RxCounts`]).

use std::io::{self, ErrorKind};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mio::{Events, Interest, Poll, Token};
use tracing::{info, warn};

use super::packet::MEDIA_HEADER;
use super::ring::{Counters, Ring};
use super::tx::{Cell, MediaThread, Scheduling, place, take_priority};
use crate::clock::PtpClock;
use crate::wire::MAX_DATAGRAM;

mod age;
mod claim;
mod counts;
mod handles;
mod holes;
mod intake;
mod lane;
mod minute;
mod netlat;
mod plan;
mod publish;
mod rejoin;
mod wake;
pub use counts::RxCounts;
use counts::{NO_LATENESS, RxStats, source_of, source_word};
use handles::Handles;
pub use intake::{Intake, PUMP_DRAIN};
use lane::Keep;
mod socket;
use intake::discard;
pub use netlat::NetLatency;
pub use plan::{RxPlan, RxRoute};
pub use socket::{GROUP_BOUND_TO_PORT, join};
use socket::{handle_of, recv_stamped, set_recv_buffer, stamp_receives, wall_now_ns};
pub use wake::WakeDelay;

/// Our RX flow slots (inferno's `MAX_FLOWS`).
pub const MAX_FLOWS: usize = 32;
/// A keepalive's bytes (inferno's).
pub const KEEPALIVE: [u8; 2] = [0x13, 0x37];
/// A granted flow's keepalives, one per this (inferno's 250 ms).
pub const KEEPALIVE_NS: u64 = 250_000_000;
/// Keepalives go out only while the flow's audio came within this.
pub const LIVE_NS: u64 = 2_000_000_000;
/// The receive buffer asked for each flow socket (the kernel gives at most
/// its `rmem_max`).
pub const RECV_BUFFER_BYTES: usize = 4 << 20;
/// The longest the thread waits for a datagram: it reads the stop flag and
/// new plans at least this often.
pub const WAKE_NS: u64 = 33_000_000;
/// The shortest time from one RX pass's start to the next on Linux, ns:
/// datagrams that arrive meanwhile wait in their sockets and the next pass
/// reads them together, instead of a wake per datagram (gen3 on the N100
/// box, 31.10.2026: 7 stage flows at fpp 4 woke the thread 168 k times a
/// second on the core it shares with TX). A datagram is read at most this
/// much later, a fifth of the lowest latency we receive at (250 µs).
pub const PASS_NS: u64 = 50_000;
const _: () = assert!(PASS_NS * 5 <= 250_000, "a fifth of the lowest RX latency");

/// How long to rest after a pass that began at `began` (the thread's clock,
/// ns) when it is `now`: up to [`PASS_NS`] from the start, 0 past it.
pub fn rest_ns(began: u64, now: u64) -> u64 {
    began.saturating_add(PASS_NS).saturating_sub(now)
}

/// The most datagrams read from one socket in a pass (a flood never holds
/// the thread); the rest wait for the next pass, at once.
pub const DRAIN: usize = 64;
/// The most datagrams the RX thread drops from a slot without a flow at one
/// wake under the pump (dstack #326): a whole receive buffer of small
/// ones, so an unsubscribed transmitter still sending (about 100 a wake)
/// leaves no backlog for the slot's next flow.
pub const UNPLANNED_DRAIN: usize = 4096;
/// The RX thread's `SCHED_FIFO` priority when `RLIMIT_RTPRIO` allows it:
/// below the TX thread's (a send preempts a receive) and above the engine
/// pump's.
pub const RX_PRIORITY: i32 = 44;
/// The most frames one channel slot of a packet holds (16-bit, one slot).
const FRAMES_MAX: usize = (MAX_DATAGRAM - MEDIA_HEADER) / 2;
/// A datagram read; one longer than [`MAX_DATAGRAM`] is refused.
const BUFFER: usize = 2048;

/// The drops of the UDP socket bound to `ip`:`port` in a `/proc/net/udp`
/// table: its last column. The local address is the address's bytes in
/// the host's order as hex, then the port as hex (`0100007F:1F90` for
/// 127.0.0.1:8080 on a little-endian host). `None` for no such socket.
pub fn udp_drops(table: &str, ip: Ipv4Addr, port: u16) -> Option<u64> {
    let want = format!("{:08X}:{port:04X}", u32::from_ne_bytes(ip.octets()));
    table.lines().skip(1).find_map(|line| {
        let mut cols = line.split_whitespace();
        let local = cols.nth(1)?;
        (local == want).then(|| line.split_whitespace().last()?.parse().ok())?
    })
}

/// What the control side and the RX thread share.
#[derive(Debug)]
pub struct RxShared {
    rate: u32,
    plans: [Cell<3>; MAX_FLOWS],
    routes: Box<[AtomicU64]>,
    /// Each RX channel's read delay ([`RxPlan::delay`] of its flow), for
    /// the engine's pump.
    delays: Box<[AtomicU32]>,
    /// The device latency the delays are planned against, samples, as the
    /// manager last published it (0: none yet): the engine's pump plays at
    /// it ([`RxShared::playout`]).
    playout: AtomicU64,
    routes_version: AtomicU32,
    /// Moves once per [`RxShared::rejoin_groups`]: the RX thread joins
    /// every slot's group again when it sees it move (dstack #282).
    rejoins: AtomicU32,
    /// After the RX thread acted on a rejoin: a slot's group could not be
    /// joined (the daemon asks again; [`RxShared::take_groups_missing`]).
    groups_missing: AtomicBool,
    sockets: Box<[UdpSocket]>,
    /// Our address: the multicast groups are joined on it.
    ip: Ipv4Addr,
    rings: Arc<[Ring]>,
    /// One per flow slot ([`MAX_FLOWS`]), on the heap: each holds its
    /// histograms (about 5 KB, dstack #234), too much for a stack in all.
    lanes: Box<[RxStats]>,
    /// Held by [`RxShared::publish`] only (one writer at a time); the RX
    /// thread never takes it.
    writer: Mutex<()>,
    poll_errors: AtomicU64,
    stop: AtomicBool,
    base: Instant,
    recv_buffer: usize,
    /// Every slot socket stamps its receive times (Linux): the passes
    /// measure wake delays and wire lateness.
    stamps: AtomicBool,
    /// Until when (the thread's clock, [`RxShared::now_ns`]) the RX thread
    /// stalls before its next pass ([`RxShared::stall_thread`]; 0: none).
    stall_until: AtomicU64,
    /// Who writes the rings (dstack #326): unset until the
    /// engine's pump ([`RxShared::pump_intake`]), the app
    /// ([`RxShared::app_intake`], dstack #307) or the RX thread
    /// ([`Receiver::new`]) claims it, once for the device's life.
    ring_writer: AtomicU8,
    /// Each slot's group socket for the writer, by its handle.
    handles: Handles,
    /// The group sockets the RX thread held when it ended while the pump
    /// writes: closed with this state, after the pump's last read.
    parked: Mutex<Vec<mio::net::UdpSocket>>,
}

/// The last of the [`MAX_FLOWS`] ports from `base`, `None` past 65535.
pub(crate) fn last_port(base: u16) -> Option<u16> {
    base.checked_add(MAX_FLOWS as u16 - 1)
}

impl RxShared {
    /// The flow slots of a device at `ip` running at `rate`: one socket each
    /// bound to `ip`, and the RX channels' `rings` (one per RX channel,
    /// shared with the TX side's echo).
    pub fn new(rate: u32, ip: Ipv4Addr, rings: Arc<[Ring]>) -> io::Result<Self> {
        Self::new_at(rate, ip, rings, None)
    }

    /// [`RxShared::new`] with slot `n`'s socket on port `base + n` when
    /// `base` is given ([`crate::device::DeviceConfig::rx_port_base`]); a
    /// port taken is an error naming it.
    pub fn new_at(
        rate: u32,
        ip: Ipv4Addr,
        rings: Arc<[Ring]>,
        base: Option<u16>,
    ) -> io::Result<Self> {
        Self::with_slots(rate, ip, rings, base, MAX_FLOWS)
    }

    /// [`RxShared::new_at`] with sockets for slots 0 to `slots` only (the
    /// device's `flow_slots`, at most [`MAX_FLOWS`]).
    pub fn with_slots(
        rate: u32,
        ip: Ipv4Addr,
        rings: Arc<[Ring]>,
        base: Option<u16>,
        slots: usize,
    ) -> io::Result<Self> {
        if base.is_some_and(|b| last_port(b).is_none()) {
            return Err(io::Error::other("rx_port_base: past port 65535"));
        }
        let mut recv_buffer = 0;
        let sockets = (0..slots.min(MAX_FLOWS))
            .map(|slot| {
                // In range: checked above, before any port is bound.
                let port = base.map_or(0, |b| b + slot as u16);
                let s = UdpSocket::bind(SocketAddrV4::new(ip, port))
                    .map_err(|e| io::Error::new(e.kind(), format!("RX flow port {port}: {e}")))?;
                s.set_nonblocking(true)?;
                crate::winsock::no_connreset(&s)?;
                recv_buffer = set_recv_buffer(&s, RECV_BUFFER_BYTES);
                Ok(s)
            })
            .collect::<io::Result<Box<[UdpSocket]>>>()?;
        let stamps = sockets.iter().all(stamp_receives);
        Ok(RxShared {
            rate,
            plans: std::array::from_fn(|_| Cell::new()),
            routes: (0..rings.len()).map(|_| AtomicU64::new(0)).collect(),
            delays: (0..rings.len()).map(|_| AtomicU32::new(0)).collect(),
            routes_version: AtomicU32::new(0),
            rejoins: AtomicU32::new(0),
            groups_missing: AtomicBool::new(false),
            sockets,
            ip,
            rings,
            lanes: (0..MAX_FLOWS).map(|_| RxStats::default()).collect(),
            writer: Mutex::new(()),
            poll_errors: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            base: Instant::now(),
            recv_buffer,
            stamps: AtomicBool::new(stamps),
            playout: AtomicU64::new(0),
            stall_until: AtomicU64::new(0),
            ring_writer: AtomicU8::new(claim::WRITER_UNSET),
            handles: Handles::new(),
            parked: Mutex::new(Vec::new()),
        })
    }

    /// The device latency is `ns`: the pump's playout point is its whole
    /// samples ([`crate::clock::latency_samples`], as [`crate::stream::Timing::of`]).
    pub fn set_device_latency(&self, ns: u64) {
        self.playout.store(
            crate::clock::latency_samples(ns, self.rate),
            Ordering::Relaxed,
        );
    }

    /// The device latency the published delays are planned against,
    /// samples; `None` before the first publish.
    pub fn playout(&self) -> Option<u64> {
        Some(self.playout.load(Ordering::Relaxed)).filter(|&p| p > 0)
    }

    /// How much later than the device latency the engine's pump reads RX
    /// channel `rx` (from 0), samples: its flow's [`RxPlan::delay`] (one
    /// `Relaxed` load).
    pub fn delay(&self, rx: usize) -> u64 {
        self.delays
            .get(rx)
            .map_or(0, |d| u64::from(d.load(Ordering::Relaxed)))
    }

    /// Whether RX channel `rx` (from 0) is routed to a flow slot (its route
    /// atomic, `Relaxed`): the engine's pump ([`crate::stream`]) reads only
    /// a routed channel's ring, so an unsubscribed input is silence and
    /// counts no hole.
    pub fn routed(&self, rx: usize) -> bool {
        self.routes
            .get(rx)
            .is_some_and(|r| r.load(Ordering::Relaxed) != 0)
    }

    /// Each flow slot's UDP port, where its transmitter sends the audio.
    pub fn ports(&self) -> io::Result<[u16; MAX_FLOWS]> {
        let mut ports = [0; MAX_FLOWS];
        for (port, s) in ports.iter_mut().zip(self.sockets.iter()) {
            *port = s.local_addr()?.port();
        }
        Ok(ports)
    }

    /// Flow slot `slot`'s address.
    pub fn local_addr(&self, slot: usize) -> io::Result<SocketAddr> {
        self.sockets
            .get(slot)
            .ok_or_else(|| io::Error::other("no such slot"))?
            .local_addr()
    }

    /// Flow slot `slot`'s counts.
    pub fn lane(&self, slot: usize) -> RxCounts {
        let Some(l) = self.lanes.get(slot) else {
            return RxCounts::default();
        };
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let (lateness_sum_ns, lateness_count) = l.ages.totals();
        let (read_wait_sum_ns, read_wait_count) = l.read_waits.totals();
        RxCounts {
            packets: get(&l.packets),
            refused: get(&l.refused),
            stray: get(&l.stray),
            out_of_order: get(&l.out_of_order),
            reordered: get(&l.reordered),
            gaps: get(&l.gaps),
            gap_frames: get(&l.gap_frames),
            late: get(&l.late),
            late_at_read: get(&l.late_at_read),
            wire_late: get(&l.wire_late),
            lateness_peak: get(&l.lateness_peak),
            lateness_at_read_peak: get(&l.lateness_at_read_peak),
            lateness_sum_ns,
            lateness_count,
            read_wait_peak_us: get(&l.read_wait_peak_ns) / 1_000,
            read_wait_sum_ns,
            read_wait_count,
            wake_delay_peak_us: get(&l.wake_peak_ns) / 1_000,
            unplaced: get(&l.unplaced),
            keepalives: get(&l.keepalives),
            keepalive_errors: get(&l.keepalive_errors),
            recv_errors: get(&l.recv_errors),
            joins: get(&l.joins),
            join_errors: get(&l.join_errors),
        }
    }

    /// Where flow slot `slot`'s audio comes from: its transmitter's address
    /// and port as the last packet came (dstack #266); `None` before a
    /// packet of its flow, and after its plan changed.
    pub fn source(&self, slot: usize) -> Option<SocketAddrV4> {
        source_of(self.lanes.get(slot)?.source.load(Ordering::Relaxed))
    }

    /// Each flow slot's datagrams the kernel dropped at its socket (a full
    /// receive buffer), from `/proc/net/udp` (Linux; `None` elsewhere or
    /// when the table cannot be read).
    pub fn socket_drops(&self) -> Option<[u64; MAX_FLOWS]> {
        #[cfg(target_os = "linux")]
        {
            let table = std::fs::read_to_string("/proc/net/udp").ok()?;
            let ports = self.ports().ok()?;
            let ip = match self.sockets.first()?.local_addr().ok()? {
                SocketAddr::V4(a) => *a.ip(),
                SocketAddr::V6(_) => return None,
            };
            Some(std::array::from_fn(|slot| {
                udp_drops(&table, ip, ports[slot]).unwrap_or(0)
            }))
        }
        #[cfg(not(target_os = "linux"))]
        None
    }

    /// Every flow slot's packets (the manager's [`crate::subscribe::Manager::tick`]).
    pub fn packets(&self) -> [u64; MAX_FLOWS] {
        std::array::from_fn(|slot| self.lanes[slot].packets.load(Ordering::Relaxed))
    }

    /// Flow slot `slot`'s largest lateness since the last call, samples
    /// (the heartbeat's 0x8003, plan Task 6); kept for
    /// [`RxShared::last_lateness`]. It also takes the packets' least
    /// lateness in ns, kept for [`RxShared::last_min_lateness_ns`], and
    /// the largest wake delay, kept for [`RxShared::last_wake_delay_us`],
    /// and the second's mean age and mean read wait, kept for
    /// [`RxShared::last_lateness_mean_ns`] and
    /// [`RxShared::last_read_wait_mean_ns`], so the heartbeat's second is all
    /// of theirs. Only the heartbeat calls it: a second caller would split
    /// its seconds.
    pub fn take_max_lateness(&self, slot: usize) -> u64 {
        self.lanes.get(slot).map_or(0, |l| {
            let wake = l.max_wake_ns.swap(0, Ordering::Relaxed);
            l.last_wake_ns.store(wake, Ordering::Relaxed);
            l.network.take();
            l.ages.take();
            l.read_waits.take();
            let least = l.min_lateness_ns.0.swap(NO_LATENESS, Ordering::Relaxed);
            l.last_min_lateness_ns.0.store(least, Ordering::Relaxed);
            let taken = l.max_lateness.swap(0, Ordering::Relaxed);
            l.last_lateness.store(taken, Ordering::Relaxed);
            taken
        })
    }

    /// What the last [`RxShared::take_max_lateness`] of flow slot `slot`
    /// took (the heartbeat's last second), samples: the control socket's
    /// status reads it and takes nothing (plan Task 7).
    pub fn last_lateness(&self, slot: usize) -> u64 {
        self.lanes
            .get(slot)
            .map_or(0, |l| l.last_lateness.load(Ordering::Relaxed))
    }

    /// The largest wake delay of flow slot `slot`'s packets in the
    /// heartbeat's last second (what the last [`RxShared::take_max_lateness`]
    /// took), µs: the RX pass's time less the kernel's receive time (dstack
    /// #234; 0 without receive stamps). The status reads it and takes
    /// nothing.
    pub fn last_wake_delay_us(&self, slot: usize) -> u64 {
        self.lanes
            .get(slot)
            .map_or(0, |l| l.last_wake_ns.load(Ordering::Relaxed) / 1_000)
    }

    /// The measured network latency of flow slot `slot`'s packets in the
    /// heartbeat's last second (what the last
    /// [`RxShared::take_max_lateness`] took; dstack #244): the least, the
    /// 99th percentile and the largest of each packet's arrival (the
    /// kernel's receive time on our PTP clock) less its own stamp. `None`
    /// for a second without a stamped packet (always on Windows, where the
    /// kernel stamps no receive time). The status reads it and takes
    /// nothing.
    pub fn last_network_latency(&self, slot: usize) -> Option<NetLatency> {
        self.lanes.get(slot)?.network.last()
    }

    /// The device's measured network latency for an app's status (dstack
    /// #325, the ASIO bridge's receiver figure): the largest p99 of any
    /// flow slot's last second ([`RxShared::last_network_latency`]), ns;
    /// `None` while no slot measured one (always on Windows).
    pub fn network_p99_ns(&self) -> Option<u64> {
        self.lanes
            .iter()
            .filter_map(|l| l.network.last())
            .map(|n| n.p99_us * 1_000)
            .max()
    }

    /// Whether every slot socket stamps its datagrams' receive times, so
    /// `wire_late` and the wake delays are measured (dstack #234: Linux,
    /// where the kernel takes `SO_TIMESTAMPNS`; false on Windows). A
    /// multicast group's socket asks for the stamp too ([`join`]).
    pub fn stamps(&self) -> bool {
        self.stamps.load(Ordering::Relaxed)
    }

    /// Turns the measuring off as on a box without receive stamps: the
    /// Windows path's behaviour, for a Linux test.
    #[cfg(test)]
    pub(crate) fn set_stamps(&self, on: bool) {
        self.stamps.store(on, Ordering::Relaxed);
    }

    /// The packets' judge of flow slot `slot` (phase D, plan Task 3): the
    /// least lateness in ns of the packets placed in the heartbeat's last
    /// second (what the last [`RxShared::take_max_lateness`] took: our
    /// clock's now in ns less each packet's own stamp in ns, 0 for a packet
    /// stamped after our now); `None` for a second without a placed packet.
    /// The control socket's status reads it and takes nothing.
    pub fn last_min_lateness_ns(&self, slot: usize) -> Option<u64> {
        let least = self
            .lanes
            .get(slot)?
            .last_min_lateness_ns
            .0
            .load(Ordering::Relaxed);
        (least != NO_LATENESS).then_some(least)
    }

    /// The mean age of flow slot `slot`'s packets placed in the
    /// heartbeat's last second (what the last
    /// [`RxShared::take_max_lateness`] took; dstack #342), ns, rounded
    /// down: our clock's now less each packet's own stamp, the figure
    /// whose least is [`RxShared::last_min_lateness_ns`]; an age past
    /// 4.29 s counts as 4.29 s. `None` for a second without a placed
    /// packet. The status reads it and takes nothing; a reader with its
    /// own window diffs [`RxCounts`] ([`RxCounts::lateness_mean_ns_since`]).
    pub fn last_lateness_mean_ns(&self, slot: usize) -> Option<u64> {
        self.lanes.get(slot)?.ages.last_mean_ns()
    }

    /// The mean wait from arrival to the writer's read of flow slot
    /// `slot`'s packets placed in the heartbeat's last second (what the
    /// last [`RxShared::take_max_lateness`] took; dstack #350), ns,
    /// rounded down: the part of the age at the read that the reported
    /// ages leave out (an app's pull, the pump's block). `None` for a
    /// second without a placed packet. The status reads it and takes
    /// nothing.
    pub fn last_read_wait_mean_ns(&self, slot: usize) -> Option<u64> {
        self.lanes.get(slot)?.read_waits.last_mean_ns()
    }

    /// The receive buffer the kernel gave the flow sockets, bytes.
    pub fn recv_buffer(&self) -> usize {
        self.recv_buffer
    }

    /// The thread's failed waits.
    pub fn poll_errors(&self) -> u64 {
        self.poll_errors.load(Ordering::Relaxed)
    }

    /// The RX channels' rings.
    pub fn rings(&self) -> &Arc<[Ring]> {
        &self.rings
    }

    /// The control side's clock and the thread's for keepalives: ns since
    /// this state was built.
    pub fn now_ns(&self) -> u64 {
        u64::try_from(self.base.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    /// Asks the RX thread to end (within [`WAKE_NS`]).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Stalls the RX thread for `ns` before its next pass, as the
    /// scheduler leaves a normal-priority thread behind on a loaded box
    /// (dstack #326: strih-snv, up to 17.5 ms). For tests of the receive
    /// path under such a stall; a running device never calls it.
    #[doc(hidden)]
    pub fn stall_thread(&self, ns: u64) {
        self.stall_until
            .fetch_max(self.now_ns().saturating_add(ns), Ordering::AcqRel);
    }

    /// The RX thread's side of [`RxShared::stall_thread`]: sleeps out a
    /// stall asked for and not yet over; `true` when it slept.
    fn serve_stall(&self) -> bool {
        let rest = self
            .stall_until
            .load(Ordering::Acquire)
            .saturating_sub(self.now_ns());
        if rest == 0 {
            return false;
        }
        std::thread::sleep(Duration::from_nanos(rest));
        true
    }
}

/// The RX thread's state: everything it needs, allocated before its loop.
/// The ring writer ([`Intake`]) is its own while the RX thread writes the
/// rings; under the engine's pump (the normal profile, dstack #326) it is
/// the pump's, and this thread keeps the groups, the keepalives and the
/// rejoins only, polling no flow socket.
#[derive(Debug)]
pub struct Receiver {
    shared: Arc<RxShared>,
    poll: Poll,
    events: Events,
    /// The flow sockets (the shared ones' duplicates: registered with the
    /// poll while this thread writes; the keepalives go out on them).
    sockets: Vec<mio::net::UdpSocket>,
    /// Each slot's multicast socket while its plan names a group, and the
    /// group: read instead of the slot's own socket (by its handle,
    /// `rx/handles.rs`), registered with the slot's token while this
    /// thread writes.
    joined: Vec<Option<(SocketAddrV4, mio::net::UdpSocket)>>,
    /// Group sockets replaced, each with its swap's number: closed once
    /// the writer moved past it.
    retired: Vec<(u64, mio::net::UdpSocket)>,
    /// Each slot's plan as this thread reads it, and its keepalives.
    keeps: Vec<Keep>,
    /// [`RxShared::rejoin_groups`]'s count acted on last.
    rejoins_seen: u32,
    /// Slots with datagrams waiting, one bit each.
    ready: u32,
    /// The ring writer, while this thread is it.
    intake: Option<Intake>,
    /// A datagram read and dropped under the pump ([`Receiver::pass`]).
    buf: Box<[u8; BUFFER]>,
}

impl Receiver {
    /// The thread's state for `shared`: its poll, and, while it writes the
    /// rings (the engine's pump did not claim them first,
    /// [`RxShared::pump_intake`]), every flow socket registered with it and
    /// the writer.
    pub fn new(shared: Arc<RxShared>) -> io::Result<Self> {
        let poll = Poll::new()?;
        let intake = shared
            .thread_writes()
            .then(|| Intake::new(Arc::clone(&shared)));
        let mut sockets = Vec::with_capacity(MAX_FLOWS);
        for (slot, s) in shared.sockets.iter().enumerate() {
            let mut s = mio::net::UdpSocket::from_std(s.try_clone()?);
            if intake.is_some() {
                poll.registry()
                    .register(&mut s, Token(slot), Interest::READABLE)?;
            }
            sockets.push(s);
        }
        Ok(Receiver {
            poll,
            events: Events::with_capacity(MAX_FLOWS),
            sockets,
            joined: (0..MAX_FLOWS).map(|_| None).collect(),
            retired: Vec::with_capacity(2 * MAX_FLOWS),
            keeps: (0..MAX_FLOWS).map(|_| Keep::default()).collect(),
            rejoins_seen: shared.rejoins.load(Ordering::Acquire),
            ready: 0,
            intake,
            buf: Box::new([0; BUFFER]),
            shared,
        })
    }

    /// The writer's view of each slot (tests; none under the pump).
    #[cfg(test)]
    fn lanes(&self) -> &[lane::Lane] {
        self.intake.as_ref().map_or(&[], |i| &i.lanes)
    }

    /// Waits up to `timeout` for datagrams; the slots that have some are
    /// read at the next [`Receiver::pass`]. A failed wait is the poll's
    /// error (an interrupt is nothing to the thread; any other is counted,
    /// [`RxShared::poll_errors`], and the thread naps).
    pub fn wait(&mut self, timeout: Duration) -> io::Result<()> {
        self.poll.poll(&mut self.events, Some(timeout))?;
        for ev in &self.events {
            if let Some(bit) = u32::try_from(ev.token().0)
                .ok()
                .and_then(|t| 1u32.checked_shl(t))
            {
                self.ready |= bit;
            }
        }
        Ok(())
    }

    /// How long the next [`Receiver::wait`] may last at the thread's clock
    /// `mono`: none while datagrams wait, else until the next keepalive is
    /// due, at most [`WAKE_NS`].
    pub fn timeout(&self, mono: u64) -> Duration {
        if self.ready != 0 {
            return Duration::ZERO;
        }
        let due = self
            .keeps
            .iter()
            .filter_map(|k| k.next_keepalive)
            .fold(mono.saturating_add(WAKE_NS), u64::min);
        Duration::from_nanos(due.saturating_sub(mono))
    }

    /// One pass at PTP time `now_ns` (`None`: our clock is not ready) and
    /// the thread's clock `mono`: the plans that changed (their groups),
    /// every slot with datagrams waiting read while this thread writes the
    /// rings ([`Intake`]), the group sockets the writer moved past closed,
    /// the keepalives due sent. The wall clock is read once, for the
    /// packets' wake delays (dstack #234), only while the sockets stamp
    /// their receives and this thread reads them.
    pub fn pass(&mut self, now_ns: Option<u64>, mono: u64) {
        self.refresh();
        match &mut self.intake {
            Some(intake) => {
                let wall = self.shared.stamps().then(wall_now_ns).flatten();
                self.ready = intake.pass(
                    self.ready,
                    (now_ns, mono, wall),
                    &self.sockets,
                    &self.joined,
                );
            }
            None => self.empty_unplanned(),
        }
        self.release();
        self.keepalives(mono);
    }

    /// Under the pump (which reads only the slots that have a flow): each
    /// slot without one emptied of what came to it, counted `stray`, so a
    /// later flow in the slot never starts behind a backlog of the old one.
    fn empty_unplanned(&mut self) {
        let lanes = &self.shared.lanes;
        for ((keep, socket), stats) in self.keeps.iter().zip(&self.sockets).zip(lanes.iter()) {
            if keep.plan.src.is_none() {
                discard(stats, &mut self.buf[..], UNPLANNED_DRAIN, |buf| {
                    recv_stamped(socket, buf)
                });
            }
        }
    }

    /// Reads the plans that changed for this thread's part: a group to
    /// join or leave, a keepalive schedule started again; and a rejoin
    /// asked (dstack #282).
    fn refresh(&mut self) {
        let shared = &*self.shared;
        let mut regrouped = 0u64;
        for (slot, (keep, cell)) in self.keeps.iter_mut().zip(&shared.plans).enumerate() {
            if keep.seen != Some(cell.version())
                && let Some((version, w)) = cell.read()
            {
                let plan = RxPlan::from_words(w);
                if plan != keep.plan {
                    // Another group, or a new flow of a group whose join
                    // failed: (re)joined.
                    let unjoined = plan.group.is_some() && self.joined[slot].is_none();
                    if plan.group != keep.plan.group || unjoined {
                        regrouped |= 1 << slot;
                    }
                    keep.plan = plan;
                    keep.next_keepalive = None;
                }
                keep.seen = Some(version);
            }
        }
        // dstack #282: an interface change asked every slot's group joined
        // again (the membership went with the address).
        let rejoin = self.rejoins_asked();
        regrouped |= rejoin.unwrap_or(0);
        while regrouped != 0 {
            let slot = regrouped.trailing_zeros() as usize;
            regrouped &= regrouped - 1;
            self.regroup(slot);
        }
        if rejoin.is_some() {
            self.tell_missing();
        }
    }

    /// Slot `slot`'s plan names another group (or none), or a rejoin was
    /// asked (dstack #282): the old group's socket is closed (which leaves
    /// the group) once the writer moved past it, the new group's opened,
    /// handed to the writer and, while this thread writes, registered.
    /// Only on a plan change or a rejoin, never per packet.
    fn regroup(&mut self, slot: usize) {
        let want = self.keeps[slot].plan.group;
        let polled = self.intake.is_some();
        let old = self.joined[slot].take();
        if let Some((group, _)) = &old {
            info!(slot = slot + 1, %group, "RX multicast flow: group left");
        }
        if let Some(group) = want {
            let stats = &self.shared.lanes[slot];
            let joined = join(self.shared.ip, group).and_then(|s| {
                let mut s = mio::net::UdpSocket::from_std(s);
                if polled {
                    self.poll
                        .registry()
                        .register(&mut s, Token(slot), Interest::READABLE)?;
                }
                Ok(s)
            });
            match joined {
                Ok(s) => {
                    stats.joins.fetch_add(1, Ordering::Relaxed);
                    info!(slot = slot + 1, %group, "RX multicast flow: group joined");
                    self.joined[slot] = Some((group, s));
                }
                Err(e) => {
                    stats.join_errors.fetch_add(1, Ordering::Relaxed);
                    warn!(slot = slot + 1, %group, error = %e, "RX multicast flow: the group could not be joined");
                }
            }
        }
        let (handle, of) = self.joined[slot].as_ref().map_or((0, 0), |(group, s)| {
            (handle_of(s), source_word(Some(*group)))
        });
        let moved = self.shared.handles.set(slot, handle, of);
        if let Some((_, mut s)) = old {
            if polled {
                let _ = self.poll.registry().deregister(&mut s);
            }
            self.retired.push((moved, s));
        }
    }

    /// Closes the replaced group sockets the writer has moved past.
    fn release(&mut self) {
        let handles = &self.shared.handles;
        self.retired.retain(|(moved, _)| !handles.released(*moved));
    }

    /// Sends the keepalives due at the thread's clock `mono`: one per
    /// [`KEEPALIVE_NS`] per granted flow whose audio came within
    /// [`LIVE_NS`] (the writer's counts say when and from where), to that
    /// audio's source while it is the plan's transmitter.
    fn keepalives(&mut self, mono: u64) {
        for (slot, keep) in self.keeps.iter_mut().enumerate() {
            let stats = &self.shared.lanes[slot];
            let heard = stats.heard.load(Ordering::Acquire).checked_sub(1);
            let source = source_of(stats.source.load(Ordering::Relaxed))
                .filter(|s| keep.plan.src == Some(*s.ip()));
            let live = keep.plan.live && heard.is_some_and(|h| mono.saturating_sub(h) < LIVE_NS);
            let (true, Some(to)) = (live, source) else {
                keep.next_keepalive = None;
                continue;
            };
            let due = *keep
                .next_keepalive
                .get_or_insert(mono.saturating_add(KEEPALIVE_NS));
            if mono < due {
                continue;
            }
            let counter = match self.sockets[slot].send_to(&KEEPALIVE, SocketAddr::V4(to)) {
                Ok(_) => &stats.keepalives,
                Err(_) => &stats.keepalive_errors,
            };
            counter.fetch_add(1, Ordering::Relaxed);
            let next = due.saturating_add(KEEPALIVE_NS);
            keep.next_keepalive = Some(if next <= mono {
                mono.saturating_add(KEEPALIVE_NS)
            } else {
                next
            });
        }
    }
}

impl Drop for Receiver {
    /// While the engine's pump writes the rings it may still read a group
    /// socket by its handle: every group socket this thread holds then goes
    /// to the shared state and closes with it, after the pump's last read
    /// (dstack #326).
    fn drop(&mut self) {
        if self.intake.is_some() {
            return;
        }
        let mut parked = self
            .shared
            .parked
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        parked.extend(
            self.joined
                .iter_mut()
                .filter_map(Option::take)
                .map(|(_, s)| s),
        );
        parked.extend(self.retired.drain(..).map(|(_, s)| s));
    }
}

/// Starts the RX thread: it receives `shared`'s flows on `clock` until
/// [`RxShared::stop`], and records its scheduling in `scheduling` (the
/// device's, [`super::tx::TxShared::scheduling`]).
pub fn spawn(
    clock: Arc<dyn PtpClock>,
    shared: Arc<RxShared>,
    cpu: Option<usize>,
    scheduling: Arc<Scheduling>,
) -> io::Result<JoinHandle<()>> {
    let rx = Receiver::new(Arc::clone(&shared))?;
    std::thread::Builder::new()
        .name("dstack-rx".to_string())
        .spawn(move || run(clock.as_ref(), &shared, rx, cpu, &scheduling))
}

/// What a failed wait means for the RX thread: an interrupt (a signal) is
/// nothing; any other failure is counted ([`RxShared::poll_errors`]) and
/// the thread naps [`WAKE_NS`] (`true`), so a failing poll is never a spin.
fn failed_wait(shared: &RxShared, e: &io::Error) -> bool {
    if e.kind() == ErrorKind::Interrupted {
        return false;
    }
    shared.poll_errors.fetch_add(1, Ordering::Relaxed);
    true
}

/// The RX thread's loop (glue: every decision is [`Receiver`]'s and
/// [`failed_wait`]'s).
fn run(
    clock: &dyn PtpClock,
    shared: &RxShared,
    mut rx: Receiver,
    cpu: Option<usize>,
    scheduling: &Scheduling,
) {
    take_priority(MediaThread::Rx, RX_PRIORITY, scheduling);
    place("RX thread", cpu);
    info!(
        bytes = shared.recv_buffer(),
        asked = RECV_BUFFER_BYTES,
        // dstack #326: who writes the rings, the pump or this thread.
        pump_writes = shared.pump_writes(),
        // dstack #307: or the app's consumer (an RX bridge it pulls).
        app_writes = shared.app_writes(),
        "RX flow sockets' receive buffer"
    );
    let mut pin = super::repin::RePin::new(cpu);
    while !shared.stopping() {
        shared.serve_stall();
        if let Err(e) = rx.wait(rx.timeout(shared.now_ns()))
            && failed_wait(shared, &e)
        {
            std::thread::sleep(Duration::from_nanos(WAKE_NS));
        }
        let began = shared.now_ns();
        pin.keep(began);
        rx.pass(clock.now_ns(), began);
        // Linux sleeps precisely; Windows' sleep is coarse (ms), so it
        // keeps a wake per datagram.
        #[cfg(target_os = "linux")]
        std::thread::sleep(Duration::from_nanos(rest_ns(began, shared.now_ns())));
    }
}

#[cfg(test)]
mod tests;
