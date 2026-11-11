// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.
// Protocol facts (the create and delete forms, the UltimoX2's answers)
// as `flows` records them; the code is our own.

//! ARC 2.8.9's multicast create and delete on our own device (0x2601,
//! 0x2602, dstack #193): what a UltimoX2 answers, on our TX table.

use std::net::Ipv4Addr;

use tracing::info;

use super::{ArcOutcome, Change, flows, refuse};
use crate::device::{DeviceConfig, DeviceState};
use crate::media::tx::TxTable;
use crate::wire::{Header, STATUS_OK, read_u16, reply};

/// ARC 2.8.9's multicast create or delete (`h.op1`, see [`super::answer`]) on the
/// TX table `tx` of the device `cfg` at `ip`, kept in its `state`
/// ([`DeviceState::multicast`], dstack #207): a flow made or deleted is
/// [`ArcOutcome::Changed`] ([`Change::TX_FLOWS_ONLY`]), which the
/// caller saves before it answers; a refusal changes neither.
///
/// **Create** (0x2601, [`flows::create_body`]'s form, read by
/// [`flows::parse_flows`]): one multicast record whose media-local id
/// names the slot (from 1) and whose channels are our TX channels (1 to
/// [`flows::MAX_FLOW_CHANNELS`], each once). The flow takes our
/// `tx_fpp_max` frames a packet (the most our advertised latency covers;
/// Dante Controller asks 0, the device's own) and our encoding, and is
/// sent to its group ([`TxTable::add_own_multicast`]). The reply is its
/// record in the list's form with no destination, as a UltimoX2 answers.
/// Refused ([`crate::wire::STATUS_ERROR`]), creating nothing: a body
/// that does not read, other than one multicast record, a slot past ours
/// or in use, channels we do not have.
///
/// **Delete** (0x2602, [`flows::delete_body`]'s form): the multicast flow
/// of the media-local id at body bytes 12–13 goes; OK and an empty body,
/// as a UltimoX2 answers. Refused when that slot holds no multicast flow.
pub(super) fn answer(
    h: &Header,
    body: &[u8],
    state: &mut DeviceState,
    cfg: &DeviceConfig,
    tx: &mut TxTable,
    ip: Ipv4Addr,
) -> ArcOutcome {
    if h.op1 == flows::DELETE {
        let Some(slot) = read_u16(body, 12).and_then(|id| usize::from(id).checked_sub(1)) else {
            return refuse(h, "a multicast delete without its flow");
        };
        if !tx.remove_multicast(slot) {
            return refuse(h, "a multicast delete of a slot without one");
        }
        state.remove_multicast(slot);
        info!(slot, "a multicast TX flow ended (ARC 2.8.9), its save next");
        return ArcOutcome::Changed(reply(h, STATUS_OK, &[]), Change::TX_FLOWS_ONLY);
    }
    let asked = match flows::parse_flows(body) {
        Some(f) if f.unread == 0 => f.flows,
        _ => return refuse(h, "a multicast create that does not read"),
    };
    let [f] = &asked[..] else {
        return refuse(h, "a multicast create of other than one flow");
    };
    if !f.multicast() {
        return refuse(h, "a create of a unicast flow");
    }
    let Some(slot) = usize::from(f.local_id).checked_sub(1) else {
        return refuse(h, "a multicast create in slot 0");
    };
    let group = match tx.add_own_multicast(cfg, ip, slot, &f.channels) {
        Ok(group) => group,
        Err(why) => return refuse(h, why),
    };
    state.set_multicast(slot, &f.channels);
    info!(
        slot,
        %group,
        channels = f.channels.len(),
        fpp = cfg.tx_fpp_max,
        "a multicast TX flow made (ARC 2.8.9), its save next"
    );
    let made = flows::TxFlow {
        id: f.local_id,
        local_id: f.local_id,
        flags: flows::MULTICAST,
        channels: f.channels.clone(),
        fpp: cfg.tx_fpp_max,
        bits: u16::from(cfg.encoding),
        destinations: Vec::new(),
    };
    ArcOutcome::Changed(
        reply(h, STATUS_OK, &flows::list_reply_body(&[made])),
        Change::TX_FLOWS_ONLY,
    )
}

#[cfg(test)]
mod tests;
