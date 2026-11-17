// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.
// Protocol facts (byte layouts, opcodes, behaviour) checked against
// inferno (https://github.com/teodly/inferno) as a reference; the code is
// our own.

//! The info server's socket side: the loops that receive the requests on
//! [`REQUEST_PORT`], send the replies, notifications and heartbeats
//! ([`serve_info`], [`serve_shared`] with [`share`]), and the running
//! device's heartbeat records ([`Sources`]). Every packet's bytes come from
//! the pure builder in [`super`] ([`InfoBuilder`]).

use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::time::MissedTickBehavior;
use tracing::{debug, info, warn};

use super::limit::{self, Limit};
use super::sendwatch::{self, Recover, SendWatch};
use super::unanswered::Unanswered;
use super::{
    Beat, Changes, Follower, HEARTBEAT_EVERY, HEARTBEAT_GROUP, HEARTBEAT_PORT, INFO_GROUP,
    INFO_PORT, InfoBuilder, RECV_BUFFER, REQUEST_PORT, Records, never_answered, peak_byte, share,
};
use crate::clock::{ClockStats, PtpClock};
use crate::ctl::{Requests, info_op};
use crate::media::rx::{MAX_FLOWS, RxShared};
use crate::media::tx::TxShared;
use crate::subscribe::Manager;

/// The running device's records ([`crate::daemon::media::Media::beat`]):
/// the media clock, dsync's frequency adjustment as the clock's poller
/// keeps it, the TX side (its channels' peaks), the RX side (its rings'
/// peaks, its slots' lateness) and the manager (which slots hold a granted
/// flow). Each beat takes the peaks and every slot's lateness (the largest
/// in samples and the packets' least in ns), so the next one reports its
/// own second (the control socket's status takes none of them).
/// Before the media clock is ready the heartbeat is the header alone.
#[derive(Clone)]
pub struct Sources {
    pub clock: Arc<dyn PtpClock>,
    pub freq: Arc<AtomicI32>,
    pub tx: Arc<TxShared>,
    pub rx: Arc<RxShared>,
    pub manager: Arc<Mutex<Manager>>,
    /// Where each beat leaves its channels' levels for the meter frames
    /// ([`crate::cmc`]): the peaks are taken once, here.
    pub levels: Arc<Levels>,
    /// The poller's counts and the grandmaster dsync named.
    pub stats: Arc<ClockStats>,
}

/// The channels' levels as meter bytes ([`crate::meter::level_of_peak`]),
/// the last beat's: what a meter frame sends.
#[derive(Debug, Default)]
pub struct Levels(Mutex<(Vec<u8>, Vec<u8>)>);

impl Levels {
    /// Keeps the TX and RX peak bytes of a beat as meter bytes.
    pub fn set(&self, tx_peaks: &[u8], rx_peaks: &[u8]) {
        let meter = |p: &[u8]| p.iter().map(|&b| crate::meter::level_of_peak(b)).collect();
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = (meter(tx_peaks), meter(rx_peaks));
    }

