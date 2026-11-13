// SPDX-License-Identifier: MIT OR Apache-2.0
//! The flow ports of a device, as pure functions: the fixed block each
//! device slot takes by default inside one dstack range (dstack #264), and
//! which ports a configured `tx_port_base` must stay clear of (dstack #262).
//!
//! Slot `k` (the device's ARC port − 4440, the slot
//! `dstack_endpoint::slots::pick` chose) receives on
//! `FLOW_PORT_FIRST + k·SLOT_STRIDE` .. +31 and sends from the next 32
//! ports. The range 29800-31847 lies below every OS's ephemeral ports
//! (Linux 32768-60999, Windows 49152-65535) and below dsync's 31900/31910,
//! which it holds on the wildcard address on every PC; it holds the bases
//! configured before #264 (30000, 30100). The generic Dante-leg filter
//! (`tools/linux/dante-leg.nft`, `tools/windows/dante-leg.ps1`) accepts
//! exactly this range, so every device of ours on any PC is accepted.

use std::ops::RangeInclusive;

use crate::media::rx::{MAX_FLOWS, last_port};

/// Device slots on one address: slot `k` takes ARC 4440+k, flow control
/// 4455+k and CMC 8800+k (`docs/two-networks.md`'s slot ranges).
pub const PORT_SLOTS: u16 = 32;

/// The first port of the dstack flow range: slot 0's first RX port.
pub const FLOW_PORT_FIRST: u16 = 29_800;

/// The ports each slot takes: its RX block, then its TX block.
pub const SLOT_STRIDE: u16 = 2 * MAX_FLOWS as u16;

/// Where a slot's TX block starts, from its RX block's first port.
pub const TX_OFFSET: u16 = MAX_FLOWS as u16;

/// The last port of the dstack flow range (31847).
pub const FLOW_PORT_LAST: u16 = FLOW_PORT_FIRST + PORT_SLOTS * SLOT_STRIDE - 1;

// The range stays below Linux's ephemeral ports (32768) and dsync's 31900.
const _: () = assert!(FLOW_PORT_LAST < 31_900);

/// The device slot of an ARC port (`arc::PORT + k`, `k` below
/// [`PORT_SLOTS`]), `None` for any other port.
pub fn slot_of(arc_port: u16) -> Option<u16> {
    arc_port
        .checked_sub(crate::arc::PORT)
        .filter(|&k| k < PORT_SLOTS)
}

/// Slot `k`'s default RX and TX flow port bases, `k` below [`PORT_SLOTS`].
pub fn slot_bases(k: u16) -> (u16, u16) {
    let rx = FLOW_PORT_FIRST + k * SLOT_STRIDE;
    (rx, rx + TX_OFFSET)
}

/// A device's flow port bases as it binds them ([`flow_bases`]), and the
/// start WARNs they give.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowBases {
    pub rx: u16,
    pub tx: u16,
    /// The device's slot, `None` for an ARC port outside every slot's.
    pub slot: Option<u16>,
    pub warnings: Vec<String>,
}

