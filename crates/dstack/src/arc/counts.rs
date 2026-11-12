// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.
// Protocol facts (byte layouts, opcodes, behaviour) checked against
// inferno (https://github.com/teodly/inferno) as a reference; the code is
// our own.

//! The counts and the names (dstack #227, #231): 0x1000's counts
//! ([`counts`]) with the device's flow slots ([`flow_slots`],
//! [`table_slots`]) and 0x1003's names ([`device_names`]), in ARC 2.7's
//! layouts. ARC 2.8.9's 0x1000 (`v289`) builds on [`counts`] and
//! [`flow_slots`]; its 0x1003 is its own.

use super::{MAX_CHANNELS_IN_FLOW, MAX_FLOWS, REVISION, wire_u16};
use crate::device::{DeviceConfig, DeviceState};
use crate::wire::{Body, HEADER_LEN};

/// The counts' flags byte, inferno's 0x30: bit 4 (0x10), TX channels can
/// be renamed; bit 5 (0x20), TX multicast flows. With bit 5 Dante
/// Controller offers Add on the Transmit tab and creates the flow with
/// 2.7's 0x2201 (or 2.8.9's 0x2601 when segmented flows are stated), which
/// our devices answer (dstack #193, #222). One literal: the bits joined by
/// `|` would leave its `^` mutant, which gives the same byte.
const COUNTS_FLAGS: u8 = 0x30;
/// The device names' header (0x1003), before its strings.
const NAMES_HEADER: usize = 38;

/// The flow slots the device has each way (dstack #227): its config's
/// `flow_slots` (one value for TX and RX, the sockets each direction
/// opens), at most [`MAX_FLOWS`], the TX table's own bound
/// ([`TxTable::with_slots`](crate::media::tx::TxTable::with_slots)).
/// 0x1000 states it, and Dante Controller creates a multicast flow in the
/// highest flow it states.
pub(super) fn flow_slots(cfg: &DeviceConfig) -> u16 {
    wire_u16(cfg.flow_slots.min(usize::from(MAX_FLOWS)))
}

/// [`flow_slots`] as a TX flow table states them (one byte: 0x2600's
/// pages and 0x2601's reply, dstack #227).
pub(super) fn table_slots(cfg: &DeviceConfig) -> u8 {
    u8::try_from(flow_slots(cfg)).unwrap_or(u8::MAX)
}

/// 0x1000: inferno's layout, with our flow slots ([`flow_slots`]) where
/// inferno states its 32.
pub(super) fn counts(cfg: &DeviceConfig) -> Vec<u8> {
    let tx = wire_u16(cfg.tx.len());
    let rx = wire_u16(cfg.rx.len());
    let mut out = Body::new();
    out.u8(0);
    out.u8(COUNTS_FLAGS);
    out.u16(tx);
    out.u16(rx);
    out.u16(4);
    out.u16(MAX_CHANNELS_IN_FLOW.min(tx));
    out.u16(8);
    // The TX and RX flows the device has.
    out.u16(flow_slots(cfg));
    out.u16(flow_slots(cfg));
    out.u16(tx.saturating_add(rx));
    out.u16(1);
    out.u16(1);
    out.zeros(12);
    out.into_vec()
}

/// 0x1003: a 38-byte header of offsets and constants, then the current
/// name, the factory name, the board name and the revision.
pub(super) fn device_names(state: &DeviceState, cfg: &DeviceConfig) -> Vec<u8> {
    let mut out = Body::new();
    out.zeros(NAMES_HEADER);
    let name = out.cstr(&state.name);
    let factory = out.cstr(&cfg.name);
    let board = out.cstr(&cfg.model);
    let revision = out.cstr(REVISION);
    #[rustfmt::skip]
    let fields: [u16; NAMES_HEADER / 2] = [
        0, 0, 0, board, revision, 0, name, factory, name,
        0, 0, 0, 0, 0, 0,
        0x2729, 0, 0x1102, 0,
    ];
    for (i, value) in fields.into_iter().enumerate() {
        out.set_u16(wire_u16(HEADER_LEN + 2 * i), value);
    }
    out.into_vec()
}
