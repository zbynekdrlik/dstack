// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! Resolving the subscribed channels and requesting their flows.

use std::collections::BTreeMap;
use std::net::SocketAddrV4;

use tracing::{info, warn};

use super::resolve::{Advertised, Multicast, MulticastLookup, Resolver};
use super::{
    Action, FPP_STEP_MAX, Flow, FlowId, MAX_FLOWS, MAX_FPP_FLOORS, Manager, QUICK_RETRY_NS, Route,
    SubStatus, retry_after,
};
use crate::flows::control::FlowRequest;
use crate::media::packet::MEDIA_HEADER;
use crate::media::tx::{FPP_MAX, sendable};
use crate::wire::MAX_DATAGRAM;

impl Manager {
    /// The routed channels whose record changed (another address, latency,
    /// fpp or slots: a channel record that replaced an ARC read, or a
    /// transmitter that changed) leave their flow, which then ends and is
    /// asked for again from the new record. A record not found now changes
    /// nothing.
    pub(super) fn reresolve(&mut self, resolver: &dyn Resolver) {
        for rx in 0..self.routes.len() {
            let Some(route) = &self.routes[rx] else {
                continue;
            };
            let slot = route.flow.slot;
            let Some(f) = self.flows.get(slot).and_then(Option::as_ref) else {
                continue;
            };
            let Some(now) = resolver.lookup(&route.sub.tx_device, &route.sub.tx_channel) else {
                continue;
            };
            let shared = resolver.multicast(&route.sub.tx_device, &route.sub.tx_channel, &now);
            let moves = match (&shared, f.group) {
                // A multicast flow carries it now: it leaves its unicast flow
                // (a transmitter slot freed) for that one.
                (MulticastLookup::Found(m), None) => (fits(m, &now) && !self.coarser(m.fpp, &now))
                    .then_some("a multicast flow carries it now, it moves there"),
                // Its multicast flow is gone, or moved.
                (MulticastLookup::None, Some(_)) => Some("its multicast flow is gone"),
                (MulticastLookup::Found(m), Some(g)) if m.group != g => {
                    Some("its multicast flow moved to another group")
                }
                _ => None,
            };
            let why = match moves {
                Some(why) => why,
                None if same_source(&f.advert, &now) => continue,
                None => "its transmitter's record changed, its flow is asked for again",
            };
            info!(channel = rx + 1, "RX channel: {why}");
            self.routes[rx] = None;
            // A granted flow no channel uses is stopped by `close`; one
            // still asked for is given up here.
            let used = self.routes.iter().flatten().any(|r| r.flow.slot == slot);
            if !used && self.flows[slot].as_ref().is_some_and(Flow::asked) {
                self.abandon(slot);
                self.free(slot, self.now);
            }
        }
    }

    /// The subscribed channels without a flow: each routed to a flow that
    /// carries its TX channel already, or listed with the multicast flow
    /// that carries it, or listed under its transmitter for a unicast flow.
    /// A channel whose transmitter's flows are being read waits.
    #[allow(clippy::type_complexity)]
    pub(super) fn resolve(
        &mut self,
        resolver: &dyn Resolver,
    ) -> (
        BTreeMap<SocketAddrV4, Vec<(usize, Advertised)>>,
        Vec<(usize, Advertised, Multicast)>,
    ) {
        let mut pending: BTreeMap<SocketAddrV4, Vec<(usize, Advertised)>> = BTreeMap::new();
        let mut shared = Vec::new();
        for rx in 0..self.subs.len() {
            let Some(sub) = self.subs[rx].as_ref().filter(|_| self.routes[rx].is_none()) else {
                continue;
            };
            let Some(a) = resolver.lookup(&sub.tx_device, &sub.tx_channel) else {
                continue;
            };
            if a.rate != self.rate {
                // It would refuse (0x0301): never asked. Dante Controller
                // names it a format mismatch; said once per subscription.
                if self.failed[rx] != Some(SubStatus::ChannelFormat) {
                    warn!(
                        rx = rx + 1,
                        tx_channel = %sub.tx_channel,
                        tx_device = %sub.tx_device,
                        tx_rate = a.rate,
                        our_rate = self.rate,
                        "RX channel not received: its transmitter runs at another sample rate (a Dante flow needs one rate at both ends)"
                    );
                }
                self.failed[rx] = Some(SubStatus::ChannelFormat);
                continue;
            }
            if let Some((flow, slot)) = self.carrying(&a) {
                self.route(rx, flow, slot);
                continue;
            }
            match resolver.multicast(&sub.tx_device, &sub.tx_channel, &a) {
                MulticastLookup::Found(m) if fits(&m, &a) && !self.port_clash(&m, &a) => {
                    shared.push((rx, a, m))
                }
                MulticastLookup::Unknown => {}
                _ => pending.entry(a.control).or_default().push((rx, a)),
            }
        }
        (pending, shared)
    }

