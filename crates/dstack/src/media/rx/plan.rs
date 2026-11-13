// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.
// Protocol facts (byte layouts, opcodes, behaviour) checked against
// inferno (https://github.com/teodly/inferno) as a reference; the code is
// our own.

//! What the control side hands the RX thread: each flow slot's [`RxPlan`]
//! and each RX channel's [`RxRoute`], as the words of their seqlocked
//! atomics ([`super::RxShared::publish`]).

use std::net::{Ipv4Addr, SocketAddrV4};

/// What the RX thread receives in one flow slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RxPlan {
    /// The transmitter: only its datagrams are this flow's. `None`: no
    /// flow.
    pub src: Option<Ipv4Addr>,
    /// Bytes per sample.
    pub bps: u8,
    /// Channel slots per frame.
    pub nslots: u8,
    /// The flow's playout point behind media now, samples (P_flow: its
    /// latency, at least its frames per packet; at most `u16::MAX`).
    pub latency: u32,
    /// How much later than the device latency this flow's channels are
    /// read, samples (P_flow − P; at most `u16::MAX`).
    pub delay: u16,
    /// The pump's block, samples: a packet counts late once its first
    /// frame is later than `latency + block`, the read budget of a block
    /// whose frames a packet fills (at most 511).
    pub block: u16,
    /// Granted by its transmitter: keepalives go out.
    pub live: bool,
    /// Which flow (a new flow in the slot is a new plan).
    pub generation: u32,
    /// A multicast flow's group: the slot receives it on a socket of its
    /// own that joins the group on our address (dstack #148), and sends no
    /// keepalive. `None`: the slot's own socket, where a unicast flow comes.
    pub group: Option<SocketAddrV4>,
}

impl RxPlan {
    /// The plan as three words (the seqlocked cell's).
    pub fn words(&self) -> [u64; 3] {
        let Some(src) = self.src else {
            return [0; 3];
        };
        [
            (u64::from(src.to_bits()) << 32)
                + (u64::from(self.delay) << 16)
                + u64::from(self.latency.min(u32::from(u16::MAX))),
            (1 << 63)
                + (u64::from(self.live) << 62)
                + (u64::from(self.block & 0x1ff) << 48)
                + (u64::from(self.generation) << 16)
                + (u64::from(self.bps) << 8)
                + u64::from(self.nslots),
            self.group.map_or(0, |g| {
                (1 << 63) + (u64::from(g.ip().to_bits()) << 16) + u64::from(g.port())
            }),
        ]
    }

    /// The plan [`RxPlan::words`] wrote.
    pub fn from_words(w: [u64; 3]) -> RxPlan {
        if w[1] >> 63 == 0 {
            return RxPlan::default();
        }
        RxPlan {
            src: Some(Ipv4Addr::from_bits((w[0] >> 32) as u32)),
            bps: (w[1] >> 8) as u8,
            nslots: w[1] as u8,
            latency: u32::from(w[0] as u16),
            delay: (w[0] >> 16) as u16,
            block: ((w[1] >> 48) & 0x1ff) as u16,
            live: (w[1] >> 62) & 1 == 1,
            generation: (w[1] >> 16) as u32,
            group: (w[2] >> 63 == 1)
                .then(|| SocketAddrV4::new(Ipv4Addr::from_bits((w[2] >> 16) as u32), w[2] as u16)),
        }
    }
}

/// Where an RX channel's samples come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RxRoute {
    /// The flow slot.
    pub flow: usize,
    /// The channel slot in its frames.
    pub slot: usize,
}

impl RxRoute {
    /// A route as one word, 0 for none.
    pub fn word(route: Option<RxRoute>) -> u64 {
        route.map_or(0, |r| {
            (1 << 63) + ((r.flow as u64 & 0xff) << 8) + (r.slot as u64 & 0xff)
        })
    }

    /// The route [`RxRoute::word`] wrote.
    pub fn from_word(w: u64) -> Option<RxRoute> {
        (w >> 63 == 1).then_some(RxRoute {
            flow: ((w >> 8) & 0xff) as usize,
            slot: (w & 0xff) as usize,
        })
    }
}

/// Whether plan `now` is another flow than `had` in its slot (another
/// transmitter or another flow of the manager's, its generation): its RX
/// channels' rings restart (dstack #234), so the holes read before its
/// first packet are start holes. A plan change of the same flow (granted,
/// a latency) is none.
pub(crate) fn new_flow(had: RxPlan, now: RxPlan) -> bool {
    now.src.is_some() && (had.src != now.src || had.generation != now.generation)
}
