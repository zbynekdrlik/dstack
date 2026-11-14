// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.
// Protocol facts (byte layouts, opcodes, behaviour) checked against
// inferno (https://github.com/teodly/inferno) as a reference; the code is
// our own.

//! Our RX subscriptions (Gen 3 phase B, plan Task 5): which TX channel of
//! which device each RX channel receives ([`crate::device::Subscription`],
//! set on our own device, kept in the state file), and the flows that
//! bring them.
//!
//! [`Manager`] is the pure core, driven by the control side: [`Manager::set`]
//! (a subscription changed), [`Manager::tick`] (every
//! [`TICK`]: the packets each flow slot received, the records the
//! [`resolve::Resolver`] knows), [`Manager::answered`] (a request's answer)
//! and [`Manager::stop_all`] (shutdown). It returns [`Action`]s, the only
//! datagrams our RX ever sends besides keepalives: a flow request to a
//! transmitter for its channels to one of our flow slots, and a stop. It
//! never changes another device's subscriptions: it asks transmitters for
//! flows of our own RX channels only.
//!
//! **What bounds our requests (network safety, plan Review Focus 1):** per
//! transmitter (by address) one round of requests at a time, one request
//! for each flow its waiting channels need (dstack #20: band's two flows
//! were asked 9 s apart), and at most one round per [`RETRY_NS`] (9 s),
//! counted from the last request whatever its answer; each failure in a
//! row (a refusal, no answer within [`GIVE_UP_NS`], a flow that stays
//! silent) doubles the wait up to [`RETRY_MAX_NS`] ([`retry_after`]), and
//! audio received ends the row. A request with no answer is the one
//! exception: asked again [`QUICK_RETRY_NS`] after it for as long as the
//! transmitter stays silent, as a transmitter that is starting answers
//! nothing until its clock is ready (since v0.6.63; mbc-snv took 37 s after
//! a reboot). Any answer ends it; a refusal then backs off.
//! Stops are one datagram per flow we were granted. Nothing here depends on
//! what a transmitter answers to stay inside these bounds.
//!
//! **Flows.** Up to [`MAX_FLOWS`] (inferno's 32), each in one of our flow
//! slots: the slot's socket ([`crate::media::rx::RxShared`]) is where the
//! transmitter sends the audio. A flow has `min(nchan, 8, our RX count)`
//! channel slots, filled with the TX channels its transmitter's waiting
//! channels need (in RX order, one slot per TX channel however many RX
//! channels ask for it), the rest empty; the frames per packet are the
//! transmitter's most that fit one datagram; the latency is the larger of
//! its record's and ours. A flow no channel uses any more is stopped; a
//! flow that has received nothing for [`SILENT_NS`] once it is
//! [`YOUNG_NS`] old is stopped and its channels resolved again. A channel
//! whose subscription changes leaves its flow; phase B sends no update
//! (0x0102): a flow keeps the slots it was asked with.
//!
//! **Statuses** ([`SubStatus`], Dante's codes; our server lists inferno's
//! few): a subscribed channel is
//! Unresolved without a flow (or TX fail / too many TX flows after its last
//! request failed), InProgress while its flow is asked for or quiet, and
//! Receiving while its flow's audio arrives.
//!
//! **Multicast flows** (dstack #148): a transmitter's own multicast flow that
//! carries a channel ([`Resolver::multicast`], read from its ARC flow list) is
//! received instead of asking for a unicast flow: one of our flow slots joins
//! its group ([`RxPlan::group`](crate::media::rx::RxPlan::group)), no request,
//! no stop and no keepalive is sent, and its channels show Receiving
//! (multicast). While the transmitter's flows are being read the channel waits,
//! so no unicast flow takes a transmitter's last slot first. A channel on a
//! unicast flow moves to a multicast flow that appears (its unicast flow is
//! stopped, a slot at the transmitter freed); one on a multicast flow that is
//! gone, moved or silent is resolved again. We never create, change or end
//! another device's multicast flow here.
//!
//! **A coarser multicast flow** (dstack #297, `subscribe/coarse.rs`): on a
//! device whose config sets `arc_rx_fpp` (the opt-in; a device on the
//! default keeps every multicast flow as above), a multicast flow is
//! preferred only when its fpp is at most what our own unicast flow from
//! that transmitter would carry ([`fpp_for`], the transmitter's step).
//! A channel on a multicast flow with more frames a packet asks for its
//! own unicast flow, only while the transmitter's flow list shows a free
//! TX flow slot, and keeps the multicast flow until the unicast flow
//! delivers (make-before-break); a refusal, no answer or no free slot
//! keeps the multicast flow for that transmitter while the manager lives,
//! with no other side effect. A unicast flow never moves to a coarser
//! multicast flow.
//!
//! The RX thread learns the flows through [`Manager::rx_plan`] and
//! [`Manager::rx_route`] (handed over by `RxShared::publish`). Times are the
//! control side's ns.

