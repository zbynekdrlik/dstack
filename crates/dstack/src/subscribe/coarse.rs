// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! Our own unicast flow over a coarser multicast flow (dstack #297).
//!
//! A transmitter's multicast flow costs a receiver its frames a packet in
//! packet time before the first sample can leave (32 frames at 96 kHz:
//! 0.33 ms). Only a device whose config sets `arc_rx_fpp` opts in (the
//! main's decision on #297): a device on the default, the band's FOH/PA
//! feed among them, sees no change. A channel whose multicast flow has more frames a packet than
//! our own unicast flow from that transmitter would carry ([`fpp_for`], the
//! transmitter's fpp step) asks for that unicast flow. The owner's rules
//! (14.11.2026: the multicast flow feeds the PA too, "extrémne opatrne"):
//!
//! - the transmitter's multicast flow is never created, changed or ended
//!   here: the only datagrams are our own unicast flow's request and stop;
//! - the request takes only a free TX flow slot, as the transmitter's flow
//!   list states; no free slot, or no slot count, asks nothing;
//! - a refusal, no answer or a unicast flow that delivers nothing keeps the
//!   channels on the multicast flow, with no other side effect (no failure
//!   counted, no status changed), and that transmitter is not asked again
//!   while the manager lives: no loop;
//! - make-before-break: the channels stay on the multicast flow until the
//!   unicast flow delivers, then move; the multicast flow, unused, is left.

use std::net::SocketAddrV4;

use tracing::info;

use super::resolve::{Advertised, Multicast, MulticastLookup, Resolver};
use super::{Action, Answer, FlowId, MAX_CHANNELS, MAX_FLOWS, MAX_FPP_FLOORS, Manager, fpp_for};

impl Manager {
    /// The channel slots of a unicast flow from `a`'s transmitter: its
    /// record's, at most [`MAX_CHANNELS`] and our RX channels.
    pub(super) fn flow_slots(&self, a: &Advertised) -> usize {
        usize::from(a.nchan).min(MAX_CHANNELS).min(self.subs.len())
    }

    /// The frames per packet our unicast flow from `a`'s transmitter asks:
    /// [`fpp_for`], raised to the step a transmitter that refused a smaller
    /// fpp (0x0314) gets; `None` when no frame count fits. A step always
    /// fits a datagram (the `const` assertion at `FPP_STEP_MAX`).
    pub(super) fn unicast_fpp(&self, a: &Advertised) -> Option<u16> {
        let fpp = fpp_for(a, self.flow_slots(a))?;
        Some(
            self.fpp_floor
                .get(&a.control)
                .map_or(fpp, |&floor| floor.max(fpp)),
        )
    }

    /// Whether the multicast flow `m` has more frames a packet than our own
    /// unicast flow for `a` would carry, on a device that opts in (its
    /// config sets `arc_rx_fpp`); never on one that does not.
    pub(super) fn coarser(&self, m: &Multicast, a: &Advertised) -> bool {
        self.own_unicast && self.unicast_fpp(a).is_some_and(|ours| m.fpp > ours)
    }

    /// Whether the channels of the transmitter at `to` stay on its
    /// multicast flow; a full list keeps every transmitter's.
    fn kept(&self, to: SocketAddrV4) -> bool {
        self.multicast_kept.contains(&to) || self.multicast_kept.len() >= MAX_FPP_FLOORS
    }

    /// The channels of the transmitter at `to` stay on its multicast flow
    /// (fpp `multicast`, ours `ours`), for `why`; said once.
    fn keep_multicast(&mut self, to: SocketAddrV4, why: &str, multicast: u16, ours: u16) {
        if self.kept(to) {
            return;
        }
        self.multicast_kept.insert(to);
        info!(
            multicast_fpp = multicast,
            our_fpp = ours,
            "RX channels stay on their transmitter's multicast flow: our own unicast flow {why}"
        );
    }

    /// The flow for channels on a multicast flow in `slot` came to nothing
    /// (`why`): it is freed, its channels stay where they are.
    pub(super) fn upgrade_failed(&mut self, slot: usize, why: &str, now: u64) {
        let Some(f) = &self.flows[slot] else { return };
        let (to, ours) = (f.to, f.fpp);
        let multicast = self
            .flows
            .iter()
            .flatten()
            .find(|g| g.to == to && g.group.is_some())
            .map_or(0, |g| g.fpp);
        self.keep_multicast(to, why, multicast, ours);
        self.free(slot, now);
    }

    /// The answer to the request of a flow for channels on a multicast flow
    /// in `slot`: a grant is kept (its channels move once it delivers),
    /// anything else keeps them on the multicast flow.
    pub(super) fn upgrade_answered(&mut self, slot: usize, answer: Answer, now: u64) {
        let why = match answer {
            Answer::Granted(handle) => {
                if let Some(f) = self.flows[slot].as_mut() {
                    f.handle = Some(handle);
                }
                return;
            }
            Answer::Refused(_) => "was refused",
            Answer::Failed => "got no answer",
        };
        self.upgrade_failed(slot, why, now);
    }

