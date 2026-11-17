// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The TX thread (moved out of `tx.rs`, dstack #360): its state and pass
//! ([`Transmitter`]), its start ([`spawn`]) and loops (`run_passes`, the
//! bounded escape through the relay thread, `run_relay`).

use std::io;
use std::net::{SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;
use std::time::Instant;

use tracing::{info, warn};

use super::{
    DSCP_EF, EVENTS_EVERY_NS, FPP_MAX, KEEPALIVE_DRAIN, MAX_CHANNELS, MAX_FLOWS, MediaThread,
    PassClock, Plan, SEND_BUFFER_BYTES, Schedule, Scheduling, TX_PRIORITY, TxShared, account, host,
    place, relay, send_clock, send_timed, shift, source_from, split, take_priority,
};
use crate::clock::{self, PtpClock};
use crate::media::bypass::{self, Direct, HEADROOM};
use crate::media::packet;
use crate::media::ring::Ring;
use crate::media::source::Source;
use crate::wire::MAX_DATAGRAM;

/// One slot as the TX thread sees it.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Lane {
    /// The plan cell's version read last.
    seen: Option<u32>,
    plan: Plan,
    /// `None` until the clock is ready, and after the plan restarted.
    pub(super) schedule: Option<Schedule>,
}

/// The TX thread's state: everything it needs, allocated before its loop.
#[derive(Debug)]
pub struct Transmitter {
    pub(super) shared: Arc<TxShared>,
    pub(super) lanes: Vec<Lane>,
    sources: Vec<Source>,
    sources_seen: Vec<Option<u32>>,
    /// One packet's samples, slot by slot.
    frames: Box<[[i32; FPP_MAX as usize]; MAX_CHANNELS]>,
    /// One packet after [`HEADROOM`] bytes for a direct send's headers.
    packet: Box<[u8; HEADROOM + MAX_DATAGRAM]>,
    /// The IPv4 ID of the next direct packet.
    ip_id: u16,
    /// The host sender's frames of a pass (dstack #1); `None` without a
    /// host sender.
    batch: Option<host::Batch>,
    /// The clock's epoch at the last pass.
    pub(super) epoch: Option<u32>,
    /// The engine shift at that epoch ([`shift::EngineLine`]).
    pub(super) engine_shift: i64,
    /// When the next read of the plans, sources and keepalives is due.
    next_events: Option<u64>,
    pub(super) rate: u32,
    pub(super) max_lag: u64,
    /// The device's `tx_margin_ns`.
    margin_ns: u64,
    /// The device's advertised latency, ns: a packet older than this past
    /// its stamp is late at a receiver.
    latency_ns: u64,
    /// Whether the relay thread holds it, covering a bounded escape.
    relayed: bool,
    /// When the last bounded escape began, PTP ns.
    pub(super) escaped_ns: Option<u64>,
    /// What the TX thread measured of the time before this pass (dstack
    /// #359): the restart line's cause.
    pub(super) before: split::Before,
    /// The app's TX pull (dstack #363, [`super::TxFill`]): filled each
    /// pass before the sends; `None` without one.
    pub(super) fill: Option<super::TxFill>,
}

impl Transmitter {
    /// The thread's state for `shared` (every allocation it will need).
    pub fn new(shared: Arc<TxShared>) -> Self {
        let rate = shared.cfg.sample_rate;
        let channels = shared.sources.len();
        Transmitter {
            lanes: vec![Lane::default(); MAX_FLOWS],
            sources: vec![Source::Silence; channels],
            sources_seen: vec![None; channels],
            frames: Box::new([[0; FPP_MAX as usize]; MAX_CHANNELS]),
            packet: Box::new([0; HEADROOM + MAX_DATAGRAM]),
            ip_id: 0,
            batch: shared.host_for_tx().map(host::Batch::new),
            // After the host's: the claims are closed now.
            fill: shared.fill_for_tx(),
            epoch: None,
            engine_shift: 0,
            next_events: None,
            rate,
            max_lag: clock::latency_samples(shared.advertised_latency_ns(), rate),
            margin_ns: shared.cfg.tx_margin_ns,
            latency_ns: shared.advertised_latency_ns(),
            relayed: false,
            escaped_ns: None,
            before: split::Before::default(),
            shared,
        }
    }