    /// The TX and RX levels; empty before the first beat with records.
    pub fn get(&self) -> (Vec<u8>, Vec<u8>) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Beat for Sources {
    fn records(&self) -> Option<Records> {
        let records = self.take_records()?;
        self.levels.set(&records.tx_peaks, &records.rx_peaks);
        Some(records)
    }

    fn clock(&self) -> Option<Follower> {
        follower_of(
            self.clock.now_ns().is_some(),
            self.stats.grandmaster(),
            self.stats.master(),
            self.freq.load(Ordering::Relaxed),
        )
    }
}

/// What the clock stats state: a follower of `master` (the grandmaster
/// when dsync names none) under `grandmaster` at `freq_ppb`, only while
/// the media clock is `ready` and a grandmaster is known.
pub fn follower_of(
    ready: bool,
    grandmaster: Option<[u8; 6]>,
    master: Option<[u8; 6]>,
    freq_ppb: i32,
) -> Option<Follower> {
    let grandmaster = grandmaster.filter(|_| ready)?;
    Some(Follower {
        grandmaster,
        master: master.unwrap_or(grandmaster),
        freq_ppb,
    })
}

impl Sources {
    /// The beat's records, the peaks and lateness taken.
    fn take_records(&self) -> Option<Records> {
        self.clock.now_ns()?;
        let granted: [bool; MAX_FLOWS] = {
            let m = self.manager.lock().unwrap_or_else(PoisonError::into_inner);
            std::array::from_fn(|slot| m.flow(slot).is_some_and(|f| f.handle.is_some()))
        };
        Some(Records {
            freq_ppb: self.freq.load(Ordering::Relaxed),
            tx_peaks: (0..self.tx.cfg.tx.len())
                .map(|tx| peak_byte(self.tx.take_peak(tx)))
                .collect(),
            rx_peaks: self
                .rx
                .rings()
                .iter()
                .map(|ring| peak_byte(ring.take_peak()))
                .collect(),
            rate: self.tx.cfg.sample_rate,
            // Every slot's second is taken, granted or not, so the status's
            // lateness and the packets' judge never show an ended flow's
            // last second; only a granted flow's goes into the record.
            lateness: std::array::from_fn(|slot| {
                let taken = self.rx.take_max_lateness(slot);
                if granted[slot] {
                    u32::try_from(taken.min(i32::MAX as u64)).unwrap_or(0)
                } else {
                    0
                }
            }),
            late: std::array::from_fn(|slot| {
                let [.., a, b, c, d] = self.rx.lane(slot).late.to_be_bytes();
                u32::from_be_bytes([a, b, c, d])
            }),
        })
    }
}

/// Where the device's group traffic goes (the info packets, the heartbeat
/// and the direct mDNS queries), and how often the heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Multicast {
    /// Where the replies and the start's board and product info go.
    pub info: SocketAddrV4,
    /// Where the heartbeat goes.
    pub heartbeat: SocketAddrV4,
    /// The heartbeat's period; never zero.
    pub every: Duration,
    /// Where the direct mDNS instance queries go
    /// ([`crate::subscribe::instance`]): mDNS's group, or a test's
    /// responder.
    pub mdns: SocketAddrV4,
}

impl Multicast {
    /// Dante's groups and inferno's period.
    pub const DANTE: Multicast = Multicast {
        info: SocketAddrV4::new(INFO_GROUP, INFO_PORT),
        heartbeat: SocketAddrV4::new(HEARTBEAT_GROUP, HEARTBEAT_PORT),
        every: HEARTBEAT_EVERY,
        mdns: crate::subscribe::instance::MDNS,
    };
}

/// The info server on `sock` (bound to the device's address and
/// [`REQUEST_PORT`]): sends the board and the product info, then answers
/// every request to `to.info` within the bound ([`limit`]: at most
/// [`limit::PER_SPAN`] replies of a kind a second, the drops logged as one
/// warning a minute, looked at every [`limit::CHECK`]), sends the
/// channel-change notification to `to.info` when `changes` holds one
/// (within the bound; one held back goes at the next [`limit::CHECK`]) and
/// beats to `to.heartbeat` every `to.every` with `beat`'s records, the
/// first beat at once. The socket's address picks the interface the
/// multicasts leave by (Linux routes a multicast from a bound source
/// address out of that address's interface). Every datagram received is
/// counted in `requests` (the control socket's status). Ends with an error
/// receiving, or when a reopen cannot bind the address again; a packet that
/// cannot be sent is counted and the server goes on: failures are logged at
/// most once a minute and, in a row, recovered from ([`sendwatch`]: the
/// multicast interface named again, then the socket reopened on its
/// address; dstack #340).
pub async fn serve_info(
    sock: UdpSocket,
    mc: InfoBuilder,
    to: Multicast,
    beat: Arc<dyn Beat>,
    changes: Arc<Changes>,
    requests: Arc<Requests>,
) -> io::Result<()> {
    serve_info_with(sock, None, mc, to, beat, changes, requests).await
}

