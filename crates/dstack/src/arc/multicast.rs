// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.
// Protocol facts (the create and delete forms, the UltimoX2's answers)
// as `flows` records them; the code is our own.

//! ARC 2.8.9's multicast create and delete on our own device (0x2601,
//! 0x2602, dstack #193): what a UltimoX2 answers, on our TX table.

use std::net::Ipv4Addr;

use tracing::info;

use super::{ArcOutcome, flows, refuse};
use crate::device::DeviceConfig;
use crate::media::tx::{TxTable, multicast_group};
use crate::wire::{Header, STATUS_OK, read_u16, reply};

/// ARC 2.8.9's multicast create or delete (`h.op1`, see [`super::answer`]) on the
/// TX table `tx` of the device `cfg` at `ip`.
///
/// **Create** (0x2601, [`flows::create_body`]'s form, read by
/// [`flows::parse_flows`]): one multicast record whose media-local id
/// names the slot (from 1) and whose channels are our TX channels (1 to
/// [`flows::MAX_FLOW_CHANNELS`], each once). The flow takes our
/// `tx_fpp_max` frames a packet (the most our advertised latency covers;
/// Dante Controller asks 0, the device's own) and our encoding, and is
/// sent to [`multicast_group`]. The reply is its record in the list's
/// form with no destination, as a UltimoX2 answers. Refused
/// ([`crate::wire::STATUS_ERROR`]), creating nothing: a body that does not read, other
/// than one multicast record, a slot past ours or in use, channels we do
/// not have.
///
/// **Delete** (0x2602, [`flows::delete_body`]'s form): the multicast flow
/// of the media-local id at body bytes 12–13 goes; OK and an empty body,
/// as a UltimoX2 answers. Refused when that slot holds no multicast flow.
pub(super) fn answer(
    h: &Header,
    body: &[u8],
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
        info!(slot, "a multicast TX flow deleted (ARC 2.8.9)");
        return ArcOutcome::Reply(reply(h, STATUS_OK, &[]));
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
    if !flow_channels(&f.channels, cfg.tx.len()) {
        return refuse(h, "a multicast create of channels we do not have");
    }
    let Some(slot) = usize::from(f.local_id).checked_sub(1) else {
        return refuse(h, "a multicast create in slot 0");
    };
    let group = multicast_group(ip, cfg.process_id, slot);
    let bps = cfg.encoding / 8;
    if let Err(why) = tx.add_multicast(slot, group, &f.channels, cfg.tx_fpp_max, bps) {
        return refuse(h, why);
    }
    info!(
        slot,
        %group,
        channels = f.channels.len(),
        fpp = cfg.tx_fpp_max,
        "a multicast TX flow created (ARC 2.8.9)"
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
    ArcOutcome::Reply(reply(h, STATUS_OK, &flows::list_reply_body(&[made])))
}

/// Whether `channels` can make a flow of a device with `tx` TX channels:
/// 1 to [`flows::MAX_FLOW_CHANNELS`] of them, each one of ours (from 1)
/// and each once.
fn flow_channels(channels: &[u16], tx: usize) -> bool {
    (1..=flows::MAX_FLOW_CHANNELS).contains(&channels.len())
        && channels.iter().all(|&c| (1..=tx).contains(&usize::from(c)))
        && (1..channels.len()).all(|i| !channels[..i].contains(&channels[i]))
}

#[cfg(test)]
mod tests;