    /// One pass at PTP time `now_ns` (`None`: the clock is not ready) and
    /// the clock's `anchor` (its epoch and latest known re-anchor; a bare
    /// epoch knows no step): the plans, sources and keepalives when due
    /// (every pass while the clock is not ready, and at once when the clock
    /// went back), every flow shifted by a re-anchor's known step or else
    /// realigned ([`shift`]), then every packet due sent. Nothing is sent
    /// before the clock is ready.
    pub fn step(&mut self, now_ns: Option<u64>, anchor: impl Into<clock::Anchor>) {
        self.shared.set_clock_ready(now_ns.is_some());
        let anchor = anchor.into();
        // Taken with the clock read (the caller's), so a slow events pass
        // counts in the lateness of the packets after it.
        let pass = Instant::now();
        let events_due = match (now_ns, self.next_events) {
            // Past the next read, or the clock went back before the last.
            (Some(now), Some(at)) => now >= at || at - now > EVENTS_EVERY_NS,
            _ => true,
        };
        if events_due {
            self.events();
            self.next_events = now_ns.map(|now| now.saturating_add(EVENTS_EVERY_NS));
        }
        let Some(now_ns) = now_ns else {
            if let Some(fill) = self.fill.as_mut() {
                fill.idle();
            }
            return;
        };
        let now = send_clock(now_ns, self.rate, self.margin_ns);
        if self.epoch != Some(anchor.epoch) {
            self.re_anchored(now, clock::to_samples(now_ns, self.rate), anchor);
        }
        self.epoch = Some(anchor.epoch);
        self.fill_due(now_ns);
        self.send_due(now, now_ns, pass);
    }

    /// Before the TX thread's wait (after a pass): whether the wait makes a
    /// bounded escape (dstack #1, `spin_escape_ms`), whose block the relay
    /// thread covers ([`relay`], dstack #141). `ask` says whether one is due
    /// (`clock::ask_escape`). A last pass at a fresh clock `read` (PTP ns
    /// and the clock's epoch) sends what is due by then, never anything
    /// ahead of its time; a clock that is not ready or re-anchored since
    /// the pass puts the escape off (`defer`, `clock::defer_escape`;
    /// `false` when it is overdue and escapes anyway), so a re-anchor is
    /// always the TX thread's own pass.
    pub fn before_wait(
        &mut self,
        read: impl FnOnce() -> (Option<u64>, u32),
        ask: impl FnOnce() -> bool,
        defer: impl FnOnce() -> bool,
    ) -> bool {
        if !ask() {
            return false;
        }
        let (now_ns, epoch) = read();
        let escapes = match now_ns {
            Some(now_ns) if self.epoch == Some(epoch) => {
                let now = send_clock(now_ns, self.rate, self.margin_ns);
                self.fill_due(now_ns);
                self.send_due(now, now_ns, Instant::now());
                true
            }
            _ => !defer(),
        };
        if escapes && now_ns.is_some() {
            self.escaped_ns = now_ns;
        }
        if escapes {
            // The relay thread's passes come next: nothing measured.
            self.before = split::Before::default();
        }
        escapes
    }

    /// When the next pass is due, PTP ns: the next packet of any flow or
    /// the next read of the plans; 0 (at once: a clock that is not ready
    /// naps) when the clock is not ready.
    pub fn wake_ns(&self, now_ns: Option<u64>) -> u64 {
        if now_ns.is_none() {
            return 0;
        }
        // A lane has a schedule only while its plan sends; the TX pull's
        // next block is due at the pump's own time (dstack #363).
        self.lanes
            .iter()
            .filter_map(|l| l.schedule.map(|s| s.wake_ns(self.rate, self.margin_ns)))
            .chain(self.fill.as_ref().and_then(|f| f.wake_ns(self.rate)))
            .fold(self.next_events.unwrap_or(0), u64::min)
    }

    /// The TX pull's blocks due at PTP `now_ns` into the engine rings
    /// (dstack #363), before the pass's sends.
    fn fill_due(&mut self, now_ns: u64) {
        if let Some(fill) = self.fill.as_mut() {
            fill.fill_due(
                clock::to_samples(now_ns, self.rate),
                self.max_lag,
                &self.shared.engine,
                self.engine_shift,
            );
        }
    }