    /// Whether `m` shares its port with another group of `a`'s transmitter
    /// that one of our flows receives, where our socket cannot tell the two
    /// groups apart: on Windows a group's socket is bound to the port only,
    /// so it gets every group sent to that port, and the same source passes
    /// the RX filter (Linux binds the group itself). Such a flow is asked
    /// for as unicast instead.
    fn port_clash(&self, m: &Multicast, a: &Advertised) -> bool {
        self.port_only
            && self.flows.iter().flatten().any(|f| {
                f.to == a.control
                    && f.group
                        .is_some_and(|g| g.port() == m.group.port() && g != m.group)
            })
    }

    /// RX channel `rx` (its record `a`) receives the multicast flow `m`:
    /// routed to it when a flow of ours receives it already (another
    /// channel joined it this tick), else to a new flow in a free slot,
    /// which the RX thread joins. No request is sent. No free slot: it
    /// waits for one.
    pub(super) fn receive_multicast(
        &mut self,
        rx: usize,
        a: &Advertised,
        m: &Multicast,
        now: u64,
        packets: &[u64; MAX_FLOWS],
    ) {
        if let Some((flow, slot)) = self.carrying(a) {
            self.route(rx, flow, slot);
            return;
        }
        let Some(slot) = self.free_slot() else { return };
        self.generation = self.generation.wrapping_add(1);
        let id = FlowId {
            slot,
            generation: self.generation,
        };
        let slots: Vec<Option<u16>> = m.channels.iter().map(|&c| Some(c)).collect();
        let at = slots.iter().position(|&c| c == Some(a.tx_channel_id));
        info!(
            slot = slot + 1,
            channels = slots.len(),
            fpp = m.fpp,
            our_fpp = self.unicast_fpp(a).unwrap_or(0),
            group = %m.group,
            "RX multicast flow: a transmitter's multicast flow is received"
        );
        let name = self.subs[rx]
            .as_ref()
            .map_or_else(String::new, |s| s.tx_device.clone());
        self.flows[slot] = Some(Flow {
            generation: id.generation,
            to: a.control,
            slots,
            bits: m.bits,
            fpp: m.fpp,
            latency_ns: a.latency_ns.max(self.latency_ns),
            device: name.to_ascii_lowercase(),
            name,
            advertised_ns: a.latency_ns,
            advert: *a,
            since: now,
            handle: None,
            group: Some(m.group),
            heard: None,
            packets: packets[slot],
            upgrade: false,
        });
        if let Some(at) = at {
            self.route(rx, id, at);
        }
    }

    /// A flow from `a`'s transmitter that carries its TX channel, and the
    /// channel slot.
    fn carrying(&self, a: &Advertised) -> Option<(FlowId, usize)> {
        self.flows.iter().enumerate().find_map(|(slot, f)| {
            let f = f.as_ref().filter(|f| f.to == a.control && !f.upgrade)?;
            let at = f.slots.iter().position(|&s| s == Some(a.tx_channel_id))?;
            Some((
                FlowId {
                    slot,
                    generation: f.generation,
                },
                at,
            ))
        })
    }

    /// Routes RX channel `rx` to channel slot `slot` of `flow`.
    pub(super) fn route(&mut self, rx: usize, flow: FlowId, slot: usize) {
        if let Some(sub) = self.subs[rx].clone() {
            self.routes[rx] = Some(Route { flow, slot, sub });
            self.failed[rx] = None;
        }
    }

    /// Whether the transmitter at `ip` may be asked now.
    pub(super) fn may_request(&self, to: SocketAddrV4, now: u64) -> bool {
        self.peers.get(&to).is_none_or(|p| {
            let wait = if p.unanswered > 0 {
                QUICK_RETRY_NS
            } else {
                retry_after(p.failures)
            };
            p.in_flight.is_empty() && p.last_request.is_none_or(|t| now >= t.saturating_add(wait))
        })
    }