mod coarse;
pub mod directory;
pub mod instance;
mod publish;
mod request;
pub mod resolve;
mod slots;
pub mod sub_status;

use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, SocketAddrV4};

pub use request::fpp_for;
pub use sub_status::{SubState, SubStatus};

use crate::device::{DeviceConfig, Subscription};
use crate::flows::client::ClientError;
use crate::flows::control::{FlowRequest, Handle, status};
use crate::media::packet::MEDIA_HEADER;
use crate::media::tx::MAX_CHANNELS;
use crate::wire::MAX_DATAGRAM;
use resolve::{Advertised, Resolver};
// The tests (`use super::*`) read these names; the code that uses them
// lives in the child modules.
#[cfg(test)]
use crate::media::rx::{RxPlan, RxRoute};
#[cfg(test)]
use resolve::{Multicast, MulticastLookup};

pub use crate::media::rx::MAX_FLOWS;

/// How often the control side calls [`Manager::tick`].
pub const TICK_NS: u64 = 250_000_000;
/// [`TICK_NS`] as a duration.
pub const TICK: std::time::Duration = std::time::Duration::from_nanos(TICK_NS);
/// The least time between two requests to one transmitter (inferno's 9 s
/// between resolves).
pub const RETRY_NS: u64 = 9_000_000_000;
/// The wait from an unanswered request to the next one (it is given up
/// after [`GIVE_UP_NS`], so 2 s after that).
pub const QUICK_RETRY_NS: u64 = 6_000_000_000;
/// The largest fpp a refusal with [`status::FPP_REFUSED`] steps up to:
/// the most Dante devices advertise ([`crate::announce::FPP_MAX`]).
pub const FPP_STEP_MAX: u16 = crate::announce::FPP_MAX;
/// Every fpp step fits one datagram in any flow [`fpp_for`] accepts (at
/// most [`MAX_CHANNELS`] slots of at most 4 bytes).
const _: () = assert!(MEDIA_HEADER + MAX_CHANNELS * 4 * FPP_STEP_MAX as usize <= MAX_DATAGRAM);
/// Transmitters whose fpp step is kept at most.
pub const MAX_FPP_FLOORS: usize = 64;
/// The most time between two requests to a transmitter that keeps failing.
pub const RETRY_MAX_NS: u64 = 72_000_000_000;
/// A request without an answer for this long has failed (the client waits
/// 3 s; this catches an answer that was never handed over).
pub const GIVE_UP_NS: u64 = 4_000_000_000;
/// A flow without a packet for this long is silent (inferno's 2 s).
pub const SILENT_NS: u64 = 2_000_000_000;
/// A flow younger than this is never stopped for silence (inferno's 6 s).
pub const YOUNG_NS: u64 = 6_000_000_000;

/// The wait before the next request to a transmitter after `failures`
/// failures in a row: 9 s for none or one, then doubling to 72 s.
pub fn retry_after(failures: u32) -> u64 {
    (RETRY_NS << failures.saturating_sub(1).min(3)).min(RETRY_MAX_NS)
}

/// One flow: its slot and which of the slot's flows it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlowId {
    pub slot: usize,
    pub generation: u32,
}