    /// Each granted flow for channels on a multicast flow that delivers
    /// now takes those channels: every channel routed to a multicast flow
    /// of its transmitter whose TX channel it carries. The multicast flow,
    /// unused then, is left by [`Manager::close`].
    pub(super) fn promote(&mut self) {
        for slot in 0..MAX_FLOWS {
            let Some(f) = self.flows[slot]
                .as_ref()
                .filter(|f| f.upgrade && f.handle.is_some() && f.heard.is_some())
            else {
                continue;
            };
            let (to, generation, ids, fpp) = (f.to, f.generation, f.slots.clone(), f.fpp);
            let mut moved = 0;
            for rx in 0..self.routes.len() {
                let Some(r) = &self.routes[rx] else { continue };
                let Some(from) = self.flows[r.flow.slot]
                    .as_ref()
                    .filter(|g| g.to == to && g.group.is_some())
                else {
                    continue;
                };
                let id = from.slots.get(r.slot).copied().flatten();
                if let Some(at) = ids.iter().position(|&s| s.is_some() && s == id) {
                    self.route(rx, FlowId { slot, generation }, at);
                    moved += 1;
                }
            }
            if let Some(f) = self.flows[slot].as_mut() {
                f.upgrade = false;
            }
            info!(
                slot = slot + 1,
                fpp,
                channels = moved,
                "RX unicast flow delivers: its channels leave the multicast flow"
            );
        }
    }

    /// Asks, for each transmitter that may be asked now, one unicast flow
    /// for its channels that receive a coarser multicast flow of it, while
    /// its flow list shows a free TX flow slot, once that multicast flow
    /// delivers. The channels stay routed to the multicast flow.
    pub(super) fn upgrade(
        &mut self,
        resolver: &dyn Resolver,
        now: u64,
        packets: &[u64; MAX_FLOWS],
        out: &mut Vec<Action>,
    ) {
        let mut waiting: std::collections::BTreeMap<SocketAddrV4, Vec<(usize, Advertised)>> =
            std::collections::BTreeMap::new();
        let mut shown: Vec<(SocketAddrV4, Multicast)> = Vec::new();
        for rx in 0..self.routes.len() {
            let Some(r) = &self.routes[rx] else { continue };
            // A multicast flow that delivers: our audio never waits on it.
            let Some(g) = self.flows[r.flow.slot]
                .as_ref()
                .filter(|g| g.group.is_some() && g.heard.is_some())
            else {
                continue;
            };
            let (to, group) = (g.to, g.group);
            let sub = r.sub.clone();
            let Some(a) = resolver.lookup(&sub.tx_device, &sub.tx_channel) else {
                continue;
            };
            let asking = self.flows.iter().flatten().any(|f| f.upgrade && f.to == to);
            if a.control != to || a.rate != self.rate || asking || self.kept(to) {
                continue;
            }
            let MulticastLookup::Found(m) = resolver.multicast(&sub.tx_device, &sub.tx_channel, &a)
            else {
                continue;
            };
            if Some(m.group) != group || !self.coarser(&m, &a) {
                continue;
            }
            shown.push((to, m));
            waiting.entry(to).or_default().push((rx, a));
        }
        for (to, list) in waiting {
            let Some((_, m)) = shown.iter().find(|(t, _)| *t == to) else {
                continue;
            };
            let (multicast, free) = (m.fpp, m.free_slots);
            let ours = self.unicast_fpp(&list[0].1).unwrap_or(0);
            match free {
                None => {
                    self.keep_multicast(
                        to,
                        "is not asked: the transmitter's free TX flow slots are not known",
                        multicast,
                        ours,
                    );
                    continue;
                }
                Some(0) => {
                    self.keep_multicast(
                        to,
                        "is not asked: the transmitter has no free TX flow slot",
                        multicast,
                        ours,
                    );
                    continue;
                }
                Some(_) => {}
            }
            if !self.may_request(to, now) {
                continue;
            }
            let Some(action) = self.request(&list, now, packets, true) else {
                continue;
            };
            let device = self.subs[list[0].0]
                .as_ref()
                .map_or_else(String::new, |s| s.tx_device.clone());
            let rx: Vec<usize> = list.iter().map(|(rx, _)| rx + 1).collect();
            info!(
                tx_device = %device,
                rx = ?rx,
                multicast_fpp = multicast,
                our_fpp = ours,
                free_slots = free.unwrap_or(0),
                "RX multicast flow has more frames a packet than ours: our own unicast flow is asked for, the multicast flow kept until it delivers"
            );
            out.push(action);
        }
    }
}
