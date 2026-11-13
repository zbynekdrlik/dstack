// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The control side's hand-over to the RX thread ([`RxShared::publish`]):
//! the flow slots' plans and the RX channels' routes, and the restart of
//! the rings a new flow feeds (dstack #234).

use std::sync::PoisonError;
use std::sync::atomic::Ordering;

use super::{MAX_FLOWS, RxPlan, RxRoute, RxShared, new_flow};

impl RxShared {
    /// Hands the flow slots' plans and the RX channels' routes to the RX
    /// thread (what changed). Routes that leave a flow go first and routes
    /// that join one last, so no channel ever reads another flow's samples.
    pub fn publish(
        &self,
        plan: impl Fn(usize) -> RxPlan,
        route: impl Fn(usize) -> Option<RxRoute>,
    ) {
        let _writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        let mut changed = false;
        for (rx, cell) in self.routes.iter().enumerate() {
            let have = cell.load(Ordering::Relaxed);
            if have != 0 && have != RxRoute::word(route(rx)) {
                cell.store(0, Ordering::Release);
                changed = true;
            }
        }
        let mut delays = [0u16; MAX_FLOWS];
        // dstack #234: the slots whose flow is new; their rings restart.
        let mut fresh = [false; MAX_FLOWS];
        for (slot, cell) in self.plans.iter().enumerate() {
            let p = plan(slot);
            delays[slot] = p.delay;
            let w = p.words();
            let had = cell.peek();
            if had != w {
                fresh[slot] = new_flow(RxPlan::from_words(had), p);
                cell.write(w);
            }
        }
        for (rx, cell) in self.delays.iter().enumerate() {
            let delay = route(rx).and_then(|r| delays.get(r.flow).copied());
            cell.store(u32::from(delay.unwrap_or(0)), Ordering::Relaxed);
        }
        for (rx, cell) in self.routes.iter().enumerate() {
            let r = route(rx);
            let want = RxRoute::word(r);
            let joins = want != cell.load(Ordering::Relaxed);
            // Before the route is stored, so the pump never reads the new
            // flow's ring unrestarted. The RX thread restarts it again at
            // its next refresh (`Receiver::refresh`): until then it may
            // still write the old flow's packets there.
            if let Some(r) = r
                && (joins || fresh.get(r.flow) == Some(&true))
                && let Some(ring) = self.rings.get(rx)
            {
                ring.restart();
            }
            if joins {
                cell.store(want, Ordering::Release);
                changed = true;
            }
        }
        if changed {
            self.routes_version.fetch_add(1, Ordering::Release);
        }
    }
}