/// A datagram to send for the manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Ask the transmitter's flow control `to` for the flow `req`; hand
    /// the answer to [`Manager::answered`] with `flow`.
    Request {
        flow: FlowId,
        to: SocketAddrV4,
        req: FlowRequest,
    },
    /// Tell the transmitter's flow control `to` to end the flow `handle`
    /// (once; its answer changes nothing).
    Stop { to: SocketAddrV4, handle: Handle },
}

/// A request's answer, as the manager reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Granted(Handle),
    /// The transmitter's status.
    Refused(u16),
    /// No answer, or a socket error.
    Failed,
}

impl Answer {
    /// The client's result.
    pub fn of(r: &Result<Handle, ClientError>) -> Answer {
        match r {
            Ok(h) => Answer::Granted(*h),
            Err(ClientError::Refused(status)) => Answer::Refused(*status),
            Err(_) => Answer::Failed,
        }
    }
}

/// One of our flows.
#[derive(Debug, Clone)]
struct Flow {
    generation: u32,
    /// The transmitter's flow control.
    to: SocketAddrV4,
    /// Its TX channel in each slot.
    slots: Vec<Option<u16>>,
    bits: u32,
    fpp: u16,
    latency_ns: u64,
    /// The transmitter's device name, lower-case (the `playout_override`
    /// key), and its advertised latency (0 when it advertises none).
    device: String,
    advertised_ns: u64,
    /// The transmitter's device name as the subscription writes it, for
    /// the status and the logs (dstack #266).
    name: String,
    /// The record it was asked for from (a changed record restarts it).
    advert: Advertised,
    /// When it was asked for.
    since: u64,
    /// The transmitter's handle; `None` while asked for (and for a
    /// multicast flow, which is never asked for).
    handle: Option<Handle>,
    /// A multicast flow's group: received, never asked for (dstack #148).
    group: Option<SocketAddrV4>,
    /// When its slot last received a packet.
    heard: Option<u64>,
    /// The slot's packet count when last looked at.
    packets: u64,
    /// Asked for channels that receive a coarser multicast flow (dstack
    /// #297): they move to it once it delivers, never before.
    upgrade: bool,
}

impl Flow {
    /// Asked for and not granted yet: a request waits on its answer.
    fn asked(&self) -> bool {
        self.handle.is_none() && self.group.is_none()
    }
}

/// A transmitter we ask, by address.
#[derive(Debug, Clone, Default)]
struct Peer {
    /// Its requests not answered yet.
    in_flight: Vec<FlowId>,
    last_request: Option<u64>,
    /// Failures in a row: rounds, not flows (dstack #20).
    failures: u32,
    /// The round (its request time) whose failure is counted already.
    counted: Option<u64>,
    /// Requests in a row that got no answer at all.
    unanswered: u32,
}

impl Peer {
    /// A flow of the last round failed: the round counts once, however
    /// many of its flows fail (iemmixer, 10.11.2026: band's two flows
    /// counted twice and the re-request waited 72 s).
    fn count_failure(&mut self) {
        if self.counted != self.last_request {
            self.failures += 1;
            self.counted = self.last_request;
        }
    }
}

/// Where a channel's audio comes from: its flow's slot and channel slot,
/// for the subscription it was routed for.
#[derive(Debug, Clone)]
struct Route {
    flow: FlowId,
    slot: usize,
    sub: Subscription,
}

/// A flow as the ARC lists show it (plan Task 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowView {
    pub id: FlowId,
    /// Our flow slot's UDP port, where the audio comes.
    pub port: u16,
    /// The transmitter's flow control.
    pub to: SocketAddrV4,
    /// `None` while asked for, and for a multicast flow.
    pub handle: Option<Handle>,
    /// A multicast flow's group (dstack #148).
    pub group: Option<SocketAddrV4>,
    /// The TX channel of each channel slot.
    pub channels: Vec<Option<u16>>,
    /// Our RX channels (from 0) each channel slot feeds.
    pub rx: Vec<Vec<usize>>,
    pub bits: u32,
    pub fpp: u16,
    pub latency_ns: u64,
    /// The transmitter's advertised latency (0: none).
    pub advertised_ns: u64,
    /// Its playout is below that, by the device config's
    /// `playout_override` (the owner's approval).
    pub below_advertised: bool,
    /// The transmitter's Dante device name, as the subscription that asked
    /// for the flow writes it (dstack #266).
    pub device: String,
}