    /// One flow for the first channels `waiting` (one transmitter's, in RX
    /// order) needs, and its request; `None` when the transmitter cannot be
    /// asked (its records disagree, no frame count fits, no flow slot is
    /// free).
    pub(super) fn request(
        &mut self,
        waiting: &[(usize, Advertised)],
        now: u64,
        packets: &[u64; MAX_FLOWS],
        upgrade: bool,
    ) -> Option<Action> {
        let first = waiting.first()?.1;
        let format = |a: &Advertised| (a.nchan, a.bits, a.dbcp1);
        if waiting.iter().any(|(_, a)| format(a) != format(&first)) {
            return None;
        }
        let n = self.flow_slots(&first);
        let Some(fpp) = self.unicast_fpp(&first) else {
            for (rx, _) in waiting {
                self.failed[*rx] = Some(SubStatus::TxFail);
            }
            return None;
        };
        let slot = self.free_slot()?;
        // Each TX channel once, in RX order; the first n fill the slots, the
        // rest wait for the next flow.
        let mut ids: Vec<Option<u16>> = Vec::with_capacity(waiting.len());
        for (_, a) in waiting {
            if !ids.contains(&Some(a.tx_channel_id)) {
                ids.push(Some(a.tx_channel_id));
            }
        }
        ids.resize(n, None);
        let latency_ns = first.latency_ns.max(self.latency_ns);
        self.generation = self.generation.wrapping_add(1);
        let id = FlowId {
            slot,
            generation: self.generation,
        };
        let name = waiting
            .first()
            .and_then(|(rx, _)| self.subs.get(*rx)?.as_ref())
            .map_or_else(String::new, |s| s.tx_device.clone());
        self.flows[slot] = Some(Flow {
            generation: id.generation,
            to: first.control,
            slots: ids.clone(),
            bits: first.bits,
            fpp,
            latency_ns,
            device: name.to_ascii_lowercase(),
            name,
            advertised_ns: first.latency_ns,
            advert: first,
            since: now,
            handle: None,
            group: None,
            heard: None,
            packets: packets[slot],
            upgrade,
        });
        // A flow for channels on a multicast flow takes them once it
        // delivers ([`Manager::promote`]).
        for (rx, a) in waiting.iter().filter(|_| !upgrade) {
            if let Some(at) = ids.iter().position(|&s| s == Some(a.tx_channel_id)) {
                self.route(*rx, id, at);
            }
        }
        let peer = self.peers.entry(first.control).or_default();
        peer.in_flight.push(id);
        peer.last_request = Some(now);
        Some(Action::Request {
            flow: id,
            to: first.control,
            req: FlowRequest {
                rx_host: self.host.clone(),
                rx_flow_name: format!("{}_{}", slot + 1, self.process),
                sample_rate: self.rate,
                bits: first.bits,
                channels: ids,
                fpp,
                rx: SocketAddrV4::new(self.ip, self.ports[slot]),
            },
        })
    }

    /// The free flow slot freed longest ago (never used first, then the
    /// lowest).
    pub(super) fn free_slot(&self) -> Option<usize> {
        (0..self.slots)
            .filter(|&s| self.flows[s].is_none())
            .min_by_key(|&s| (self.freed[s], s))
    }

    /// The transmitter of flow slot `slot` refused its fpp
    /// ([`status::FPP_REFUSED`]): its next request asks twice the fpp, up to
    /// [`FPP_STEP_MAX`], and this refusal counts no failure (the next
    /// request comes after [`RETRY_NS`]); at [`FPP_STEP_MAX`] it is a
    /// failure as any refusal.
    pub(super) fn fpp_refused(&mut self, slot: usize, now: u64) {
        let Some(f) = &self.flows[slot] else { return };
        let (to, fpp) = (f.to, f.fpp);
        let next = fpp.saturating_mul(2).min(FPP_STEP_MAX);
        let known = self.fpp_floor.contains_key(&to);
        if next <= fpp || (!known && self.fpp_floor.len() >= MAX_FPP_FLOORS) {
            warn!(
                fpp,
                "RX flow refused (0x0314) at the largest fpp step: not asked larger"
            );
            self.fail(slot, SubStatus::TxFail, now);
            return;
        }
        info!(
            fpp,
            next, "RX flow refused (0x0314, its fpp): the next request asks a larger fpp"
        );
        self.fpp_floor.insert(to, next);
        for (route, failed) in self.routes.iter_mut().zip(self.failed.iter_mut()) {
            if route.as_ref().is_some_and(|r| r.flow.slot == slot) {
                *route = None;
                *failed = None;
            }
        }
        self.free(slot, now);
    }
}

/// Whether one of our flow slots can receive the multicast flow `m` for
/// the channel `a`: our sample format (its bits) and frames that fit one
/// datagram.
fn fits(m: &Multicast, a: &Advertised) -> bool {
    let bps = usize::try_from(a.bits / 8).unwrap_or(0);
    m.bits == a.bits && !m.channels.is_empty() && sendable(m.channels.len(), bps, m.fpp)
}

/// Whether two records of a transmitter's channels ask for the same flow:
/// the same flow control, latency, fpp range and slots (a flow carries
/// several TX channels, so the channel's id is not compared).
fn same_source(a: &Advertised, b: &Advertised) -> bool {
    (a.control, a.latency_ns, a.fpp_min, a.fpp_max, a.nchan)
        == (b.control, b.latency_ns, b.fpp_min, b.fpp_max, b.nchan)
}

/// The frames per packet to ask `a`'s transmitter for in a flow of `n`
/// slots: its most, at most [`FPP_MAX`], within one datagram; `None` when
/// that is below its least.
pub fn fpp_for(a: &Advertised, n: usize) -> Option<u16> {
    let bps = usize::try_from(a.bits / 8).ok()?;
    let fit = (MAX_DATAGRAM - MEDIA_HEADER) / n.checked_mul(bps).filter(|&b| b > 0)?;
    let fpp = usize::from(a.fpp_max.min(FPP_MAX)).min(fit);
    let fpp = u16::try_from(fpp).ok()?;
    (fpp >= a.fpp_min.max(1) && sendable(n, bps, fpp)).then_some(fpp)
}
