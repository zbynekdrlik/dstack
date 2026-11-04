// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The running device's audio side (Gen 3 phase B, plan Task 7): what the
//! daemon builds once and every server shares.
//!
//! - [`Media`]: the media clock (PTP time from dsync's poller), the RX
//!   channels' rings, the TX side ([`TxShared`], the flow-control server's
//!   table and the TX thread's plans and sources), the RX side
//!   ([`RxShared`], our flow sockets and the RX thread's plans and routes),
//!   the subscription [`Manager`], the request counts and the checks the
//!   control socket reads. ARC lists its flows ([`Media::flows`]), the
//!   heartbeat reads its records from it ([`Media::beat`]).
//! - [`Threads`]: the TX and RX threads ([`Media::spawn_threads`]); asked to
//!   end and waited for at the stop, never killed.
//! - [`Driver`]: the control side of our RX. Every
//!   [`crate::subscribe::TICK`] the manager's round ([`Driver::tick`]): its
//!   flow requests and stops go out through our flow-control client, one
//!   task each, their answers come back through [`Answers`]
//!   ([`Driver::answered`]); the RX thread gets the plans and routes; the
//!   RX channels whose status moved go to the channel-change notification;
//!   the checks read their rings ([`Media::step_checks`]).
//! - The stop ([`Driver::stop`]): every RX flow we were granted is stopped
//!   at its transmitter, one datagram each, all sent at once and their
//!   answers awaited [`STOP_WAIT`] at most; a request still in flight is
//!   dropped (its flow, if granted, gets no keepalive and expires at its
//!   transmitter).
//!
//! - The engine (phase C): [`Media::new`] builds an engine ring on each TX
//!   channel the engine feeds; [`Media::attach_engine`] shows its pump on
//!   the control socket's status ([`Media::engine`]).
//! - The app's own part of the status (dstack #104):
//!   [`Media::set_app_status`] sets a hook whose JSON object the status
//!   carries as `app` ([`Media::app_status`]). The hook runs on a thread of
//!   its own, never on the control task ([`app`], step 5).
//!
//! Locks are taken in the device's order (the device, then the TX control,
//! then the manager); the checks' lock, the engine's and the app hook's are
//! taken alone (the app hook runs on its own thread, outside its lock).

use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::AtomicI32;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tracing::{info, warn};

use crate::arc::Running;
use crate::clock::{ClockStats, PtpClock, to_samples};
use crate::ctl::{Checks, Limits, Playout, Requests};
use crate::device::Device;
use crate::flows::client::Client;
use crate::flows::control::op;
use crate::info::{Changes, Sources};
use crate::media::ring::Ring;
use crate::media::rx::RxShared;
use crate::media::tx::TxShared;
use crate::media::{rx, tx};
use crate::stream::{PumpStats, PumpView};
use crate::subscribe::resolve::Resolver;
use crate::subscribe::{Action, Answer, FlowId, Manager};

pub mod app;

pub use app::AppStatus;

/// How long the stop waits for the transmitters' answers to our stops:
/// every stop has gone out by then.
pub const STOP_WAIT: Duration = Duration::from_secs(1);
/// How long the stop waits for the TX and RX threads to end (each ends
/// within one of its waits, 33 ms).
pub const THREADS_WAIT: Duration = Duration::from_secs(1);

/// The media clock and what its poller keeps beside it.
#[derive(Clone)]
pub struct Clock {
    pub ptp: Arc<dyn PtpClock>,
    /// dsync's frequency adjustment, ppb (the heartbeat's 0x8001).
    pub freq: Arc<AtomicI32>,
    pub stats: Arc<ClockStats>,
}

/// The running device's audio side (see the module).
pub struct Media {
    /// Our address: the flow sockets', and where our flow requests leave
    /// from.
    pub ip: Ipv4Addr,
    pub clock: Arc<dyn PtpClock>,
    pub freq: Arc<AtomicI32>,
    pub clock_stats: Arc<ClockStats>,
    pub tx: Arc<TxShared>,
    pub rx: Arc<RxShared>,
    pub manager: Arc<Mutex<Manager>>,
    pub requests: Arc<Requests>,
    pub checks: Mutex<Checks>,
    /// The engine's pump once one is attached (phase C): the control
    /// socket's status shows it.
    engine: Mutex<Option<Arc<PumpStats>>>,
    /// The channels' levels the heartbeat leaves for the meter frames.
    pub levels: Arc<crate::info::Levels>,
    /// The app's status hook, its thread and its cached part
    /// ([`Media::set_app_status`]).
    app: app::AppHook,
}

