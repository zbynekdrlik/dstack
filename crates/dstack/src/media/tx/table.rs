// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The control side's TX flows ([`TxTable`], moved out of `tx.rs`, dstack
//! #360): at most [`MAX_FLOWS`] flows, each to the one receiver that asked
//! for it; `multicast.rs` adds the multicast ones.

use std::net::SocketAddrV4;

use super::{KEEPALIVE_TIMEOUT_NS, MAX_CHANNELS, MAX_FLOWS, Plan, sendable};
use crate::flows::control::{self, FlowRequest, Handle, status};

/// One TX flow, as the control side keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxFlow {
    /// The receiver: where the audio goes, and the only source of
    /// keepalives.
    pub dest: SocketAddrV4,
    /// The receiving device's name, as it asked (for the ARC lists).
    pub rx_host: String,
    /// The receiver's name of the flow.
    pub rx_flow_name: String,
    /// The slots: our TX channel number (from 1) or `None`.
    pub channels: Vec<Option<u16>>,
    pub fpp: u16,
    /// Bytes per sample: the request's bits / 8, as inferno takes them.
    pub bps: u8,
    pub cookie: u16,
    /// When a datagram from the receiver last kept the flow alive (the
    /// control side's ns).
    pub heard_ns: u64,
    /// No datagram for [`KEEPALIVE_TIMEOUT_NS`]: sends nothing.
    pub expired: bool,
    /// A multicast flow (ARC 2.8.9's create, dstack #193): `dest` is its
    /// group, it has no receiver (`rx_host` and `rx_flow_name` empty) and
    /// never expires; the flow server's handles do not reach it.
    pub multicast: bool,
}

/// The TX flows: the control side's table (pure; the times are the
/// control side's ns, [`TxShared::now_ns`](super::TxShared::now_ns)). A
/// clone is a copy to put back when a change cannot be kept (a multicast create the state file
/// could not save, `arc::answer`).
#[derive(Debug, Clone)]
pub struct TxTable {
    pub(super) flows: Vec<Option<TxFlow>>,
    /// The slots it fills: 0 to this (the device's `flow_slots`).
    pub(super) slots: usize,
    /// The next slot to try, mod `slots` (inferno's `flow_seq_id`).
    seq: u32,
    /// The cookies' generator state.
    rng: u64,
    /// The flow server's last warning per (requester, reason), ns: a
    /// refusal is warned once a minute each.
    pub warned: Vec<(std::net::Ipv4Addr, &'static str, u64)>,
}

impl TxTable {
    /// An empty table; `seed` starts the cookies (the daemon gives it the
    /// time, tests a constant).
    pub fn new(seed: u64) -> Self {
        Self::with_slots(seed, MAX_FLOWS)
    }

    /// An empty table that fills only slots 0 to `slots` (at most
    /// [`MAX_FLOWS`]).
    pub fn with_slots(seed: u64, slots: usize) -> Self {
        TxTable {
            flows: vec![None; MAX_FLOWS],
            slots: slots.min(MAX_FLOWS),
            seq: 0,
            rng: seed,
            warned: Vec::new(),
        }
    }

    /// A flow for `req` (checked by the server): a destination already
    /// served gets its channels, fpp and width set and keeps its handle
    /// (revived when expired; inferno keeps its fpp and width, a "TODO
    /// FIXME" there); a new one takes the next free slot after expired
    /// flows are freed, or [`status::TOO_MANY_TX_FLOWS`].
    pub fn add(&mut self, req: &FlowRequest, now: u64) -> Result<(usize, Handle), u16> {
        let bps = u8::try_from(req.bits / 8).unwrap_or(u8::MAX);
        if let Some(slot) = self.slot_of(req.rx)
            && let Some(flow) = self.flows[slot].as_mut()
        {
            flow.rx_host.clone_from(&req.rx_host);
            flow.rx_flow_name.clone_from(&req.rx_flow_name);
            flow.channels.clone_from(&req.channels);
            flow.fpp = req.fpp;
            flow.bps = bps;
            flow.heard_ns = now;
            flow.expired = false;
            return Ok((slot, control::handle(slot as u32, flow.cookie)));
        }
        self.free_expired();
        for _ in 0..self.slots {
            let slot = self.seq as usize % self.slots;
            self.seq = self.seq.wrapping_add(1);
            if self.flows[slot].is_none() {
                let cookie = self.cookie();
                self.flows[slot] = Some(TxFlow {
                    dest: req.rx,
                    rx_host: req.rx_host.clone(),
                    rx_flow_name: req.rx_flow_name.clone(),
                    channels: req.channels.clone(),
                    fpp: req.fpp,
                    bps,
                    cookie,
                    heard_ns: now,
                    expired: false,
                    multicast: false,
                });
                return Ok((slot, control::handle(slot as u32, cookie)));
            }
        }
        Err(status::TOO_MANY_TX_FLOWS)
    }