/// Our RX subscriptions and their flows.
#[derive(Debug)]
pub struct Manager {
    host: String,
    ip: Ipv4Addr,
    /// Our multicast sockets are bound to the port only
    /// ([`crate::media::rx::GROUP_BOUND_TO_PORT`]).
    port_only: bool,
    ports: [u16; MAX_FLOWS],
    rate: u32,
    latency_ns: u64,
    /// The pump's block, samples (the device config's).
    block: u64,
    /// Our process id (the config's), in our flows' names.
    process: u16,
    /// The config's `playout_override`, by lower-case device name.
    playout_override: BTreeMap<String, u32>,
    subs: Vec<Option<Subscription>>,
    routes: Vec<Option<Route>>,
    /// A channel's last request failed this way (until it is routed again
    /// or its subscription changes).
    failed: Vec<Option<SubStatus>>,
    flows: Vec<Option<Flow>>,
    /// When each flow slot was last freed: a new flow takes the slot freed
    /// longest ago (a transmitter still sending to an old flow's port finds
    /// no new flow there).
    freed: [Option<u64>; MAX_FLOWS],
    /// The slots it fills: 0 to this (the device's `flow_slots`, the RX
    /// thread's sockets).
    slots: usize,
    generation: u32,
    peers: BTreeMap<SocketAddrV4, Peer>,
    /// The least fpp each transmitter is asked for, after it refused a
    /// smaller one with [`status::FPP_REFUSED`] (at most [`MAX_FPP_FLOORS`]
    /// transmitters; kept while the manager lives).
    fpp_floor: BTreeMap<SocketAddrV4, u16>,
    /// Requests given up on: a grant that comes after all is stopped. The
    /// flag marks one asked for channels on a multicast flow (dstack #297),
    /// whose answer changes nothing else.
    abandoned: Vec<(FlowId, SocketAddrV4, bool)>,
    /// Transmitters whose channels stay on their multicast flow: our own
    /// unicast flow was refused, never answered or silent, or they showed
    /// no free TX flow slot (dstack #297; at most `coarse::MAX_KEPT`, kept
    /// while the manager lives).
    multicast_kept: BTreeSet<SocketAddrV4>,
    /// The device config sets `arc_rx_fpp`: it opts in to its own unicast
    /// flow over a coarser multicast flow (dstack #297); left out, every
    /// multicast flow that carries a channel is preferred, as before.
    own_unicast: bool,
    /// The statuses [`Manager::changed`] reported last.
    reported: Vec<Option<SubStatus>>,
    /// The last tick's time.
    now: u64,
}

impl Manager {
    /// Our device's name changed: the next flow requests carry `host`.
    pub fn set_host(&mut self, host: &str) {
        self.host = host.to_string();
    }

    /// Our device's name as it is now ([`Manager::set_host`]): the status
    /// carries it (dstack #209).
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The device latency is `ns` now (Dante Controller set it): every
    /// flow's playout and delay follow from the next plan read
    /// ([`Manager::rx_plan`]).
    pub fn set_latency(&mut self, ns: u64) {
        self.latency_ns = ns;
    }

    /// The device latency the flows are planned with, ns.
    pub fn latency_ns(&self) -> u64 {
        self.latency_ns
    }

