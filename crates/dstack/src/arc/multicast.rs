// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.
// Protocol facts (the create and delete forms, the UltimoX2's answers,
// Dante Controller's 2.7 create) as `flows` records them; the code is our
// own.

//! The multicast create and delete on our own device: ARC 2.8.9's (0x2601,
//! 0x2602, dstack #193), what a UltimoX2 answers, and ARC 2.7's (0x2201,
//! dstack #222), Dante Controller's on a device without segmented flows.
//! Both make and end the same flow on our TX table, kept in the same state
//! ([`make`], [`end`]).

use std::net::{Ipv4Addr, SocketAddrV4};

use tracing::info;

use super::{ArcOutcome, Change, flows, refuse, table_slots};
use crate::device::{DeviceConfig, DeviceState};
use crate::media::tx::TxTable;
use crate::wire::{Header, STATUS_OK, read_u16, reply};

/// A multicast create or delete (`h.op1`, see [`super::answer`]) on the
/// TX table `tx` of the device `cfg` at `ip`, kept in its `state`
/// ([`DeviceState::multicast`], dstack #207): a flow made or deleted is
/// [`ArcOutcome::Changed`] ([`Change::TX_FLOWS_ONLY`]), which the
/// caller saves before it answers; a refusal changes neither.
///
/// **2.8.9's create** (0x2601, [`flows::create_body`]'s form, read by
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
/// **2.8.9's delete** (0x2602, [`flows::delete_body`]'s form): the
/// multicast flow of the media-local id at body bytes 12–13 goes; OK and
/// an empty body, as a UltimoX2 answers. Refused when that slot holds no
/// multicast flow.
///
/// **2.7's create** (0x2201, [`flows::parse_create_27`]): the same flow,
/// its flow id naming the slot; OK and an empty body. Refused as 2.8.9's,
/// and for another type than multicast or a rate or encoding other than
/// ours (0 asks ours).
///
/// **2.7's delete** (0x2202, [`flows::parse_delete_27`]): the flows it
/// names go, all or none; OK and an empty body.
pub(super) fn answer(
    h: &Header,
    body: &[u8],
    state: &mut DeviceState,
    cfg: &DeviceConfig,
    tx: &mut TxTable,
    ip: Ipv4Addr,
) -> ArcOutcome {
    match h.op1 {
        flows::DELETE => {
            let Some(id) = read_u16(body, 12) else {
                return refuse(h, "a multicast delete without its flow");
            };
            end(h, state, tx, &[id])
        }
        flows::DELETE_27 => match flows::parse_delete_27(body) {
            Some(ids) => end(h, state, tx, &ids),
            None => refuse(h, "a 2.7 multicast delete that does not read"),
        },
        flows::CREATE_27 => create_27(h, body, state, cfg, tx, ip),
        _ => create_289(h, body, state, cfg, tx, ip),
    }
}

/// 2.8.9's create (see [`answer`]).
fn create_289(
    h: &Header,
    body: &[u8],
    state: &mut DeviceState,
    cfg: &DeviceConfig,
    tx: &mut TxTable,
    ip: Ipv4Addr,
) -> ArcOutcome {
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
    if let Err(refused) = make(h, state, cfg, tx, ip, f.local_id, &f.channels) {
        return refused;
    }
    let made = flows::TxFlow {
        id: f.local_id,
        local_id: f.local_id,
        flags: flows::MULTICAST,
        channels: f.channels.clone(),
        fpp: cfg.tx_fpp_max,
        bits: u16::from(cfg.encoding),
        destinations: Vec::new(),
    };
    // One record, which always fits a page; its table states our flow
    // slots, as 0x2600 does (dstack #227).
    let (record, _) = flows::list_reply_page(table_slots(cfg), &[made]);
    ArcOutcome::Changed(reply(h, STATUS_OK, &record), Change::TX_FLOWS_ONLY)
}

/// 2.7's create (see [`answer`]).
fn create_27(
    h: &Header,
    body: &[u8],
    state: &mut DeviceState,
    cfg: &DeviceConfig,
    tx: &mut TxTable,
    ip: Ipv4Addr,
) -> ArcOutcome {
    let Some(asked) = flows::parse_create_27(body) else {
        return refuse(h, "a 2.7 multicast create that does not read");
    };
    if asked.kind != flows::TYPE_MULTICAST_27 {
        return refuse(h, "a 2.7 create of another flow type than multicast");
    }
    if ![0, cfg.sample_rate].contains(&asked.rate) {
        return refuse(h, "a 2.7 multicast create at another rate than ours");
    }
    if ![0, u16::from(cfg.encoding)].contains(&asked.encoding) {
        return refuse(h, "a 2.7 multicast create in another encoding than ours");
    }
    match make(h, state, cfg, tx, ip, asked.id, &asked.channels) {
        Ok(_) => ArcOutcome::Changed(reply(h, STATUS_OK, &[]), Change::TX_FLOWS_ONLY),
        Err(refused) => refused,
    }
}

/// Makes our multicast flow of TX `channels` in the slot of flow `id`
/// (from 1) and keeps it in `state`: its group, or the refusal (slot 0, a
/// slot past ours or in use, channels we do not have), which changes
/// nothing.
fn make(
    h: &Header,
    state: &mut DeviceState,
    cfg: &DeviceConfig,
    tx: &mut TxTable,
    ip: Ipv4Addr,
    id: u16,
    channels: &[u16],
) -> Result<SocketAddrV4, ArcOutcome> {
    let Some(slot) = usize::from(id).checked_sub(1) else {
        return Err(refuse(h, "a multicast create in slot 0"));
    };
    let group = tx
        .add_own_multicast(cfg, ip, slot, channels)
        .map_err(|why| refuse(h, why))?;
    state.set_multicast(slot, channels);
    info!(
        op = format_args!("{:#06x}", h.op1),
        slot,
        %group,
        channels = channels.len(),
        fpp = cfg.tx_fpp_max,
        "a multicast TX flow made, its save next"
    );
    Ok(group)
}

/// Ends our multicast flows `ids` (from 1) and takes them out of `state`:
/// OK and an empty body. All or nothing: refused, ending none, when one
/// of their slots holds no multicast flow; a flow named twice ends once.
fn end(h: &Header, state: &mut DeviceState, tx: &mut TxTable, ids: &[u16]) -> ArcOutcome {
    let slots: Option<Vec<usize>> = ids
        .iter()
        .map(|&id| {
            usize::from(id)
                .checked_sub(1)
                .filter(|&slot| tx.flow(slot).is_some_and(|f| f.multicast))
        })
        .collect();
    let Some(mut slots) = slots else {
        return refuse(h, "a multicast delete of a slot without one");
    };
    // A flow named twice ends once.
    slots.sort_unstable();
    slots.dedup();
    for slot in slots {
        tx.remove_multicast(slot);
        state.remove_multicast(slot);
        info!(
            op = format_args!("{:#06x}", h.op1),
            slot, "a multicast TX flow ended, its save next"
        );
    }
    ArcOutcome::Changed(reply(h, STATUS_OK, &[]), Change::TX_FLOWS_ONLY)
}

#[cfg(test)]
mod tests;
