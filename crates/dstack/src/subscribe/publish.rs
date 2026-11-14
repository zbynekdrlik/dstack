// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! What the manager publishes for the RX thread: the plan of each flow
//! slot, the route of each RX channel, and the flows' views.

use super::{Flow, FlowId, FlowView, Manager};
use crate::clock::latency_samples;
use crate::media::rx::{RxPlan, RxRoute};

impl Manager {
    /// What the RX thread receives in flow slot `slot`.
    pub fn rx_plan(&self, slot: usize) -> RxPlan {
        let Some(Some(f)) = self.flows.get(slot) else {
            return RxPlan::default();
        };
        let (playout, delay) = self.read_at(f);
        RxPlan {
            src: Some(*f.to.ip()),
            bps: (f.bits / 8) as u8,
            nslots: f.slots.len() as u8,
            latency: u32::try_from(playout).unwrap_or(u32::MAX),
            delay: u16::try_from(delay).unwrap_or(u16::MAX),
            block: u16::try_from(self.block).unwrap_or(u16::MAX),
            live: f.handle.is_some(),
            generation: f.generation,
            group: f.group,
        }
    }

    /// A flow's playout point, samples: its latency (its transmitter's or
    /// ours, the larger) and at least its frames per packet. A packet leaves
    /// its transmitter after its last frame; the pump's block in hand covers
    /// the network for packets that align with blocks (iemmixer's box,
    /// 31.10.2026: fpp 32 played out at 31 with a block of 16, no hole).
    /// Only that flow's channels are read later; the others keep ours.
    fn playout(&self, f: &Flow) -> u64 {
        let fpp = u64::from(f.fpp);
        match self.playout_override.get(&f.device) {
            Some(&o) => u64::from(o)
                .max(fpp)
                .max(latency_samples(self.latency_ns, self.rate)),
            None => latency_samples(f.latency_ns, self.rate).max(fpp),
        }
    }

    /// A flow's playout point as the pump reads it, and its delay past
    /// ours, samples. The pump reads whole blocks: a delay below a block
    /// does not move a packet's first frame's read (iemmixer's box,
    /// 31.10.2026: a delay of 8 at a block of 16 played as 0, with holes;
    /// 16 played clean). So the delay is rounded up to whole blocks.
    fn read_at(&self, f: &Flow) -> (u64, u64) {
        let device = latency_samples(self.latency_ns, self.rate);
        let block = self.block.max(1);
        let delay = self
            .playout(f)
            .saturating_sub(device)
            .div_ceil(block)
            .saturating_mul(block);
        (device.saturating_add(delay), delay)
    }

    /// Where RX channel `rx`'s samples come from.
    pub fn rx_route(&self, rx: usize) -> Option<RxRoute> {
        let r = self.routes.get(rx)?.as_ref()?;
        Some(RxRoute {
            flow: r.flow.slot,
            slot: r.slot,
        })
    }

    /// The flow in flow slot `slot`.
    pub fn flow(&self, slot: usize) -> Option<FlowView> {
        let f = self.flows.get(slot)?.as_ref()?;
        let mut rx = vec![Vec::new(); f.slots.len()];
        for (channel, r) in self.routes.iter().enumerate() {
            if let Some(r) = r.as_ref().filter(|r| r.flow.slot == slot) {
                rx[r.slot].push(channel);
            }
        }
        Some(FlowView {
            id: FlowId {
                slot,
                generation: f.generation,
            },
            port: self.ports[slot],
            to: f.to,
            handle: f.handle,
            group: f.group,
            channels: f.slots.clone(),
            rx,
            bits: f.bits,
            fpp: f.fpp,
            latency_ns: f.latency_ns,
            advertised_ns: f.advertised_ns,
            below_advertised: self.read_at(f).0 < latency_samples(f.advertised_ns, self.rate),
            device: f.name.clone(),
        })
    }

    /// The transmitter's Dante device name of the flow in flow slot `slot`
    /// (dstack #266), as [`FlowView::device`]; `None` for an empty slot.
    pub fn flow_device(&self, slot: usize) -> Option<&str> {
        Some(self.flows.get(slot)?.as_ref()?.name.as_str())
    }
}