    /// The manager of the device `cfg` named `host` at `ip`, whose flow
    /// slots receive on `ports`; no subscription yet.
    pub fn new(cfg: &DeviceConfig, host: &str, ip: Ipv4Addr, ports: [u16; MAX_FLOWS]) -> Self {
        let n = cfg.rx.len();
        Manager {
            host: host.to_string(),
            ip,
            ports,
            rate: cfg.sample_rate,
            latency_ns: cfg.latency_ns,
            block: cfg.block,
            process: cfg.process_id,
            playout_override: cfg
                .playout_override
                .iter()
                .map(|(d, &p)| (d.to_ascii_lowercase(), p))
                .collect(),
            subs: vec![None; n],
            routes: vec![None; n],
            failed: vec![None; n],
            flows: vec![None; MAX_FLOWS],
            slots: cfg.flow_slots.min(MAX_FLOWS),
            freed: [None; MAX_FLOWS],
            generation: 0,
            peers: BTreeMap::new(),
            fpp_floor: BTreeMap::new(),
            abandoned: Vec::new(),
            multicast_kept: BTreeSet::new(),
            own_unicast: cfg.arc_rx_fpp.is_some(),
            reported: vec![None; n],
            port_only: crate::media::rx::GROUP_BOUND_TO_PORT,
            now: 0,
        }
    }

    /// Sets RX channel `rx`'s subscription (from 0; `None` removes it);
    /// `false` for a channel we do not have. The flows follow at the next
    /// tick.
    pub fn set(&mut self, rx: usize, sub: Option<Subscription>) -> bool {
        let Some(slot) = self.subs.get_mut(rx) else {
            return false;
        };
        if *slot != sub {
            *slot = sub;
            self.failed[rx] = None;
        }
        true
    }

    /// Every subscription at once (the state file's at start).
    pub fn load(&mut self, subs: &[Option<Subscription>]) {
        for (rx, sub) in subs.iter().enumerate() {
            self.set(rx, sub.clone());
        }
    }

    /// Our address: where every flow slot receives (ARC 0x3200).
    pub fn ip(&self) -> Ipv4Addr {
        self.ip
    }

    /// RX channel `rx`'s subscription.
    pub fn subscription(&self, rx: usize) -> Option<&Subscription> {
        self.subs.get(rx)?.as_ref()
    }

    /// Whether any RX channel is subscribed.
    pub fn subscribed(&self) -> bool {
        self.subs.iter().any(Option::is_some)
    }

    /// One round at `now`: `packets` holds each flow slot's packet count
    /// (`RxShared::packets`), `resolver` the channel records known. Flows
    /// that heard packets are alive; requests without an answer for
    /// [`GIVE_UP_NS`] have failed; flows no channel uses, and flows silent
    /// for [`SILENT_NS`] once [`YOUNG_NS`] old, are stopped; channels
    /// without a flow are resolved, routed to a flow that carries their TX
    /// channel already, or asked for (one request per transmitter allowed
    /// now).
    pub fn tick(
        &mut self,
        now: u64,
        packets: &[u64; MAX_FLOWS],
        resolver: &dyn Resolver,
    ) -> Vec<Action> {
        self.now = now;
        let mut out = Vec::new();
        self.hear(now, packets);
        self.give_up(now);
        self.promote();
        // Channels whose subscription changed leave their flow.
        for (route, sub) in self.routes.iter_mut().zip(&self.subs) {
            if route.as_ref().is_some_and(|r| Some(&r.sub) != sub.as_ref()) {
                *route = None;
            }
        }
        self.reresolve(resolver);
        self.close(now, &mut out);
        let (pending, shared) = self.resolve(resolver);
        for (rx, a, m) in shared {
            self.receive_multicast(rx, &a, &m, now, packets);
        }
        for (to, waiting) in pending {
            if !self.may_request(to, now) {
                continue;
            }
            // One request for each flow the waiting channels need: each
            // routes the channels it carries; the rest go in the next.
            let mut rest = waiting;
            while let Some(action) = self.request(&rest, now, packets, false) {
                out.push(action);
                rest.retain(|(rx, _)| self.routes[*rx].is_none());
                if rest.is_empty() {
                    break;
                }
            }
        }
        self.upgrade(resolver, now, packets, &mut out);
        self.prune(now);
        out
    }

