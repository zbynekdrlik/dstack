// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.
// Protocol facts (byte layouts) checked against netaudio's 2.8.9 encoders
// (chris-ritsen/network-audio-controller, `netaudio-core/src/commands/
// subscriptions.rs` and `commands/flows.rs`) and its reply parser
// (`responses/channel_status.rs`); the code is our own.

//! ARC 2.8.9's writes (start code [`flows::START_2809`], dstack #220), as
//! netaudio sends them to a device that announces 2.8.9 (it picks them
//! from `arcp_vers`; Dante Controller keeps 2.7's writes for our devices,
//! #216 Finding 18). Each makes the change of the 2.7 write of its kind,
//! through the same function, so both generations change a device alike:
//!
//! | op1 | change | answer |
//! |---|---|---|
//! | 0x3410 | subscribe or unsubscribe RX channels, as 0x3010 ([`subscribe_items`]) | OK, no body |
//! | 0x3401 | rename RX channels, as 0x3001 ([`rename_channels`]) | OK, no body |
//! | 0x2438 | rename TX channels, as 0x2013 | OK, the records with the names their channels now carry |
//!
//! netaudio renames a TX channel with 2.7's 0x2013, under 0x2809 too; 2.7's
//! handler answers it. 0x2438 is netaudio-core's TX-name reconcile; its
//! answer is the layout netaudio's parser reads for it.
//!
//! **Body:** 6 bytes (not read; zero from netaudio), the record size and
//! 0 (`08 00` for 0x3410, `06 00` for the renames), the page's slots (not
//! needed to read it) and the record count, then from packet byte 20 the
//! records: the channel (from 1), the media type (3, audio), the name
//! offsets (0x3410: the TX channel's and the TX device's, both 0 to
//! unsubscribe; a rename: the new name's, the name empty or the offset 0
//! for the default: Dante Controller sends a TX channel's default as offset
//! 0 in 0x2438, and 0x3401 takes it alike).
//! A body of another record size, or whose records run past its end, is
//! refused, changing nothing. A subscription record of another media is
//! passed over (we have audio channels only), as 0x3010 passes over a
//! channel we do not have; a rename of one is refused, all or nothing as
//! 2.7's.

use tracing::debug;

use super::{MEDIA_AUDIO, flows};
use crate::arc::writes::{new_name, rename_channels, renamed, subscribe_items};
use crate::arc::{ArcOutcome, Dir, Header, refuse, wire_u16};
use crate::device::{DeviceConfig, DeviceState, MAX_NAME};
use crate::wire::{Body, HEADER_LEN, MAX_DATAGRAM};

/// The subscription page (netaudio's `build_modern_arc_subscription_page`).
pub(in crate::arc) const SUBSCRIBE: u16 = 0x3410;
/// An RX channel rename (netaudio's `build_set_receiver_channel_name_for_protocol`).
pub(in crate::arc) const RX_RENAME: u16 = 0x3401;
/// TX channel names (netaudio's `build_reconcile_transmitter_channel_names_2809`).
pub(in crate::arc) const TX_NAMES: u16 = 0x2438;
/// A subscription record's bytes: channel, media, two name offsets.
const SUBSCRIPTION_RECORD: u8 = 8;
/// A name record's bytes: channel, media, the name's offset.
const NAME_RECORD: u8 = 6;
/// The body's bytes before its records: 6 bytes, the record size and 0,
/// slots, count.
const PREFIX: usize = 10;
/// The most records a 0x2438 may hold: its answer (the prefix, then per
/// record its 6 bytes and a name of at most [`MAX_NAME`] bytes and its
/// NUL) must fit one datagram.
const MAX_TX_NAMES: usize =
    (MAX_DATAGRAM - HEADER_LEN - PREFIX) / (NAME_RECORD as usize + MAX_NAME + 1);

const _: () = assert!(MAX_TX_NAMES == 38);
// The echo's record count is one byte.
const _: () = assert!(MAX_TX_NAMES <= u8::MAX as usize);

/// Our answer to `h` when it is one of ARC 2.8.9's writes; `None` for any
/// other request.
pub(in crate::arc) fn write(
    h: &Header,
    body: &[u8],
    state: &mut DeviceState,
    cfg: &DeviceConfig,
) -> Option<ArcOutcome> {
    if h.start != flows::START_2809 {
        return None;
    }
    Some(match h.op1 {
        SUBSCRIBE => subscribe(h, body, state, cfg),
        RX_RENAME => match name_items(body, Dir::Rx)
            .and_then(|items| rename_channels(&items, state, cfg, Dir::Rx))
        {
            Ok(change) => renamed(h, &[], change),
            Err(why) => refuse(h, why),
        },
        TX_NAMES => tx_names(h, body, state, cfg),
        _ => return None,
    })
}