/// The flow port bases of a device with ARC port `arc_port`: a configured
/// base (`rx`, `tx`) wins, an unset one is its slot's ([`slot_bases`]).
/// A configured base whose ports leave the dstack range is a WARN (the
/// generic filter drops them). Refused: an unset base with no slot (the
/// device never takes the OS's ports), and RX and TX blocks that overlap.
pub fn flow_bases(rx: Option<u16>, tx: Option<u16>, arc_port: u16) -> Result<FlowBases, String> {
    let slot = slot_of(arc_port);
    let default = slot.map(slot_bases);
    let pick = |set: Option<u16>, key: &str, side: fn((u16, u16)) -> u16| {
        set.or(default.map(side)).ok_or_else(|| {
            format!(
                "{key}: unset, and the ARC port {arc_port} is no device slot's ({}-{}), so the \
                 flow ports have no default; set rx_port_base and tx_port_base inside the \
                 dstack flow range {FLOW_PORT_FIRST}-{FLOW_PORT_LAST} (docs/two-networks.md)",
                crate::arc::PORT,
                crate::arc::PORT + PORT_SLOTS - 1
            )
        })
    };
    let rx_base = pick(rx, "rx_port_base", |b| b.0)?;
    let tx_base = pick(tx, "tx_port_base", |b| b.1)?;
    let block = |base: u16| u32::from(base)..=u32::from(base) + MAX_FLOWS as u32 - 1;
    let (rx_ports, tx_ports) = (block(rx_base), block(tx_base));
    if rx_ports.start() <= tx_ports.end() && tx_ports.start() <= rx_ports.end() {
        return Err(format!(
            "the RX flow ports {}-{} overlap the TX flow ports {}-{}: set rx_port_base and \
             tx_port_base {MAX_FLOWS} ports apart",
            rx_ports.start(),
            rx_ports.end(),
            tx_ports.start(),
            tx_ports.end()
        ));
    }
    let range = u32::from(FLOW_PORT_FIRST)..=u32::from(FLOW_PORT_LAST);
    let warnings = [
        ("rx_port_base", rx, rx_ports),
        ("tx_port_base", tx, tx_ports),
    ]
    .into_iter()
    .filter(|(_, set, ports)| {
        set.is_some() && !(range.contains(ports.start()) && range.contains(ports.end()))
    })
    .map(|(key, _, ports)| {
        format!(
            "{key}: its ports {}-{} leave the dstack flow range \
                 {FLOW_PORT_FIRST}-{FLOW_PORT_LAST}, which the generic Dante-leg filter \
                 accepts; a PC with that filter drops them (docs/two-networks.md)",
            ports.start(),
            ports.end()
        )
    })
    .collect();
    Ok(FlowBases {
        rx: rx_base,
        tx: tx_base,
        slot,
        warnings,
    })
}

/// The [`MAX_FLOWS`] flow ports from `base`, `None` when they would pass
/// 65535.
fn flow_ports(base: u16) -> Option<RangeInclusive<u16>> {
    last_port(base).map(|last| base..=last)
}

/// What `tx_port_base`'s ports from `base` clash with, `None` when they lie
/// in 1024..=65535 clear of the RX flow ports (`rx_base`), the device's own
/// control ports (`own`: ARC, CMC, flow control), every slot's control
/// range (ARC 4440-4471, flow control 4455-4486, CMC 8800-8831), mDNS and
/// the info ports (8700, 8702, 8708).
pub(super) fn tx_clash(base: u16, rx_base: Option<u16>, own: [u16; 3]) -> Option<String> {
    let Some(tx) = flow_ports(base).filter(|_| base >= 1024) else {
        return Some("out of range".to_string());
    };
    let slot = |first: u16| first..=first + (PORT_SLOTS - 1);
    let one = |p: u16| p..=p;
    let mut taken = vec![
        ("every slot's ARC ports", slot(crate::arc::PORT)),
        (
            "every slot's flow-control ports",
            slot(crate::flows::control::PORT),
        ),
        ("every slot's CMC ports", slot(crate::cmc::PORT)),
        ("the device's ARC port", one(own[0])),
        ("the device's CMC port", one(own[1])),
        ("the device's flow-control port", one(own[2])),
        ("mDNS", one(crate::announce::responder::PORT)),
        ("the info request port", one(crate::info::REQUEST_PORT)),
        ("the info port", one(crate::info::INFO_PORT)),
        ("the heartbeat port", one(crate::info::HEARTBEAT_PORT)),
    ];
    // An RX base past 65535 is refused on its own line; its ports up to
    // 65535 still count.
    if let Some(rx) = rx_base {
        let last = last_port(rx).unwrap_or(u16::MAX);
        taken.push(("rx_port_base's ports", rx..=last));
    }
    taken
        .into_iter()
        .find(|(_, r)| r.start() <= tx.end() && tx.start() <= r.end())
        .map(|(name, r)| {
            format!(
                "{}-{} overlaps {name} ({}-{})",
                tx.start(),
                tx.end(),
                r.start(),
                r.end()
            )
        })
}

// The TX side bounds its range with the RX side's `last_port`: one count.
const _: () = assert!(MAX_FLOWS == crate::media::tx::MAX_FLOWS);
