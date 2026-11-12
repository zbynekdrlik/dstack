// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.
// Protocol facts (byte layouts, opcodes, behaviour) checked against
// inferno (https://github.com/teodly/inferno) as a reference; the code is
// our own.

//! The paged channel lists (dstack #231): the page writer ([`page`],
//! [`paged`], inferno's `serialize_items`) with its [`Heap`] and the
//! channels' shared descriptor ([`common_descriptor`]), and the lists
//! built on it: 0x2000 ([`tx_channels`]), 0x2010 ([`tx_names`], under
//! 2.8.9 too) and 0x3000 ([`rx_channels`], with an RX channel's
//! subscription as listed, [`listed_subscription`]). [`granted_flow`] is
//! the RX flow lists' grant read (0x3200 in `arc`, 0x3600 in `v289`). The
//! flow lists (0x2200, 0x3200) and ARC 2.8.9's lists (`v289`) page through
//! it too.

use super::{
    ArcOutcome, PCM_TYPE, RX_CHANNEL_FLAGS, RX_PAGE, SOFT_LIMIT, TX_PAGE, first_item, refuse,
    wire_u16,
};
use crate::device::{DeviceConfig, DeviceState, Subscription};
use crate::subscribe::{Manager, SubStatus};
use crate::wire::{Body, Header, STATUS_MORE, STATUS_OK, reply};

/// The 16-byte descriptor of a page's channels.
const COMMON_DESCRIPTOR: usize = 16;

/// The descriptor every channel of a page points at: rate, encoding, PCM
/// type.
pub(super) fn common_descriptor(out: &mut Body, cfg: &DeviceConfig) -> u16 {
    let at = out.offset();
    let bits = u16::from(cfg.encoding);
    out.u32(cfg.sample_rate);
    out.u8(1);
    out.u8(1);
    out.u16(bits);
    out.u16(0x400);
    out.u16(bits);
    out.u16(bits);
    out.u16(u16::from(PCM_TYPE));
    debug_assert_eq!(usize::from(out.offset() - at), COMMON_DESCRIPTOR);
    at
}

/// What a page's items write behind the descriptor table: their strings
/// and descriptors, as packet offsets that are final when written (the
/// table's room is reserved first), and one descriptor every item may
/// share, written once, where the first item that asks for it stands.
pub(super) struct Heap {
    body: Body,
    shared: Option<u16>,
}

impl Heap {
    /// The shared descriptor's offset, `write` writing it the first time.
    pub(super) fn shared(&mut self, write: impl FnOnce(&mut Body) -> u16) -> u16 {
        match self.shared {
            Some(at) => at,
            None => *self.shared.insert(write(&mut self.body)),
        }
    }
}

impl std::ops::Deref for Heap {
    type Target = Body;
    fn deref(&self) -> &Body {
        &self.body
    }
}

impl std::ops::DerefMut for Heap {
    fn deref_mut(&mut self) -> &mut Body {
        &mut self.body
    }
}

/// One page of a list: `[slots, count]`, a table of `slots` descriptors of
/// `item` bytes (unused ones zero), then the heap they point into. The
/// request names the page's first item (from 1) in its body's bytes 2–3.
/// `write` writes one item's strings and descriptors to the heap and
/// returns its table descriptor, or `None` for an item that is not listed
/// (an empty flow slot: passed over, taking no room). More follow
/// (`0x8112`) when an item is left once the table is full, or when an item
/// has brought the reply to [`SOFT_LIMIT`] bytes: that item stands in the
/// table but is not counted (Dante Controller asks for it again on the
/// next page).
pub(super) fn page<I>(
    h: &Header,
    req: &[u8],
    slots: usize,
    item: usize,
    items: impl IntoIterator<Item = I>,
    write: impl FnMut(I, &mut Heap) -> Option<Vec<u8>>,
) -> ArcOutcome {
    let Some(first) = first_item(req) else {
        return refuse(h, "a page request without its first item");
    };
    paged(h, 0, first, slots, item, items, write)
}

