// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.
// Protocol facts (byte layouts, opcodes, behaviour) checked against
// inferno (https://github.com/teodly/inferno) as a reference; the code is
// our own.

//! ARC's writes that change the device state, 2.7's (0x3010, 0x3014, 0x2013,
//! 0x3001; [`super::handle`] dispatches them) and the appliers 2.8.9's writes
//! share with them (`arc/v289/writes.rs`, dstack #220): one function per
//! change, so both generations change a device alike. Subscriptions and
//! renames as the module doc of [`super`] states them.

use tracing::{debug, info};

use super::{ArcOutcome, Change, Dir, Header, refuse, unanswered};
use crate::device::{
    DeviceConfig, DeviceState, Subscription, channel_problems, valid_channel_name,
};
use crate::wire::{HEADER_LEN, STATUS_OK, read_u16, reply, string_at};

/// 0x3010's items (6 bytes each: local channel, TX channel name offset,
/// TX device name offset), read as inferno's `deserialize_items` reads
/// them: `min(count, len / 6)` items after the first two bytes. `None`
/// where inferno fails: under 2 bytes, or those items running past the end.
fn subscription_items(req: &[u8]) -> Option<Vec<[u16; 3]>> {
    const ITEM: usize = 6;
    let count = usize::from(*req.get(1)?).min(req.len() / ITEM);
    let items = req.get(2..)?.get(..count * ITEM)?;
    let (items, _) = items.as_chunks::<ITEM>();
    Some(
        items
            .iter()
            .map(|i| {
                [
                    u16::from_be_bytes([i[0], i[1]]),
                    u16::from_be_bytes([i[2], i[3]]),
                    u16::from_be_bytes([i[4], i[5]]),
                ]
            })
            .collect(),
    )
}

/// Sets RX channel `rx`'s subscription to `sub` in `state`; whether it
/// changed.
fn set_subscription(
    state: &mut DeviceState,
    cfg: &DeviceConfig,
    rx: usize,
    sub: Option<Subscription>,
) -> bool {
    state.subscription(rx) != sub.as_ref() && state.set_subscription(cfg, rx, sub)
}

/// OK, no body; [`ArcOutcome::Changed`] when `changed` names a channel.
fn subscribed(h: &Header, changed: Vec<usize>) -> ArcOutcome {
    let bytes = reply(h, STATUS_OK, &[]);
    if changed.is_empty() {
        return ArcOutcome::Reply(bytes);
    }
    ArcOutcome::Changed(
        bytes,
        Change {
            subscriptions: changed,
            ..Change::default()
        },
    )
}

/// 0x3010 (inferno's `set_channels_subscriptions`): its items, applied
/// ([`subscribe_items`]). OK, no body, to every request inferno can read
/// (also when nothing changed); a cut one gets no reply.
pub(super) fn subscribe(
    h: &Header,
    req: &[u8],
    state: &mut DeviceState,
    cfg: &DeviceConfig,
) -> ArcOutcome {
    let Some(items) = subscription_items(req) else {
        return unanswered(h, "a subscription request whose items are cut");
    };
    subscribe_items(h, req, &items, state, cfg)
}

/// Each item (`[RX channel, TX channel name offset, TX device name
/// offset]`, the offsets into `req`) subscribes its RX channel to the TX
/// channel and device its two names say, or, when either name's offset is
/// 0, unsubscribes it. Channel 0 and channels we do not have are passed
/// over; names inferno cannot read (an offset inside the header or past
/// the datagram, bytes that are not UTF-8) change nothing, and so do names
/// Dante's rules refuse (inferno subscribes to them; our state file cannot
/// keep them). OK, no body, also when nothing changed. 2.7's 0x3010 and
/// 2.8.9's 0x3410 (`arc/v289/writes.rs`) share it.
pub(super) fn subscribe_items(
    h: &Header,
    req: &[u8],
    items: &[[u16; 3]],
    state: &mut DeviceState,
    cfg: &DeviceConfig,
) -> ArcOutcome {
    let mut changed = Vec::new();
    for &[id, channel_at, device_at] in items {
        // Channel 0 is passed over here, a channel we do not have where the
        // state refuses it.
        let Some(rx) = usize::from(id).checked_sub(1) else {
            continue;
        };
        let sub = if channel_at > 0 && device_at > 0 {
            let (Some(tx_channel), Some(tx_device)) =
                (string_at(req, channel_at), string_at(req, device_at))
            else {
                debug!(channel = id, "subscription whose names cannot be read");
                continue;
            };
            let sub = Subscription {
                tx_channel: tx_channel.to_string(),
                tx_device: tx_device.to_string(),
            };
            if !sub.is_valid() {
                info!(
                    channel = id,
                    "subscription to a name Dante's rules refuse: not made"
                );
                continue;
            }
            Some(sub)
        } else {
            None
        };
        let made = sub.is_some();
        if set_subscription(state, cfg, rx, sub) && !changed.contains(&rx) {
            info!(
                seq = h.seq,
                channel = id,
                subscribed = made,
                "RX subscription changed"
            );
            changed.push(rx);
        }
    }
    subscribed(h, changed)
}