/// The body's records of `size` bytes each, as big-endian words; `None`
/// when it holds another record size or its records run past its end.
fn records(body: &[u8], size: u8) -> Option<Vec<Vec<u16>>> {
    let (prefix, rest) = body.split_at_checked(PREFIX)?;
    if prefix[6..8] != [size, 0] {
        return None;
    }
    let table = rest.get(..usize::from(prefix[9]) * usize::from(size))?;
    Some(
        table
            .chunks_exact(usize::from(size))
            .map(|r| {
                let (words, _) = r.as_chunks::<2>();
                words.iter().map(|&w| u16::from_be_bytes(w)).collect()
            })
            .collect(),
    )
}

/// 0x3410: each audio record's RX channel subscribed to its two names, or
/// unsubscribed when either offset is 0 ([`subscribe_items`]).
fn subscribe(h: &Header, body: &[u8], state: &mut DeviceState, cfg: &DeviceConfig) -> ArcOutcome {
    let Some(records) = records(body, SUBSCRIPTION_RECORD) else {
        return refuse(h, "a subscription page that does not read");
    };
    let items: Vec<[u16; 3]> = records
        .iter()
        .filter_map(|r| {
            if r[1] == MEDIA_AUDIO {
                Some([r[0], r[2], r[3]])
            } else {
                debug!(
                    channel = r[0],
                    media = r[1],
                    "subscription of a media we do not have: passed over"
                );
                None
            }
        })
        .collect();
    subscribe_items(h, body, &items, state, cfg)
}

/// A rename's items, `(channel, its new name)`: a name offset of 0 is the
/// empty name, the channel's default (Dante Controller's "back to the
/// default"); the reason when the body does not read, a record is not an
/// audio channel or there is no name at its offset.
fn name_items(body: &[u8], dir: Dir) -> Result<Vec<(u16, &str)>, &'static str> {
    records(body, NAME_RECORD)
        .ok_or("a rename that does not read")?
        .iter()
        .map(|r| match r[..] {
            [channel, MEDIA_AUDIO, 0] => Ok((channel, "")),
            [channel, MEDIA_AUDIO, at] => Ok((channel, new_name(body, channel, at, dir)?)),
            _ => Err("a rename of a channel that is not audio"),
        })
        .collect()
}

/// 0x2438: the TX channels renamed (as 0x2013), answered with the records
/// in the request's order, each with the name its channel now carries. A
/// request of more than [`MAX_TX_NAMES`] records is refused before any
/// change: its answer might not fit one datagram.
fn tx_names(h: &Header, body: &[u8], state: &mut DeviceState, cfg: &DeviceConfig) -> ArcOutcome {
    let items = match name_items(body, Dir::Tx) {
        Ok(items) if items.len() > MAX_TX_NAMES => {
            return refuse(h, "more TX names than one answer holds");
        }
        Ok(items) => items,
        Err(why) => return refuse(h, why),
    };
    match rename_channels(&items, state, cfg, Dir::Tx) {
        Ok(change) => renamed(h, &names_echo(&items, state), change),
        Err(why) => refuse(h, why),
    }
}

/// 0x2438's answer body: 6 zero bytes, `06 00`, `[n, n]`, the `n` records
/// (channel, audio, the name's offset), then the names. Every item's
/// channel exists ([`rename_channels`] took them all).
fn names_echo(items: &[(u16, &str)], state: &DeviceState) -> Vec<u8> {
    // At most MAX_TX_NAMES items ([`tx_names`]), which fits a byte.
    let n = items.len() as u8;
    let mut out = Body::new();
    out.zeros(6);
    out.bytes(&[NAME_RECORD, 0, n, n]);
    let table = out.offset();
    for &(channel, _) in items {
        out.u16(channel);
        out.u16(MEDIA_AUDIO);
        out.u16(0);
    }
    for (k, &(channel, _)) in items.iter().enumerate() {
        let name = &state.tx[usize::from(channel) - 1];
        let at = out.cstr(name);
        out.set_u16(table + wire_u16(usize::from(NAME_RECORD) * k + 4), at);
    }
    out.into_vec()
}