/// [`page`]'s reply from item `first` (from 1, not 0), with `prefix` zero
/// bytes before `[slots, count]` (ARC 2.8.9's pages have 6, [`v289`](super::v289)).
pub(super) fn paged<I>(
    h: &Header,
    prefix: usize,
    first: u16,
    slots: usize,
    item: usize,
    items: impl IntoIterator<Item = I>,
    mut write: impl FnMut(I, &mut Heap) -> Option<Vec<u8>>,
) -> ArcOutcome {
    let mut heap = Heap {
        body: Body::new(),
        shared: None,
    };
    heap.zeros(prefix + 2 + slots * item);
    let mut table: Vec<Vec<u8>> = Vec::with_capacity(slots);
    let mut counted = 0;
    let mut left = items.into_iter().skip(usize::from(first) - 1);
    let more = loop {
        let Some(it) = left.next() else {
            break false;
        };
        if table.len() == slots {
            break true;
        }
        let Some(descriptor) = write(it, &mut heap) else {
            continue;
        };
        debug_assert_eq!(descriptor.len(), item);
        table.push(descriptor);
        if usize::from(heap.offset()) >= SOFT_LIMIT {
            break true;
        }
        counted += 1;
    };
    let mut body = heap.body.into_vec();
    body[prefix] = slots as u8;
    body[prefix + 1] = counted as u8;
    let table_at = prefix + 2;
    for (i, descriptor) in table.iter().enumerate() {
        body[table_at + i * item..table_at + (i + 1) * item].copy_from_slice(descriptor);
    }
    let status = if more { STATUS_MORE } else { STATUS_OK };
    ArcOutcome::Reply(reply(h, status, &body))
}

/// 0x2000: each TX channel with its default name.
pub(super) fn tx_channels(h: &Header, req: &[u8], cfg: &DeviceConfig) -> ArcOutcome {
    let n = cfg.tx.len();
    page(h, req, n.min(TX_PAGE), 8, 0..n, |index, out| {
        let common = out.shared(|b| common_descriptor(b, cfg));
        let name = out.cstr(&cfg.tx[index]);
        Some(descriptor(&[wire_u16(index + 1), 7, common, name]))
    })
}

/// 0x2010: each TX channel `listed` takes (by index) with its current
/// name: every one under 2.7, the renamed ones under 2.8.9 ([`v289`](super::v289)).
pub(super) fn tx_names(
    h: &Header,
    req: &[u8],
    state: &DeviceState,
    listed: impl Fn(usize) -> bool,
) -> ArcOutcome {
    let n = state.tx.len();
    page(h, req, n.min(TX_PAGE), 6, 0..n, |index, out| {
        if !listed(index) {
            return None;
        }
        // A zero word heads the names (written once, before the first).
        out.shared(|b| {
            let at = b.offset();
            b.u32(0);
            at
        });
        let id = wire_u16(index + 1);
        let name = out.cstr(&state.tx[index]);
        Some(descriptor(&[id, id, name]))
    })
}

/// Our RX flow in `slot` once its transmitter granted it (an asked flow
/// is listed from its grant), as 0x3200 and ARC 2.8.9's 0x3600 list it.
pub(super) fn granted_flow(rx: &Manager, slot: usize) -> Option<crate::subscribe::FlowView> {
    rx.flow(slot).filter(|f| f.handle.is_some())
}

/// RX channel `rx`'s subscription as 0x3000 lists it, with its status
/// code: the manager's status when it holds the same subscription, else
/// Unresolved (a subscription saved a moment ago, not handed over yet).
pub(super) fn listed_subscription<'s>(
    state: &'s DeviceState,
    rx: usize,
    manager: &Manager,
) -> Option<(&'s Subscription, u32)> {
    let sub = state.subscription(rx)?;
    let status = match manager.subscription(rx) {
        Some(known) if known == sub => manager.status(rx).unwrap_or(SubStatus::Unresolved),
        _ => SubStatus::Unresolved,
    };
    Some((sub, status.code()))
}

/// 0x3000 (inferno's `get_receive_channels`): each RX channel with, when
/// subscribed, the TX channel's and the TX device's names and the status
/// code, then its own name.
pub(super) fn rx_channels(
    h: &Header,
    req: &[u8],
    state: &DeviceState,
    cfg: &DeviceConfig,
    manager: &Manager,
) -> ArcOutcome {
    let n = state.rx.len();
    page(h, req, n.min(RX_PAGE), 20, 0..n, |index, out| {
        let common = out.shared(|b| common_descriptor(b, cfg));
        let (tx_channel, tx_device, status) = match listed_subscription(state, index, manager) {
            Some((sub, status)) => (out.cstr(&sub.tx_channel), out.cstr(&sub.tx_device), status),
            None => (0, 0, 0),
        };
        let name = out.cstr(&state.rx[index]);
        let [s0, s1] = [(status >> 16) as u16, status as u16];
        Some(descriptor(&[
            wire_u16(index + 1),
            RX_CHANNEL_FLAGS,
            common,
            tx_channel,
            tx_device,
            name,
            s0,
            s1,
            0,
            0,
        ]))
    })
}

fn descriptor(fields: &[u16]) -> Vec<u8> {
    fields.iter().flat_map(|f| f.to_be_bytes()).collect()
}