/// One RX channel's state ([`Media::rx_channels`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RxChannel {
    /// The channel, from 1.
    pub rx: usize,
    /// What it is subscribed to, if anything.
    pub subscription: Option<crate::device::Subscription>,
    /// Its subscription's status (Dante's word and state:
    /// [`crate::subscribe::SubStatus::label`], `::state`).
    pub status: Option<crate::subscribe::SubStatus>,
    /// The RX flow it rides, from 1.
    pub flow: Option<usize>,
    /// That flow's packets so far.
    pub packets: u64,
    /// The samples written for this channel so far.
    pub written: u64,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Media {
    /// The audio side of `device` at `ip`: one ring per RX channel, the TX
    /// side (32 sockets on `ip`, cookies from `seed`, every TX channel
    /// silent, an engine ring on each TX channel index in `engine_tx`, from
    /// 0: the engine's; the standalone device passes none), the RX side (32
    /// flow sockets on `ip`), and the manager with the state file's
    /// subscriptions (their statuses the baseline of its first `changed`).
    /// Fails when a socket cannot be bound, or for an `engine_tx` index the
    /// device lacks (`InvalidInput`).
    pub fn new(
        device: &Device,
        ip: Ipv4Addr,
        clock: Clock,
        seed: u64,
        engine_tx: &[usize],
    ) -> std::io::Result<Media> {
        let cfg = &device.cfg;
        let rings: Arc<[Ring]> = (0..cfg.rx.len()).map(|_| Ring::new()).collect();
        let tx = Arc::new(TxShared::new(
            cfg.clone(),
            ip,
            Arc::clone(&rings),
            seed,
            engine_tx,
        )?);
        let rx = Arc::new(RxShared::with_slots(
            cfg.sample_rate,
            ip,
            rings,
            cfg.rx_port_base,
            cfg.flow_slots,
        )?);
        let mut manager = Manager::new(cfg, &device.state.name, ip, rx.ports()?);
        manager.load(&device.state.subscriptions);
        manager.changed();
        Ok(Media {
            ip,
            clock: clock.ptp,
            freq: clock.freq,
            clock_stats: clock.stats,
            tx,
            rx,
            manager: Arc::new(Mutex::new(manager)),
            requests: Arc::default(),
            checks: Mutex::new(Checks::new(cfg.rx.len(), cfg.sample_rate)),
            engine: Mutex::new(None),
            levels: Arc::default(),
            app: app::AppHook::default(),
        })
    }

    /// Shows the engine's pump counting into `stats` on the control
    /// socket's status (phase C; the engine attaches it once its pump is
    /// built, a later attach replaces it).
    pub fn attach_engine(&self, stats: Arc<PumpStats>) {
        *lock(&self.engine) = Some(stats);
    }

    /// Each RX channel's state for an application (dstack #20, the
    /// intercom: tell "the flow is lost" from "the source is quiet"): its
    /// subscription and status, the flow it rides and that flow's packets,
    /// and the samples written for it. A count that stops growing while
    /// the status reads receiving is a flow that stopped.
    pub fn rx_channels(&self) -> Vec<RxChannel> {
        let rings = self.rx.rings();
        let m = lock(&self.manager);
        (0..rings.len())
            .map(|rx| {
                let route = m.rx_route(rx);
                RxChannel {
                    rx: rx + 1,
                    subscription: m.subscription(rx).cloned(),
                    status: m.status(rx),
                    flow: route.map(|r| r.flow + 1),
                    packets: route.map_or(0, |r| self.rx.lane(r.flow).packets),
                    written: rings[rx].counters().written,
                }
            })
            .collect()
    }

    /// Sets the app's own part of the status (dstack #104): the status
    /// carries what `f` returns as `app` when that text is one JSON object
    /// of at most [`crate::ctl::APP_MAX`] bytes ([`crate::ctl::app_object`]).
    /// A later call replaces the hook (its old part dropped; a refusal of
    /// the new one is warned again).
    ///
    /// `f` runs on a thread of its own (`dstack-app-status`, started at the
    /// first call, ended with the `Media`), never inside a status request:
    /// at most once every [`app::REFRESH`], and only while the status is
    /// read (a read within [`app::READ_WINDOW`]). The status carries the
    /// last part `f` returned, so a slow `f` never delays the device's ARC,
    /// CMC or status answers; a part older than [`app::STALE`] carries
    /// `"stale":true` (dtray shows it orange), and a hook still running then
    /// is warned once. The first status after the hook is set (or after a
    /// pause in reading) has no part yet or a stale one; the thread calls
    /// `f` at once for it. A panic in `f` keeps the last part (warned once;
    /// `f` is called again at the next refresh). A hook replaced while its
    /// call stalls holds the thread: the new one runs once that call
    /// returns. `f` makes no status request of its own
    /// ([`crate::ctl::status`], `Link::status`): each counts as a read and
    /// would keep `f` called once a second for good.
    ///
    /// dtray reads three keys of it: `line`, a short text shown after the
    /// device's own line, `state`, `"ok"`, `"warn"` (orange) or `"error"`
    /// (red), and `stale` (dstack's: set over the app's own). Every other
    /// key is the app's own; dtray shows it only in Copy status. Unlike the
    /// rest of the status, the part may carry names: it is the app's.
    pub fn set_app_status(&self, f: impl Fn() -> String + Send + Sync + 'static) {
        self.app.set(Arc::new(f));
    }

    /// Sends this device's TX through the host's `sender` (dstack #1): its
    /// own kernel send path (an NDIS filter), so packets skip Winsock's send
    /// path. Call it before the media threads start (before serving; dlink
    /// and [`crate::embed`] pass it through). Windows only: the TX thread
    /// then writes whole Ethernet II frames from our NIC's MAC and hands the
    /// frames of each pass to `sender` in one call, for unicast receivers
    /// the neighbour table resolved; multicast, unresolved receivers and any
    /// frame the sender did not take go through the UDP socket as before.
    /// `false` (logged): not Windows (Linux has its own path past the
    /// qdisc), no adapter holds the device's address, or a sender was set
    /// already; TX stays as it was.
    pub fn set_frame_sender(&self, sender: Arc<dyn crate::media::bypass::FrameSender>) -> bool {
        #[cfg(windows)]
        {
            let Some((ifindex, mac, name)) = crate::media::bypass::windows::nic(self.ip) else {
                warn!(ip = %self.ip, "no adapter holds the device's address: TX stays on the UDP socket");
                return false;
            };
            let nic = crate::media::tx::HostNic { ifindex, mac, name };
            if !self.tx.use_frame_sender(sender, nic) {
                warn!("a host frame sender was set already: the first one stays");
                return false;
            }
            true
        }
        #[cfg(not(windows))]
        {
            let _ = sender;
            info!("a host frame sender is used on Windows only: TX keeps its own path");
            false
        }
    }

    /// The app's part of the status as compact JSON (the cache the hook's
    /// thread keeps, never a call of the hook): `None` without a hook,
    /// before its first call returned, or when its text is no JSON object
    /// of at most [`crate::ctl::APP_MAX`] bytes (left out, and warned the
    /// first time for each hook); `"stale":true` set when the part is older
    /// than [`app::STALE`].
    pub fn app_status(&self) -> Option<String> {
        self.app.read()
    }

    /// The attached engine's numbers, `None` before one is attached.
    pub fn engine(&self) -> Option<PumpView> {
        lock(&self.engine).as_ref().map(|stats| stats.view())
    }

    /// The attached engine's counts, `None` before one is attached (the
    /// pump watch reads them each tick).
    pub fn engine_stats(&self) -> Option<Arc<PumpStats>> {
        lock(&self.engine).clone()
    }

    /// The numbers a control request is checked against.
    pub fn limits(&self) -> Limits {
        Limits {
            tx: self.tx.cfg.tx.len(),
            rx: self.tx.cfg.rx.len(),
            rate: self.tx.cfg.sample_rate,
        }
    }

    /// The flows as ARC lists them and where it leaves the subscriptions.
    pub fn flows(&self) -> Running {
        Running {
            tx: Arc::clone(&self.tx),
            rx: Arc::clone(&self.manager),
        }
    }

    /// The heartbeat's records.
    pub fn beat(&self) -> Sources {
        Sources {
            clock: Arc::clone(&self.clock),
            freq: Arc::clone(&self.freq),
            tx: Arc::clone(&self.tx),
            rx: Arc::clone(&self.rx),
            manager: Arc::clone(&self.manager),
            levels: Arc::clone(&self.levels),
            stats: Arc::clone(&self.clock_stats),
        }
    }

    /// Hands `m`'s plans and routes to the RX thread.
    fn publish(&self, m: &Manager) {
        self.rx.publish(|slot| m.rx_plan(slot), |rx| m.rx_route(rx));
        self.rx.set_device_latency(m.latency_ns());
    }

    /// One round of the checks at the media clock's now (none while the
    /// clock is not ready): each checked RX channel whose flow was granted
    /// reads its ring up to its playout point.
    pub fn step_checks(&self) {
        let Some(now_ns) = self.clock.now_ns() else {
            return;
        };
        let rings = self.rx.rings();
        let plays: Vec<Option<Playout>> = {
            let m = lock(&self.manager);
            (0..rings.len())
                .map(|rx| {
                    let plan = m.rx_plan(m.rx_route(rx)?.flow);
                    plan.live.then_some(Playout {
                        latency: plan.latency,
                        bits: plan.bps.saturating_mul(8),
                    })
                })
                .collect()
        };
        lock(&self.checks).step(to_samples(now_ns, self.tx.cfg.sample_rate), rings, &plays);
    }

    /// Starts the TX and RX threads on the media clock. When the RX thread
    /// cannot start, the TX thread is ended again.
    pub fn spawn_threads(&self) -> std::io::Result<Threads> {
        let tx = tx::spawn(Arc::clone(&self.clock), Arc::clone(&self.tx))?;
        match rx::spawn(
            Arc::clone(&self.clock),
            Arc::clone(&self.rx),
            self.tx.cfg.rx_thread_cpu(),
        ) {
            Ok(rx) => Ok(Threads { tx, rx }),
            Err(e) => {
                self.tx.stop();
                let _ended = tx.join();
                Err(e)
            }
        }
    }
}