    /// The slot of the flow to `dest`, expired or not.
    pub fn slot_of(&self, dest: SocketAddrV4) -> Option<usize> {
        self.flows
            .iter()
            .position(|f| f.as_ref().is_some_and(|f| f.dest == dest))
    }

    /// The flow with handle `h`, expired or not; never a multicast flow,
    /// which no receiver asked for (no receiver's handle stops or updates
    /// it).
    pub fn get(&self, h: &Handle) -> Option<(usize, &TxFlow)> {
        let (slot, cookie) = control::split(h);
        let slot = usize::try_from(slot).ok()?;
        let flow = self.flows.get(slot)?.as_ref()?;
        (flow.cookie == cookie && !flow.multicast).then_some((slot, flow))
    }

    /// Removes the flow with handle `h` (expired or not); returns its slot,
    /// or [`status::FLOW_NOT_FOUND`].
    pub fn stop(&mut self, h: &Handle) -> Result<usize, u16> {
        let (slot, _) = self.get(h).ok_or(status::FLOW_NOT_FOUND)?;
        self.flows[slot] = None;
        Ok(slot)
    }

    /// Sets the channels of the flow with handle `h` (checked by the
    /// server) and keeps it alive, an expired flow revived (inferno);
    /// returns its slot, or [`status::FLOW_NOT_FOUND`].
    pub fn update(&mut self, h: &Handle, channels: &[Option<u16>], now: u64) -> Result<usize, u16> {
        let (slot, _) = self.get(h).ok_or(status::FLOW_NOT_FOUND)?;
        if let Some(flow) = self.flows[slot].as_mut() {
            flow.channels = channels.to_vec();
            flow.heard_ns = now;
            flow.expired = false;
        }
        Ok(slot)
    }

    /// A datagram from the receiver of `slot`'s flow: alive until
    /// [`KEEPALIVE_TIMEOUT_NS`] after `now`. An expired flow stays expired
    /// (inferno reads nothing for it).
    pub fn keepalive(&mut self, slot: usize, now: u64) {
        if let Some(flow) = self
            .flows
            .get_mut(slot)
            .and_then(Option::as_mut)
            .filter(|f| !f.expired)
        {
            flow.heard_ns = now;
        }
    }

    /// Expires every flow not heard from for [`KEEPALIVE_TIMEOUT_NS`];
    /// returns the slots it expired now, one bit each.
    pub fn expire(&mut self, now: u64) -> u32 {
        let mut expired = 0u32;
        for (slot, flow) in self.flows.iter_mut().enumerate() {
            if let Some(flow) = flow.as_mut().filter(|f| {
                !f.expired && !f.multicast && now.saturating_sub(f.heard_ns) >= KEEPALIVE_TIMEOUT_NS
            }) {
                flow.expired = true;
                expired += 1 << slot;
            }
        }
        expired
    }

    /// Ends the live flow in `slot` as if its receiver had gone quiet
    /// (dstack #187, dstack-dev's `tx-end`): it is expired as [`expire`]
    /// expires one, so it sends nothing, its slot is free for the next new
    /// flow, and its receiver's stop is still answered. `false` for an
    /// empty slot or a flow expired already.
    ///
    /// [`expire`]: TxTable::expire
    pub fn end(&mut self, slot: usize) -> bool {
        match self
            .flows
            .get_mut(slot)
            .and_then(Option::as_mut)
            .filter(|f| !f.expired)
        {
            Some(flow) => {
                flow.expired = true;
                true
            }
            None => false,
        }
    }

    /// The flow in `slot`.
    pub fn flow(&self, slot: usize) -> Option<&TxFlow> {
        self.flows.get(slot)?.as_ref()
    }

    /// Every flow with its slot, expired ones too.
    pub fn flows(&self) -> impl Iterator<Item = (usize, &TxFlow)> {
        self.flows
            .iter()
            .enumerate()
            .filter_map(|(slot, f)| Some((slot, f.as_ref()?)))
    }

    /// What the TX thread sends for `slot`: nothing for an empty slot, an
    /// expired flow or one that cannot be sent.
    pub fn plan(&self, slot: usize) -> Plan {
        match self.flow(slot) {
            Some(f) if !f.expired && sendable(f.channels.len(), usize::from(f.bps), f.fpp) => {
                let mut channels = [0u16; MAX_CHANNELS];
                for (c, id) in channels.iter_mut().zip(&f.channels) {
                    *c = id.unwrap_or(0);
                }
                Plan {
                    dest: Some(f.dest),
                    fpp: f.fpp,
                    bps: f.bps,
                    nch: f.channels.len() as u8,
                    channels,
                }
            }
            _ => Plan::default(),
        }
    }

    fn free_expired(&mut self) {
        for flow in &mut self.flows {
            if flow.as_ref().is_some_and(|f| f.expired) {
                *flow = None;
            }
        }
    }

    /// The next cookie (SplitMix64; any 16 bits will do, inferno's are
    /// random).
    pub(super) fn cookie(&mut self) -> u16 {
        self.rng = self.rng.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        (z >> 48) as u16
    }
}