/// [`serve_info`] as the address's manager ([`share`]): with `share` (the
/// loopback socket on [`share::share_port`]), every member's registration
/// is kept, every request received is relayed to each live member, and
/// each live member's packets are sent from `sock`. A reopen binds `sock`'s
/// address again; when that fails, the server ends with the error.
pub async fn serve_info_with(
    sock: UdpSocket,
    share: Option<UdpSocket>,
    mc: InfoBuilder,
    to: Multicast,
    beat: Arc<dyn Beat>,
    changes: Arc<Changes>,
    requests: Arc<Requests>,
) -> io::Result<()> {
    let ctx = Ctx {
        to,
        beat,
        changes,
        requests,
    };
    serve_info_by(sock, share, mc, &ctx, &send_multicast).await
}

/// [`serve_info_with`] with its multicast sends through `send` (a test's
/// failing send, dstack #340).
pub(crate) async fn serve_info_by<S>(
    mut sock: UdpSocket,
    share: Option<UdpSocket>,
    mut mc: InfoBuilder,
    ctx: &Ctx,
    send: &S,
) -> io::Result<()>
where
    S: AsyncFn(&UdpSocket, &[u8], SocketAddrV4) -> io::Result<usize>,
{
    let mut manager = Served::new(ctx.to.every, SendWatch::default());
    manager.start(&sock, &mut mc, ctx, send).await;
    loop {
        manager
            .serve(&sock, share.as_ref(), &mut mc, ctx, send)
            .await?;
        let at = sock.local_addr()?;
        drop(sock);
        sock = UdpSocket::bind(at).await?;
        crate::winsock::no_connreset(&sock)?;
        info!("the info socket reopened after failed sends");
    }
}

/// What a manager's loop serves: the groups, the beat's records, the
/// channel changes and the request counts.
pub(crate) struct Ctx {
    pub to: Multicast,
    pub beat: Arc<dyn Beat>,
    pub changes: Arc<Changes>,
    pub requests: Arc<Requests>,
}

/// The real multicast send.
async fn send_multicast(sock: &UdpSocket, packet: &[u8], to: SocketAddrV4) -> io::Result<usize> {
    sock.send_to(packet, to).await
}

