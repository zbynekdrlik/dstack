// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The flow slots' bookkeeping: what is heard, given up, closed, failed,
//! freed and forgotten.

use tracing::info;

use super::{
    Action, FlowId, GIVE_UP_NS, MAX_FLOWS, Manager, RETRY_NS, SILENT_NS, SubStatus, YOUNG_NS,
};

impl Manager {
    /// Flows whose slot received packets since the last look heard them
    /// now; audio from a granted flow ends its transmitter's failures.
    pub(super) fn hear(&mut self, now: u64, packets: &[u64; MAX_FLOWS]) {
        for (slot, flow) in self.flows.iter_mut().enumerate() {
            let Some(f) = flow.as_mut() else { continue };
            if packets[slot] != f.packets {
                f.packets = packets[slot];
                f.heard = Some(now);
                if f.handle.is_some()
                    && let Some(peer) = self.peers.get_mut(&f.to)
                {
                    peer.failures = 0;
                    peer.unanswered = 0;
                }
            }
        }
    }

    /// Requests without an answer for [`GIVE_UP_NS`] have failed.
    pub(super) fn give_up(&mut self, now: u64) {
        for slot in 0..MAX_FLOWS {
            let Some(f) = &self.flows[slot] else { continue };
            if f.asked() && now.saturating_sub(f.since) >= GIVE_UP_NS {
                let to = f.to;
                if f.upgrade {
                    self.abandon(slot);
                    self.upgrade_failed(slot, "its request got no answer", now);
                    continue;
                }
                self.abandon(slot);
                self.fail(slot, SubStatus::TxFail, now);
                if let Some(peer) = self.peers.get_mut(&to) {
                    peer.unanswered = peer.unanswered.saturating_add(1);
                }
            }
        }
    }

    /// Flow slot `slot`'s request is no longer waited for: a grant that
    /// still comes is stopped ([`Manager::answered`]); its transmitter may
    /// be asked again. The last [`MAX_FLOWS`] are kept.
    pub(super) fn abandon(&mut self, slot: usize) {
        let Some(f) = self.flows[slot].as_ref().filter(|f| f.asked()) else {
            return;
        };
        let id = FlowId {
            slot,
            generation: f.generation,
        };
        if self.abandoned.len() >= MAX_FLOWS {
            self.abandoned.remove(0);
        }
        self.abandoned.push((id, f.to, f.upgrade));
        if let Some(peer) = self.peers.get_mut(&f.to) {
            peer.in_flight.retain(|f| *f != id);
        }
    }

    /// Stops the granted flows no channel uses, and those silent once old
    /// enough (a failure: their channels are resolved again).
    pub(super) fn close(&mut self, now: u64, out: &mut Vec<Action>) {
        for slot in 0..MAX_FLOWS {
            let Some(f) = &self.flows[slot] else { continue };
            if f.asked() {
                continue;
            }
            let used = f.upgrade || self.routes.iter().flatten().any(|r| r.flow.slot == slot);
            let silent = f.heard.is_none_or(|h| now.saturating_sub(h) >= SILENT_NS)
                && now.saturating_sub(f.since) >= YOUNG_NS;
            if used && !silent {
                continue;
            }
            if f.upgrade {
                // Granted, never delivered: stopped, its channels stay on
                // the multicast flow.
                if let Some(handle) = f.handle {
                    out.push(Action::Stop { to: f.to, handle });
                }
                self.upgrade_failed(slot, "it delivered nothing", now);
                continue;
            }
            // A multicast flow is left (its socket closed), never stopped:
            // it is the transmitter's, for every receiver.
            let Some(handle) = f.handle else {
                if used {
                    info!(
                        slot = slot + 1,
                        "RX multicast flow silent: its channels are resolved again"
                    );
                }
                self.free(slot, now);
                continue;
            };
            out.push(Action::Stop { to: f.to, handle });
            if used && let Some(peer) = self.peers.get_mut(&f.to) {
                peer.count_failure();
            }
            self.free(slot, now);
        }
    }

    /// The request of flow slot `slot` failed with `why`: its channels
    /// leave it (and show `why`), its transmitter counts a failure.
    pub(super) fn fail(&mut self, slot: usize, why: SubStatus, now: u64) {
        let Some(f) = &self.flows[slot] else { return };
        let to = f.to;
        for (route, failed) in self.routes.iter_mut().zip(self.failed.iter_mut()) {
            if route.as_ref().is_some_and(|r| r.flow.slot == slot) {
                *route = None;
                *failed = Some(why);
            }
        }
        self.peers.entry(to).or_default().count_failure();
        self.free(slot, now);
    }

    /// Flow slot `slot` holds no flow from `now`; its channels have none.
    pub(super) fn free(&mut self, slot: usize, now: u64) {
        if self.flows[slot].take().is_some() {
            self.freed[slot] = Some(now);
        }
        for route in &mut self.routes {
            if route.as_ref().is_some_and(|r| r.flow.slot == slot) {
                *route = None;
            }
        }
    }

    /// Forgets transmitters that have nothing pending: no flow (a request
    /// in flight is one), no failure, and their wait over.
    pub(super) fn prune(&mut self, now: u64) {
        let flows = &self.flows;
        self.peers.retain(|to, p| {
            flows.iter().flatten().any(|f| f.to == *to)
                || p.failures > 0
                || p.last_request
                    .is_some_and(|t| now < t.saturating_add(RETRY_NS))
        });
    }
}