/// The TX and RX threads.
#[derive(Debug)]
pub struct Threads {
    tx: JoinHandle<()>,
    rx: JoinHandle<()>,
}

impl Threads {
    /// Asks both threads to end and waits up to `within` for them; `false`,
    /// and a warning, when one still runs then (it is left to end, never
    /// killed). Glue: the threads' loops end at their own stop flags.
    pub async fn stop(self, media: &Media, within: Duration) -> bool {
        media.tx.stop();
        media.rx.stop();
        let deadline = tokio::time::Instant::now() + within;
        while !(self.tx.is_finished() && self.rx.is_finished()) {
            if tokio::time::Instant::now() >= deadline {
                warn!(
                    wait_ms = within.as_millis(),
                    "a media thread did not end within the wait: left to end"
                );
                return false;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let _ = self.tx.join();
        let _ = self.rx.join();
        true
    }
}

/// The answers to our flow requests, as their tasks hand them back: the
/// daemon's loop passes each to [`Driver::answered`].
pub type Answers = mpsc::UnboundedReceiver<(FlowId, Answer)>;

/// The control side of our RX (see the module).
pub struct Driver {
    media: Arc<Media>,
    client: Arc<Client>,
    changes: Arc<Changes>,
    answers: mpsc::UnboundedSender<(FlowId, Answer)>,
    /// Flow requests waiting for their answers.
    requests: JoinSet<()>,
    stops: JoinSet<()>,
    /// Our TX flows' late packets, logged once a second.
    late: crate::media::late::LateWatch,
    /// Our RX flows' missing packets, logged once a second.
    loss: crate::media::late::RxWatch,
    /// Our TX flows' failed sends, logged once a second with the OS error.
    sends: crate::media::late::SendWatch,
    /// The host frame sender's refused batches, logged once a second.
    host: crate::media::late::HostWatch,
    /// The engine pump's re-bootstraps, skipped blocks and overruns, logged
    /// once a second.
    pump: crate::stream::watch::PumpWatch,
    /// Other traffic on our NIC, logged once a minute.
    traffic: crate::traffic::TrafficWatch,
}

impl Driver {
    /// The driver of `media`'s manager, sending through `client` and
    /// telling `changes` the RX channels whose status moved; and where the
    /// answers come back.
    pub fn new(media: Arc<Media>, client: Client, changes: Arc<Changes>) -> (Driver, Answers) {
        let (answers, back) = mpsc::unbounded_channel();
        (
            Driver {
                media,
                client: Arc::new(client),
                changes,
                answers,
                requests: JoinSet::new(),
                stops: JoinSet::new(),
                late: crate::media::late::LateWatch::new(),
                loss: crate::media::late::RxWatch::new(),
                sends: crate::media::late::SendWatch::new(),
                host: crate::media::late::HostWatch::new(),
                pump: crate::stream::watch::PumpWatch::new(),
                traffic: crate::traffic::TrafficWatch::new(),
            },
            back,
        )
    }

    /// One round of the manager now, with the channel records `resolver`
    /// knows; then the checks' round and the late, loss, send, host, pump
    /// and traffic watches.
    pub fn tick(&mut self, resolver: &dyn Resolver) {
        let now = self.media.rx.now_ns();
        let packets = self.media.rx.packets();
        let actions = self.with_manager(|m| m.tick(now, &packets, resolver));
        self.act(actions);
        self.media.step_checks();
        self.late.watch(now, &self.media.tx);
        self.loss.watch(now, &self.media.rx);
        self.sends.watch(now, &self.media.tx);
        self.host.watch(now, &self.media.tx);
        self.pump.watch(
            now,
            &self.media.tx.cfg.name,
            self.media.engine_stats().as_deref(),
        );
        self.traffic.watch(now, self.media.ip, &self.media.tx);
    }

    /// The answer to the request of `flow`.
    pub fn answered(&mut self, flow: FlowId, answer: Answer) {
        match answer {
            Answer::Granted(_) => info!(flow = flow.slot + 1, "RX flow granted"),
            Answer::Refused(status) => info!(
                flow = flow.slot + 1,
                status = format_args!("{status:#06x}"),
                "RX flow refused"
            ),
            Answer::Failed => info!(flow = flow.slot + 1, "RX flow request had no answer"),
        }
        let now = self.media.rx.now_ns();
        let actions = self.with_manager(|m| m.answered(flow, answer, now));
        self.act(actions);
    }

    /// The stop: every RX flow we were granted is stopped at its
    /// transmitter (the stops all go out at once, their answers awaited
    /// [`STOP_WAIT`] at most), every request in flight is dropped, and the
    /// RX thread receives nothing more.
    pub async fn stop(&mut self) {
        self.requests.abort_all();
        let actions = self.with_manager(Manager::stop_all);
        let flows = actions.len();
        self.act(actions);
        let all = tokio::time::timeout(STOP_WAIT, async {
            while self.stops.join_next().await.is_some() {}
        })
        .await
        .is_ok();
        self.stops.abort_all();
        info!(
            flows,
            answered = all,
            "RX flows stopped at their transmitters"
        );
    }

    /// `f` on the manager, then the plans and routes to the RX thread and
    /// the statuses that moved to the notification.
    fn with_manager(&self, f: impl FnOnce(&mut Manager) -> Vec<Action>) -> Vec<Action> {
        let (actions, moved) = {
            let mut m = lock(&self.media.manager);
            let actions = f(&mut m);
            self.media.publish(&m);
            (actions, m.changed())
        };
        self.changes.add(moved);
        actions
    }

    /// Sends the manager's datagrams, one task each; finished tasks are
    /// reaped.
    fn act(&mut self, actions: Vec<Action>) {
        while self.requests.try_join_next().is_some() {}
        while self.stops.try_join_next().is_some() {}
        for action in actions {
            match action {
                Action::Request { flow, to, req } => {
                    let peer = self.media.requests.sent(IpAddr::V4(*to.ip()), op::REQUEST);
                    info!(
                        peer,
                        flow = flow.slot + 1,
                        channels = req.channels.len(),
                        fpp = req.fpp,
                        "RX flow requested"
                    );
                    let client = Arc::clone(&self.client);
                    let answers = self.answers.clone();
                    self.requests.spawn(async move {
                        let answer = Answer::of(&client.request_flow(to, &req).await);
                        // The loop is gone only at the stop, which wants no
                        // answer.
                        let _ = answers.send((flow, answer));
                    });
                }
                Action::Stop { to, handle } => {
                    let peer = self.media.requests.sent(IpAddr::V4(*to.ip()), op::STOP);
                    info!(peer, "RX flow stop sent");
                    let client = Arc::clone(&self.client);
                    self.stops.spawn(async move {
                        if let Err(e) = client.stop_flow(to, &handle).await {
                            warn!(peer, error = %e, "an RX flow stop was not answered");
                        }
                    });
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