/// 0x3014, netaudio's unsubscribe (`0001 0000 <local channel>`): the
/// channel unsubscribed; OK, no body, also for channel 0, a channel we do
/// not have or one not subscribed. Under 6 bytes inferno fails to read it:
/// no reply.
pub(super) fn unsubscribe(
    h: &Header,
    req: &[u8],
    state: &mut DeviceState,
    cfg: &DeviceConfig,
) -> ArcOutcome {
    let Some(channel) = read_u16(req, 4) else {
        return unanswered(h, "an unsubscribe request without its channel");
    };
    // A channel we do not have the state refuses.
    let changed = usize::from(channel)
        .checked_sub(1)
        .filter(|&rx| set_subscription(state, cfg, rx, None));
    if changed.is_some() {
        info!(seq = h.seq, channel, "RX unsubscribed");
    }
    subscribed(h, changed.into_iter().collect())
}

/// 0x2013 and 0x3001: `[?, count]`, then `count` items (TX: `0, channel,
/// name offset`; RX: `channel, name offset`), the names behind them,
/// renamed ([`rename_channels`]): OK with `00 00` (TX) or no body (RX).
pub(super) fn rename(
    h: &Header,
    req: &[u8],
    state: &mut DeviceState,
    cfg: &DeviceConfig,
    dir: Dir,
) -> ArcOutcome {
    let (item, skip) = match dir {
        Dir::Tx => (6, 2),
        Dir::Rx => (4, 0),
    };
    let count = req.get(1).map_or(0, |&c| usize::from(c));
    let mut items = Vec::with_capacity(count);
    for k in 0..count {
        let at = 2 + k * item + skip;
        let (Some(id), Some(offset)) = (read_u16(req, at), read_u16(req, at + 2)) else {
            return refuse(h, "a rename whose items are cut short");
        };
        match new_name(req, id, offset, dir) {
            Ok(name) => items.push((id, name)),
            Err(why) => return refuse(h, why),
        }
    }
    let ok: &[u8] = match dir {
        Dir::Tx => &[0, 0],
        Dir::Rx => &[],
    };
    match rename_channels(&items, state, cfg, dir) {
        Ok(change) => renamed(h, ok, change),
        Err(why) => refuse(h, why),
    }
}

/// The new name of channel `id` at packet offset `offset` in `req`; at the
/// datagram's end an empty name (netaudio 0.4.2's reset sends no byte
/// after it, dstack #220); else the reason (no name there).
pub(super) fn new_name(req: &[u8], id: u16, offset: u16, dir: Dir) -> Result<&str, &'static str> {
    let at_end = usize::from(offset) == HEADER_LEN + req.len();
    string_at(req, offset)
        .or(at_end.then_some(""))
        .ok_or_else(|| {
            debug!(
                dir = dir.label(),
                channel = id,
                "rename refused: no name at its offset"
            );
            "no name at its offset"
        })
}

/// Status OK with `body`; [`ArcOutcome::Changed`] with `change` when a
/// write changed the state.
pub(super) fn renamed(h: &Header, body: &[u8], change: Option<Change>) -> ArcOutcome {
    let bytes = reply(h, STATUS_OK, body);
    match change {
        Some(change) => ArcOutcome::Changed(bytes, change),
        None => ArcOutcome::Reply(bytes),
    }
}

/// Renames `items` (the channel from 1, its new name) in direction `dir`,
/// all or nothing: the reason when one cannot be made (no item, no such
/// channel, a name Dante's rules refuse, names that would clash), else the
/// change, `None` when every name was already so. An empty name is the
/// channel's default. A
/// refusal's reason is a debug line only: the refusal is a gap the server's
/// once-a-minute warning counts, and Dante Controller asks again at once,
/// so a warning per request would be a flood. 2.7's 0x2013 and 0x3001 and
/// 2.8.9's 0x3401 and 0x2438 (`arc/v289/writes.rs`) share it.
pub(super) fn rename_channels(
    items: &[(u16, &str)],
    state: &mut DeviceState,
    cfg: &DeviceConfig,
    dir: Dir,
) -> Result<Option<Change>, &'static str> {
    let (names, defaults) = match dir {
        Dir::Tx => (&mut state.tx, &cfg.tx),
        Dir::Rx => (&mut state.rx, &cfg.rx),
    };
    let label = dir.label();
    if items.is_empty() {
        return Err("a rename without a channel");
    }
    let mut proposed = names.clone();
    let mut renamed = Vec::with_capacity(items.len());
    for &(id, name) in items {
        let Some(index) = usize::from(id)
            .checked_sub(1)
            .filter(|&i| i < proposed.len())
        else {
            debug!(dir = label, channel = id, "rename refused: no such channel");
            return Err("no such channel");
        };
        let name = if name.is_empty() {
            defaults[index].as_str()
        } else {
            name
        };
        if !valid_channel_name(name) {
            debug!(
                dir = label,
                channel = id,
                "rename refused: not a valid Dante channel name"
            );
            return Err("not a valid Dante channel name");
        }
        proposed[index] = name.to_string();
        renamed.push(id);
    }
    let problems = channel_problems(
        label,
        &proposed,
        (dir == Dir::Tx).then_some(defaults.as_slice()),
    );
    if !problems.is_empty() {
        debug!(dir = label, problems = %problems.join("; "), "rename refused");
        return Err("the names would clash");
    }
    if proposed == *names {
        return Ok(None);
    }
    // The channel-change notification names the RX channels renamed (inferno
    // also sends one, empty, for a rename that renamed nothing).
    let renamed_rx = match dir {
        Dir::Rx => (0..names.len())
            .filter(|&i| names[i] != proposed[i])
            .collect(),
        Dir::Tx => Vec::new(),
    };
    *names = proposed;
    info!(dir = label, channels = ?renamed, "channels renamed");
    Ok(Some(Change {
        renamed_rx,
        ..Change::default()
    }))
}