    /// The plans and sources that changed, and the keepalives of every
    /// flow that sends.
    fn events(&mut self) {
        let shared = &*self.shared;
        // The advertised latency, which Dante Controller may have set: the
        // lag bound and the over-latency count follow it.
        let advertised = shared.advertised_latency_ns();
        if advertised != self.latency_ns {
            self.latency_ns = advertised;
            self.max_lag = clock::latency_samples(advertised, self.rate);
        }
        for (slot, lane) in self.lanes.iter_mut().enumerate() {
            let cell = &shared.plans[slot];
            if lane.seen != Some(cell.version())
                && let Some((version, w)) = cell.read()
            {
                let plan = Plan::from_words(w);
                if plan.restarts(&lane.plan) {
                    lane.schedule = None;
                }
                lane.plan = plan;
                lane.seen = Some(version);
            }
            let Some(dest) = lane.plan.dest else {
                continue;
            };
            let mut heard = 0;
            for _ in 0..KEEPALIVE_DRAIN {
                match shared.sockets[slot].recv_from(&mut self.packet[..]) {
                    Ok((_, SocketAddr::V4(from))) if from == dest => heard += 1,
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            shared.lanes[slot]
                .keepalives
                .fetch_add(heard, Ordering::Relaxed);
        }
        for (tx, (source, seen)) in self
            .sources
            .iter_mut()
            .zip(self.sources_seen.iter_mut())
            .enumerate()
        {
            let cell = &shared.sources[tx];
            if *seen != Some(cell.version())
                && let Some((version, w)) = cell.read()
            {
                *source = source_from(w);
                *seen = Some(version);
            }
        }
    }

    /// Every packet due at media time `now` (PTP `now_ns`, read at `pass`),
    /// each flow in turn (the hot path: no allocation, no lock, no log; the
    /// send is its one system call). Never a packet before its due time
    /// (dstack #141). With a host sender, the packets to resolved
    /// neighbours are gathered as frames and sent in one call at the end of
    /// the pass ([`host::Batch`]); the rest go out at once.
    fn send_due(&mut self, now: u64, now_ns: u64, pass: Instant) {
        let Transmitter {
            shared,
            lanes,
            sources,
            frames,
            packet,
            rate,
            max_lag,
            margin_ns,
            latency_ns,
            ip_id,
            batch,
            engine_shift,
            relayed,
            escaped_ns,
            before,
            ..
        } = self;
        let clock = PassClock {
            now_ns,
            pass,
            rate: *rate,
            margin_ns: *margin_ns,
            latency_ns: *latency_ns,
            relayed: *relayed,
            escaped_ns: *escaped_ns,
        };
        let rings: &[Ring] = &shared.rings;
        for (slot, lane) in lanes.iter_mut().enumerate() {
            let Some(dest) = lane.plan.dest else {
                continue;
            };
            let plan = lane.plan;
            let stats = &shared.lanes[slot];
            let receiver_ns = shared.receiver_latency_ns(slot, dest);
            let socket = &shared.sockets[slot];
            let direct = shared.direct_mac(slot);
            // The host sender, when the slot's receiver is resolved.
            let host_mac = batch.as_ref().and_then(|_| shared.host_mac(slot));
            let src = SocketAddrV4::new(shared.ip, shared.ports[slot]);
            let schedule = lane
                .schedule
                .get_or_insert_with(|| Schedule::bootstrap(now, plan.fpp, *max_lag));
            let fpp = usize::from(plan.fpp);
            let nch = usize::from(plan.nch);
            let mut holes = 0;
            while let Some(stamp) = schedule.due_after(now, before) {
                for (frame, &id) in frames.iter_mut().zip(&plan.channels).take(nch) {
                    let out = &mut frame[..fpp];
                    match usize::from(id).checked_sub(1).and_then(|i| {
                        let own = shared.engine.get(i).and_then(Option::as_ref);
                        Some((sources.get_mut(i)?, shared.peaks.get(i)?, own))
                    }) {
                        Some((source, peak, own)) => {
                            let at = shift::source_position(source, stamp, *engine_shift);
                            holes += source.fill(at, *rate, out, rings, own).holes;
                            let loudest = out.iter().fold(0, |m: u32, s| m.max(s.unsigned_abs()));
                            peak.fetch_max(loudest, Ordering::Relaxed);
                        }
                        None => out.fill(0),
                    }
                }
                let (secs, sub) = clock::stamp(stamp, *rate);
                let samples = |f: usize, s: usize| frames[s][f];
                if let (Some(batch), Some(mac)) = (batch.as_mut(), host_mac) {
                    let queued = host::Queued {
                        slot,
                        stamp,
                        fpp: plan.fpp,
                        src,
                        dest,
                        mac,
                    };
                    // The plan is sendable (TxTable::plan), so the packet
                    // is whole and fits the frame.
                    batch.push(&clock, shared, queued, ip_id, |buf| {
                        packet::write(buf, secs, sub, nch, usize::from(plan.bps), fpp, samples)
                    });
                    continue;
                }
                // The plan is sendable (TxTable::plan), so the packet is
                // whole and fits the buffer.
                let n = packet::write(
                    &mut packet[HEADROOM..],
                    secs,
                    sub,
                    nch,
                    usize::from(plan.bps),
                    fpp,
                    samples,
                );
                let sent = send_timed(&clock, stamp, plan.fpp, || {
                    send_one(direct, socket, &mut packet[..], src, dest, ip_id, n)
                });
                account(stats, &clock, stamp, plan.fpp, &sent, n, receiver_ns);
            }
            stats.holes.fetch_add(holes as u64, Ordering::Relaxed);
            stats
                .rebootstraps
                .fetch_add(schedule.take_rebootstraps(), Ordering::Relaxed);
        }
        if let Some(batch) = batch.as_mut() {
            batch.flush(&clock, shared);
        }
    }
}

/// Sends the media packet of `n` bytes at `buf[HEADROOM..]` from `src` to
/// `dest`: past the qdisc when `direct` holds the socket and the MAC (the
/// headers written in front, the IPv4 ID taken from `ip_id`), else through
/// the slot's UDP `socket`. `Ok(true)`: it went direct. Thin glue: the
/// direct socket needs `CAP_NET_RAW`, which no test has; the headers are
/// [`bypass::write_headers`]'s, tested.
pub(super) fn send_one(
    direct: Option<(&Direct, [u8; 6])>,
    socket: &UdpSocket,
    buf: &mut [u8],
    src: SocketAddrV4,
    dest: SocketAddrV4,
    ip_id: &mut u16,
    n: usize,
) -> io::Result<bool> {
    match direct {
        Some((d, mac)) => {
            let len = bypass::write_headers(buf, src, dest, DSCP_EF << 2, *ip_id, n);
            *ip_id = ip_id.wrapping_add(1);
            d.send(&buf[..len], mac).map(|()| true)
        }
        // A datagram socket sends the datagram whole or not at all (the
        // packet fits one: `wire::MAX_DATAGRAM`), so the byte count says
        // nothing more than `Ok`.
        None => socket
            .send_to(&buf[HEADROOM..HEADROOM + n], dest)
            .map(|_| false),
    }
}

/// Starts the TX thread: it sends `shared`'s flows on `clock` until
/// [`TxShared::stop`].
pub fn spawn(clock: Arc<dyn PtpClock>, shared: Arc<TxShared>) -> io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("dstack-tx".to_string())
        .spawn(move || run(clock.as_ref(), shared))
}

/// How long the TX thread waits for its relay thread to take over before
/// it puts its escape off (dstack #141): its packets due meanwhile wait
/// that long.
pub const RELAY_TAKE_MAX: std::time::Duration = std::time::Duration::from_micros(200);

/// The TX thread's loop (glue: every decision is [`Transmitter`]'s). With
/// the bounded escape on, a relay thread covers the TX thread's blocks
/// ([`relay`], dstack #141).
fn run(clock: &dyn PtpClock, shared: Arc<TxShared>) {
    run_with(clock, shared, crate::clock::escape_on());
}

/// [`run`] with the bounded escape on (`escape`, `clock::escape_on`) or
/// off: only with it on does a relay thread start.
pub(super) fn run_with(clock: &dyn PtpClock, shared: Arc<TxShared>, escape: bool) {
    take_priority(MediaThread::Tx, TX_PRIORITY, &shared.scheduling);
    place("TX thread", shared.cfg.io_cpu);
    info!(
        bytes = shared.send_buffer(),
        asked = SEND_BUFFER_BYTES,
        "TX flow sockets' send buffer"
    );
    info!("{}", host::path_line(shared.tx_path(), &shared.tx_nic()));
    // Held while the thread runs: deep idle states delay every packet
    // after its send (`super::qos`).
    #[cfg(target_os = "linux")]
    let _wake = crate::clock::realtime_for(crate::clock::profile())
        .then(|| crate::media::qos::hold(std::path::Path::new(crate::media::qos::CPU_DMA_LATENCY)))
        .flatten();
    let tx = Transmitter::new(Arc::clone(&shared));
    if !escape {
        return run_passes(clock, &shared, tx, None);
    }
    let relay = relay::Relay::new();
    std::thread::scope(|scope| {
        let started = std::thread::Builder::new()
            .name("dstack-tx-relay".to_string())
            .spawn_scoped(scope, || run_relay(clock, &relay, &shared.scheduling));
        match started {
            Ok(_) => {
                info!(
                    "TX relay thread started: it sends TX's packets while the TX thread blocks its bounded escape"
                );
                run_passes(clock, &shared, tx, Some(&relay));
            }
            Err(e) => {
                warn!(error = %e, "the TX relay thread did not start: the TX thread makes no bounded escape");
                run_passes(clock, &shared, tx, None);
            }
        }
        relay.stop();
    });
}

/// The TX thread's passes until [`TxShared::stop`]; with a `relay`, its
/// bounded escapes hand the passes to the relay thread (dstack #141),
/// without one its waits never escape.
fn run_passes(
    clock: &dyn PtpClock,
    shared: &TxShared,
    mut tx: Transmitter,
    relay: Option<&relay::Relay<Transmitter>>,
) {
    #[cfg(any(target_os = "linux", windows))]
    crate::clock::allow_escape(relay.is_some());
    let mut pin = crate::media::repin::RePin::new(shared.cfg.io_cpu);
    let mut misses = 0u64;
    // The base of the passes' wall times ([`split`], dstack #359).
    let base = Instant::now();
    while !shared.stopping() {
        pin.keep(shared.now_ns());
        let (anchor, now) = split::timed_pass(clock, &mut tx, base);
        // The bounded escape (dstack #1, `spin_escape_ms`, Windows): the
        // relay thread sends while this thread blocks one timer tick.
        #[cfg(any(target_os = "linux", windows))]
        if let Some(relay) = relay
            && tx.before_wait(
                || (clock.now_ns(), clock.epoch()),
                crate::clock::ask_escape,
                crate::clock::defer_escape,
            )
        {
            tx = escape_through(
                relay,
                tx,
                &mut misses,
                RELAY_TAKE_MAX,
                || {
                    crate::clock::block_escape();
                },
                crate::clock::defer_escape,
            );
            continue;
        }
        // How late the sleep returns (dstack #359).
        let target = tx.wake_ns(now);
        split::timed_sleep(clock, &mut tx, target, anchor.epoch);
    }
}

/// The TX thread's bounded escape with `relay` covering it: hands `tx`
/// over, `block`s (`clock::block_escape`), takes it back. A relay that
/// does not take over within `take_max` ([`RELAY_TAKE_MAX`]) puts the
/// escape off (`defer`, `clock::defer_escape`; counted in `misses`, warned
/// at the first and every 100th); one that is overdue blocks uncovered.
pub(super) fn escape_through(
    relay: &relay::Relay<Transmitter>,
    tx: Transmitter,
    misses: &mut u64,
    take_max: std::time::Duration,
    block: impl FnOnce(),
    defer: impl FnOnce() -> bool,
) -> Transmitter {
    let start = Instant::now();
    match relay.hand_over(tx, || !take_max.saturating_sub(start.elapsed()).is_zero()) {
        Ok(()) => {
            block();
            relay.take_back()
        }
        Err(tx) => {
            *misses += 1;
            let put_off = defer();
            if *misses % 100 == 1 {
                warn!(
                    misses = *misses,
                    put_off,
                    "the TX relay thread did not take over in time: the escape is put off (uncovered when overdue)"
                );
            }
            if !put_off {
                block();
            }
            tx
        }
    }
}

/// The relay thread's loop: while it covers, the TX thread's passes at
/// their times, spinning without an escape (dstack #141).
pub(super) fn run_relay(
    clock: &dyn PtpClock,
    relay: &relay::Relay<Transmitter>,
    scheduling: &Scheduling,
) {
    take_priority(MediaThread::TxRelay, TX_PRIORITY, scheduling);
    #[cfg(any(target_os = "linux", windows))]
    crate::clock::relay_spin();
    // The base of the passes' wall times ([`split`], dstack #359).
    let base = Instant::now();
    relay.serve(|tx, back| {
        tx.relayed = true;
        loop {
            // Timed and its sleeps read as the TX thread's (dstack #359).
            let (anchor, now) = split::timed_pass(clock, tx, base);
            if back() {
                tx.relayed = false;
                return;
            }
            // At most an escape's length, so a TX thread back from its block
            // never waits long for the handback.
            let wake = tx.wake_ns(now);
            let target = now.map_or(wake, |n| wake.min(n + crate::clock::ESCAPE_MAX_NS));
            split::timed_sleep(clock, tx, target, anchor.epoch);
        }
    });
}