/// The ns since `base`, saturated: the members' lease clock (since the
/// manager's start).
pub(crate) fn elapsed_ns(base: Instant) -> u64 {
    u64::try_from(base.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// A manager's state, kept across a reopen of its socket: the members, the
/// bound, the unanswered requests, the send watch and the timers.
struct Served {
    base: Instant,
    members: share::Members,
    bound: Limit,
    unanswered: Unanswered,
    watch: SendWatch,
    every: tokio::time::Interval,
    check: tokio::time::Interval,
}

impl Served {
    fn new(period: Duration, watch: SendWatch) -> Self {
        let mut every = tokio::time::interval(period);
        every.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut check = tokio::time::interval(limit::CHECK);
        check.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Served {
            base: Instant::now(),
            members: share::Members::default(),
            bound: Limit::default(),
            unanswered: Unanswered::new(),
            watch,
            every,
            check,
        }
    }

    /// The board and the product info. They never ask a reopen: a new
    /// watch's run reaches 2 at most here, and one carried over from a
    /// reopen that fell back to a member was reset by its reopen (`run` 0).
    async fn start<S>(&mut self, sock: &UdpSocket, mc: &mut InfoBuilder, ctx: &Ctx, send: &S)
    where
        S: AsyncFn(&UdpSocket, &[u8], SocketAddrV4) -> io::Result<usize>,
    {
        let _ = self
            .out(sock, &mc.board_info(), ctx.to.info, ctx, send)
            .await;
        let _ = self
            .out(sock, &mc.product_info(), ctx.to.info, ctx, send)
            .await;
    }

    /// Serves on `sock` until a reopen is due (`Ok`) or a receive fails.
    async fn serve<S>(
        &mut self,
        sock: &UdpSocket,
        share: Option<&UdpSocket>,
        mc: &mut InfoBuilder,
        ctx: &Ctx,
        send: &S,
    ) -> io::Result<()>
    where
        S: AsyncFn(&UdpSocket, &[u8], SocketAddrV4) -> io::Result<usize>,
    {
        let to = ctx.to;
        let mut sbuf = vec![0u8; RECV_BUFFER];
        let mut buf = vec![0u8; RECV_BUFFER];
        loop {
            let out: Option<(Vec<u8>, SocketAddrV4)> = tokio::select! {
                got = sock.recv_from(&mut buf) => {
                    let (n, peer) = got?;
                    ctx.requests.received(REQUEST_PORT, peer.ip(), info_op(&buf[..n]));
                    mc.set_clock(ctx.beat.clock());
                    let reply = mc.answer_within(&mut self.bound, Instant::now(), &buf[..n]);
                    if reply.is_none() && never_answered(&buf[..n]) {
                        self.unanswered.record(Instant::now(), peer.ip(), &buf[..n]);
                    }
                    if let Some(share) = share {
                        let relay = share::Msg::Request(buf[..n].to_vec()).encode();
                        for member in self.members.live(elapsed_ns(self.base)) {
                            if let Err(e) = share.send_to(&relay, member).await {
                                debug!(error = %e, "an info request not relayed to a member");
                            }
                        }
                    }
                    reply.map(|p| (p, to.info))
                }
                got = recv_share(share, &mut sbuf) => {
                    let (n, from) = got?;
                    let now_ns = elapsed_ns(self.base);
                    match share::Msg::decode(&sbuf[..n]) {
                        Some(share::Msg::Register(id)) => {
                            if self.members.register(from, id, now_ns) {
                                debug!("an info member's lease renewed");
                            }
                            None
                        }
                        Some(share::Msg::Send(dest, packet)) if self.members.is_live(&from, now_ns) => {
                            let at = match dest {
                                share::Dest::Info => to.info,
                                share::Dest::Heartbeat => to.heartbeat,
                            };
                            Some((packet, at))
                        }
                        _ => {
                            debug!("a loopback datagram that is no member's: dropped");
                            None
                        }
                    }
                }
                _ = self.every.tick() => {
                    let records = ctx.beat.records();
                    Some((mc.heartbeat(records.as_ref()), to.heartbeat))
                }
                () = ctx.changes.added() => {
                    mc.notify_within(&mut self.bound, Instant::now(), &ctx.changes).map(|p| (p, to.info))
                }
                _ = self.check.tick() => {
                    let now = Instant::now();
                    self.unanswered.flush(now);
                    if let Some(report) = self.bound.flush(now) {
                        warn!(
                            dropped = report.total(),
                            counts = %report,
                            "info replies over the bound dropped in a minute (at most 10 a second of each kind)"
                        );
                    }
                    if let Some(r) = self.watch.flush(now) {
                        report_failures(&r);
                    }
                    mc.notify_within(&mut self.bound, now, &ctx.changes).map(|p| (p, to.info))
                }
            };
            if let Some((packet, at)) = out
                && self.out(sock, &packet, at, ctx, send).await
            {
                return Ok(());
            }
        }
    }

    /// Sends `packet` to `to` through `send` and watches the result
    /// ([`SendWatch`]): a failure is counted, reported when due and may
    /// name the multicast interface again; true when it asks a reopen.
    async fn out<S>(
        &mut self,
        sock: &UdpSocket,
        packet: &[u8],
        to: SocketAddrV4,
        ctx: &Ctx,
        send: &S,
    ) -> bool
    where
        S: AsyncFn(&UdpSocket, &[u8], SocketAddrV4) -> io::Result<usize>,
    {
        let e = match send(sock, packet, to).await {
            Ok(_) => {
                if let Some(failures) = self.watch.sent() {
                    info!(failures, "info multicast sends work again");
                }
                return false;
            }
            Err(e) => e,
        };
        ctx.requests.info_send_failed();
        let failed = self.watch.failed(Instant::now(), &e);
        if let Some(r) = &failed.report {
            report_failures(r);
        }
        match failed.recover {
            None => false,
            Some(Recover::Reapply) => {
                ctx.requests.info_send_recovered();
                reapply(sock);
                false
            }
            Some(Recover::Reopen) => {
                ctx.requests.info_send_recovered();
                true
            }
        }
    }
}

/// The failed sends since the last report, as one warning.
fn report_failures(r: &sendwatch::Report) {
    warn!(
        failures = r.failures,
        error = %r.error,
        "info packets could not be sent (counted; one line a minute at most)"
    );
}

/// Names `sock`'s multicast interface again, by its address and on
/// Windows, when that fails, by the index of the address's interface now
/// ([`sendwatch::interface_by_index`]).
fn reapply(sock: &UdpSocket) {
    let Ok(std::net::SocketAddr::V4(at)) = sock.local_addr() else {
        return;
    };
    let s = socket2::SockRef::from(sock);
    match sendwatch::interface_by_index(
        *at.ip(),
        cfg!(windows),
        |a| s.set_multicast_if_v4(&a),
        crate::membership::links,
    ) {
        Ok(named) => info!(
            ?named,
            "the info port's multicast interface named again after failed sends"
        ),
        Err(e) => warn!(error = %e, "the info port's multicast interface not named again"),
    }
}

/// A datagram on the share socket, or never without one.
async fn recv_share(
    share: Option<&UdpSocket>,
    buf: &mut [u8],
) -> io::Result<(usize, std::net::SocketAddr)> {
    match share {
        Some(s) => s.recv_from(buf).await,
        None => std::future::pending().await,
    }
}

/// A member of the address's info port ([`share`]) on `sock` (a loopback
/// socket): registers with the manager at `manager` every
/// [`share::LEASE_EVERY`], answers each relayed request with `mc`'s identity
/// (within the bound), and hands its replies, board and product info,
/// notifications and heartbeats to the manager. Every [`share::LEASE_EVERY`]
/// it tries to bind `own_at` (`<ip>`:[`REQUEST_PORT`]): when the manager has
/// ended, it returns that socket, and the caller serves as the manager.
#[allow(clippy::too_many_arguments)]
pub async fn serve_member(
    sock: UdpSocket,
    manager: std::net::SocketAddr,
    own_at: SocketAddrV4,
    mut mc: InfoBuilder,
    to: Multicast,
    beat: Arc<dyn Beat>,
    changes: Arc<Changes>,
    requests: Arc<Requests>,
) -> io::Result<(UdpSocket, InfoBuilder)> {
    // The manager may end while this device sends to it: off Windows' report
    // of the closed port, or the next receive fails (dasio 1.11).
    crate::winsock::no_connreset(&sock)?;
    let id = share::Msg::Register(mc.id.device_id).encode();
    let hand = |dest, packet: Vec<u8>| share::Msg::Send(dest, packet).encode();
    sock.send_to(&id, manager).await?;
    sock.send_to(&hand(share::Dest::Info, mc.board_info()), manager)
        .await?;
    sock.send_to(&hand(share::Dest::Info, mc.product_info()), manager)
        .await?;
    let mut lease = tokio::time::interval(share::LEASE_EVERY);
    lease.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut every = tokio::time::interval(to.every);
    every.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut check = tokio::time::interval(limit::CHECK);
    check.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut bound = Limit::default();
    let mut buf = vec![0u8; RECV_BUFFER];
    loop {
        let out = tokio::select! {
            got = sock.recv_from(&mut buf) => {
                let (n, from) = got?;
                match share::Msg::decode(&buf[..n]) {
                    Some(share::Msg::Request(d)) if from == manager => {
                        requests.received(REQUEST_PORT, from.ip(), info_op(&d));
                        mc.set_clock(beat.clock());
                        mc.answer_within(&mut bound, Instant::now(), &d).map(|p| hand(share::Dest::Info, p))
                    }
                    _ => None,
                }
            }
            _ = lease.tick() => {
                if let Ok(own) = UdpSocket::bind(own_at).await {
                    crate::winsock::no_connreset(&own)?;
                    info!("the info port is free: this device is the address's manager now");
                    return Ok((own, mc));
                }
                Some(id.clone())
            }
            _ = every.tick() => {
                let records = beat.records();
                Some(hand(share::Dest::Heartbeat, mc.heartbeat(records.as_ref())))
            }
            () = changes.added() => {
                mc.notify_within(&mut bound, Instant::now(), &changes).map(|p| hand(share::Dest::Info, p))
            }
            _ = check.tick() => {
                mc.notify_within(&mut bound, Instant::now(), &changes).map(|p| hand(share::Dest::Info, p))
            }
        };
        if let Some(out) = out
            && let Err(e) = sock.send_to(&out, manager).await
        {
            debug!(error = %e, "a packet not handed to the info manager");
        }
    }
}

/// The info port of the device at `ip`, shared with the other devices on
/// that address ([`share`]): with `own` (`<ip>`:[`REQUEST_PORT`] bound) it
/// serves as the manager, with the loopback share socket when it binds;
/// without it as a member until the port frees, then as the manager. A
/// reopen after failed sends ([`sendwatch`]) binds the port again; when
/// another device on the address took it meanwhile, this one serves as its
/// member.
pub async fn serve_shared(
    ip: Ipv4Addr,
    own: Option<UdpSocket>,
    mc: InfoBuilder,
    to: Multicast,
    beat: Arc<dyn Beat>,
    changes: Arc<Changes>,
    requests: Arc<Requests>,
) -> io::Result<()> {
    let ctx = Ctx {
        to,
        beat,
        changes,
        requests,
    };
    serve_shared_by(ip, own, mc, &ctx, &send_multicast).await
}

/// [`serve_shared`] with its multicast sends through `send` (a test's
/// failing send, dstack #340).
pub(crate) async fn serve_shared_by<S>(
    ip: Ipv4Addr,
    mut own: Option<UdpSocket>,
    mut mc: InfoBuilder,
    ctx: &Ctx,
    send: &S,
) -> io::Result<()>
where
    S: AsyncFn(&UdpSocket, &[u8], SocketAddrV4) -> io::Result<usize>,
{
    let port = share::share_port(ip);
    let at = SocketAddrV4::new(ip, REQUEST_PORT);
    // The watch outlives a fall back to a member, so its periods hold.
    let mut watch = SendWatch::default();
    loop {
        let mut sock = match own.take() {
            Some(own) => own,
            None => {
                info!(
                    port,
                    "the info port is held: this device is a member of the address's manager"
                );
                let member = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;
                let manager = std::net::SocketAddr::from((Ipv4Addr::LOCALHOST, port));
                let (own, back) = serve_member(
                    member,
                    manager,
                    at,
                    mc,
                    ctx.to,
                    Arc::clone(&ctx.beat),
                    Arc::clone(&ctx.changes),
                    Arc::clone(&ctx.requests),
                )
                .await?;
                mc = back;
                own
            }
        };
        name_interface(&sock, ip);
        let mut share = bind_share(port).await?;
        let mut manager = Served::new(ctx.to.every, std::mem::take(&mut watch));
        manager.start(&sock, &mut mc, ctx, send).await;
        loop {
            manager
                .serve(&sock, share.as_ref(), &mut mc, ctx, send)
                .await?;
            // The share port goes with the info port: a member that takes
            // the info port in this moment becomes the manager and binds it.
            drop(share.take());
            drop(sock);
            match UdpSocket::bind(at).await {
                Ok(s) => {
                    crate::winsock::no_connreset(&s)?;
                    info!("the info socket reopened after failed sends");
                    name_interface(&s, ip);
                    sock = s;
                    share = bind_share(port).await?;
                }
                Err(e) => {
                    info!(
                        error = %e,
                        "the info port could not be bound again: this device waits as a member until it can"
                    );
                    watch = std::mem::take(&mut manager.watch);
                    break;
                }
            }
        }
    }
}

/// The manager's loopback share socket on `port`; `None` (a warning) when
/// another holds it.
async fn bind_share(port: u16) -> io::Result<Option<UdpSocket>> {
    match UdpSocket::bind((Ipv4Addr::LOCALHOST, port)).await {
        // A member may end while the manager sends to it.
        Ok(s) => crate::winsock::no_connreset(&s).map(|()| Some(s)),
        Err(e) => {
            warn!(error = %e, port, "the info manager's loopback port: no members can join");
            Ok(None)
        }
    }
}

/// Names `sock`'s multicast interface by the device's address.
pub(crate) fn name_interface(sock: &UdpSocket, ip: Ipv4Addr) {
    if let Err(e) = socket2::SockRef::from(sock).set_multicast_if_v4(&ip) {
        warn!(error = %e, "the info port's multicast interface not named");
    }
}