    /// The answer to the request of `flow`, at `now`. A grant for a request
    /// given up on, or for a slot that holds another flow by now, is
    /// stopped at once.
    pub fn answered(&mut self, flow: FlowId, answer: Answer, now: u64) -> Vec<Action> {
        self.now = self.now.max(now);
        // No reply (or a socket error) is an unanswered request, asked again
        // quickly like one given up on (dstack #20: a transmitter whose NIC
        // restarted waited the full backoff, 10.11.2026); any answer ends
        // the quick retries.
        let replied = !matches!(answer, Answer::Failed);
        // A flow for channels on a multicast flow (dstack #297): its answer
        // touches no count of its transmitter's.
        let upgrade = self.flows[flow.slot]
            .as_ref()
            .is_some_and(|f| f.generation == flow.generation && f.upgrade)
            || self.abandoned.iter().any(|(id, _, u)| *id == flow && *u);
        for peer in self.peers.values_mut() {
            if peer.in_flight.contains(&flow) {
                peer.in_flight.retain(|f| *f != flow);
                if upgrade {
                    continue;
                }
                peer.unanswered = if replied {
                    0
                } else {
                    peer.unanswered.saturating_add(1)
                };
            }
        }
        let current = self.flows[flow.slot]
            .as_ref()
            .is_some_and(|f| f.generation == flow.generation && f.asked());
        if !current {
            let stale = self.abandoned.iter().position(|(id, _, _)| *id == flow);
            let to = stale.map(|at| self.abandoned.remove(at).1);
            return match (answer, to) {
                (Answer::Granted(handle), Some(to)) => vec![Action::Stop { to, handle }],
                _ => Vec::new(),
            };
        }
        if self.flows[flow.slot].as_ref().is_some_and(|f| f.upgrade) {
            self.upgrade_answered(flow.slot, answer, now);
            return Vec::new();
        }
        match answer {
            Answer::Granted(handle) => {
                if let Some(f) = self.flows[flow.slot].as_mut() {
                    f.handle = Some(handle);
                }
            }
            Answer::Refused(status::TOO_MANY_TX_FLOWS) => {
                self.fail(flow.slot, SubStatus::TooManyTxFlows, now)
            }
            Answer::Refused(status::FPP_REFUSED) => self.fpp_refused(flow.slot, now),
            Answer::Refused(_) | Answer::Failed => self.fail(flow.slot, SubStatus::TxFail, now),
        }
        Vec::new()
    }

    /// Shutdown: a stop for every flow we were granted; every flow and
    /// route dropped. A flow still asked for gets no keepalive, so it
    /// expires at its transmitter, and a grant of it that still comes is
    /// stopped ([`Manager::answered`]).
    pub fn stop_all(&mut self) -> Vec<Action> {
        let out = self
            .flows
            .iter()
            .flatten()
            .filter_map(|f| f.handle.map(|handle| Action::Stop { to: f.to, handle }))
            .collect();
        for slot in 0..MAX_FLOWS {
            self.abandon(slot);
            self.free(slot, self.now);
        }
        out
    }

    /// RX channel `rx`'s status; `None` when it is not subscribed. A route
    /// made for another subscription (the channel's changed since the last
    /// tick, which drops it) is none.
    pub fn status(&self, rx: usize) -> Option<SubStatus> {
        let sub = self.subs.get(rx)?.as_ref()?;
        let Some(route) = self.routes[rx].as_ref().filter(|r| r.sub == *sub) else {
            return Some(self.failed[rx].unwrap_or(SubStatus::Unresolved));
        };
        let flow = self.flows[route.flow.slot].as_ref()?;
        let fresh = flow
            .heard
            .is_some_and(|h| self.now.saturating_sub(h) < SILENT_NS);
        Some(match (fresh, flow.group, flow.handle) {
            (true, Some(_), _) => SubStatus::ReceivingMulticast,
            (true, None, Some(_)) => SubStatus::Receiving,
            _ => SubStatus::InProgress,
        })
    }

    /// The RX channels whose status changed since the last call (the
    /// channel-change notification, plan Task 6).
    pub fn changed(&mut self) -> Vec<usize> {
        let mut out = Vec::new();
        for rx in 0..self.reported.len() {
            let now = self.status(rx);
            if self.reported[rx] != now {
                self.reported[rx] = now;
                out.push(rx);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests;
