// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! Control of another device's ARC (UDP 4440): what an application asks
//! of a device it was told to manage (the IEM card's labels, the web
//! controller): its name, its channels' labels, and the renames.
//!
//! The requests are netaudio's (v0.0.10, `netaudio/dante/device.py`), byte
//! for byte; our own server ([`super::handle`]) answers them, so every
//! request here is tested against it:
//!
//! - 0x1002, the device's name: no body; the reply's body is the name,
//!   NUL-ended.
//! - 0x1000, the counts: no body; TX at body bytes 2–3, RX at 4–5.
//! - 0x3000, the RX channels ([`rx_channels`]), paged ([`super::client::page_request`]):
//!   `[slots, count]`, then `slots` 20-byte descriptors `id, flags,
//!   common, tx channel, tx device, name, status (2), 0, 0`; offsets count
//!   from the datagram's first byte.
//! - The TX labels: the 0x2010 own name, else the 0x2000 default
//!   ([`super::client::read_directory`]), as netaudio's `friendly_name or
//!   name`.
//! - 0x3001 (RX) and 0x2013 (TX), a channel's label ([`rename_request`]):
//!   RX body `02 01 <channel u16> <name offset 20> 00000000 <name> 00`, TX
//!   `02 01 0000 <channel u16> <name offset 24> 000000000000 <name> 00`;
//!   an empty name resets the label to the channel's default.
//! - 0x1001, the device's name ([`device_rename_request`]): body `<name>
//!   00`.
//! - 0x3010 and 0x3014, a subscription and its end ([`subscribe_request`],
//!   [`unsubscribe_request`]), as netaudio sends them.
//! - 0x1100, the settings ([`settings`]), asked with start code 0x2809 and
//!   the wanted keys (`n`, then `n` keys, as netaudio 0.4.2 asks), as every
//!   hardware device on the site answers (with 0x27ff and no body, only
//!   some list their table): `[n, m]`, then `n` pairs `key, value`; with
//!   the key's top bit set the value is the packet offset of a `u32`; a
//!   pair `0, k` says the device has no key `k`.
//!   Confirmed against netaudio's readings on six devices (dstack #16).
//! - 0x3200, the RX flows ([`rx_flows`]), paged: `[slots, count]`, then
//!   each flow's descriptor offset. A descriptor: flow (the slot, 1 up),
//!   type, rate (`u32`), 0, encoding, sockets, channel slots, words a mask,
//!   the sockets' offsets, each channel slot's mask offset (a bit per RX
//!   channel, 16 a word), descriptor 2's offset; descriptor 2 holds the
//!   flow's latency in ns at byte 8 (inferno's `query_rx_flows`; the
//!   hardware receivers on the site answer the same). The flows' latencies
//!   are what a receiver's lateness (the heartbeat, [`crate::latency`]) is
//!   measured against.
//!
//! Nothing here changes anything but what the caller names, on the one
//! device it talks to; whether a device may be changed at all is the
//! application's call (the owner's rule: no foreign device without the
//! owner).

use std::collections::BTreeMap;

use tracing::debug;

use super::client::{Exchange, Page, ReadError, START, page_request, read_list, read_tx, table};
use super::op;
use crate::subscribe::SubStatus;
use crate::wire::{HEADER_LEN, STATUS_MORE, STATUS_OK, read_u16, request, string_at};

/// The device's own name (netaudio's `device_name`; [`op::DEVICE_NAME`]).
pub const GET_NAME: u16 = op::DEVICE_NAME;
/// The device's rename (netaudio's `set_name`).
pub const SET_NAME: u16 = 0x1001;
/// A 0x3000 descriptor's size.
const RX_ITEM: usize = 20;
/// A 0x3200 page item's size: the flow descriptor's offset.
const FLOW_ITEM: usize = 2;
/// The most RX flows a list is read for: the hardware receivers on the site
/// have 64 slots.
const MAX_RX_FLOWS: usize = 256;

/// A channel's direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Tx,
    Rx,
}

/// One exchange of a whole request body: the reply's status and body.
pub trait Call {
    fn call(&mut self, op1: u16, body: &[u8]) -> Result<(u16, Vec<u8>), ReadError>;

    /// [`Call::call`] with the start code `start` in the header instead of
    /// the usual 0x27ff (the settings read asks with 0x2809). An exchange
    /// that cannot send another start code sends its usual one.
    fn call_as(&mut self, start: u16, op1: u16, body: &[u8]) -> Result<(u16, Vec<u8>), ReadError> {
        let _ = start;
        self.call(op1, body)
    }
}

/// A [`Call`] read page by page, as the directory reader asks.
struct Pages<'a, C: Call>(&'a mut C);

impl<C: Call> Exchange for Pages<'_, C> {
    fn ask(&mut self, op1: u16, start: u16) -> Result<(u16, Vec<u8>), ReadError> {
        let page = page_request(0, op1, start);
        self.0.call(op1, &page[HEADER_LEN..])
    }
}

/// netaudio's request for a channel's label: `label` `None` resets it.
pub fn rename_request(seq: u16, dir: Dir, channel: u16, label: Option<&str>) -> Vec<u8> {
    let [hi, lo] = channel.to_be_bytes();
    let mut body = match dir {
        Dir::Rx => vec![2, 1, hi, lo, 0, 20, 0, 0, 0, 0],
        Dir::Tx => vec![2, 1, 0, 0, hi, lo, 0, 24, 0, 0, 0, 0, 0, 0],
    };
    body.extend_from_slice(label.unwrap_or("").as_bytes());
    body.push(0);
    let op1 = match dir {
        Dir::Rx => op::RX_RENAME,
        Dir::Tx => op::TX_RENAME,
    };
    request(START, seq, op1, 0, &body)
}

/// netaudio's request for the device's new name.
pub fn device_rename_request(seq: u16, name: &str) -> Vec<u8> {
    let mut body = name.as_bytes().to_vec();
    body.push(0);
    request(START, seq, SET_NAME, 0, &body)
}

/// netaudio's subscription of RX channel `rx` to `tx_channel` at
/// `tx_device`: `02 01 <rx u16> 0034 <device offset u16>`, 34 zero bytes,
/// then both names NUL-ended (offsets count from the datagram's first byte).
pub fn subscribe_request(seq: u16, rx: u16, tx_channel: &str, tx_device: &str) -> Vec<u8> {
    const CHANNEL_AT: u16 = 0x34;
    let device_at = usize::from(CHANNEL_AT) + tx_channel.len() + 1;
    let [r0, r1] = rx.to_be_bytes();
    let [d0, d1] = u16::try_from(device_at).unwrap_or(u16::MAX).to_be_bytes();
    let mut body = vec![2, 1, r0, r1, 0, 0x34, d0, d1];
    body.extend([0; 34]);
    for name in [tx_channel, tx_device] {
        body.extend_from_slice(name.as_bytes());
        body.push(0);
    }
    request(START, seq, op::RX_SUBSCRIBE, 0, &body)
}

/// netaudio's unsubscribe of RX channel `rx`: `0001 0000 <rx u16>`.
pub fn unsubscribe_request(seq: u16, rx: u16) -> Vec<u8> {
    let [r0, r1] = rx.to_be_bytes();
    request(START, seq, op::RX_UNSUBSCRIBE, 0, &[0, 1, 0, 0, r0, r1])
}

/// The request body of `datagram` (built by one of the requests above).
fn body_of(datagram: &[u8]) -> &[u8] {
    &datagram[HEADER_LEN..]
}

/// The device's own name (a reply with an empty name does not read).
pub fn device_name(c: &mut impl Call) -> Result<String, ReadError> {
    let (_, body) = ok(c.call(GET_NAME, &[])?, GET_NAME)?;
    let end = body.iter().position(|&b| b == 0).unwrap_or(body.len());
    let name =
        std::str::from_utf8(&body[..end]).map_err(|_| ReadError::Malformed { op: GET_NAME })?;
    if name.is_empty() {
        return Err(ReadError::Malformed { op: GET_NAME });
    }
    Ok(name.to_string())
}

/// A reply with status OK, else the refusal.
fn ok(reply: (u16, Vec<u8>), op: u16) -> Result<(u16, Vec<u8>), ReadError> {
    if reply.0 == STATUS_OK {
        Ok(reply)
    } else {
        Err(ReadError::Refused {
            op,
            status: reply.0,
        })
    }
}

/// A device's channel counts and labels, by channel number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Labels {
    pub tx_count: usize,
    pub rx_count: usize,
    pub tx: BTreeMap<u16, String>,
    pub rx: BTreeMap<u16, String>,
}

impl Labels {
    /// Every channel the counts promise has a label.
    pub fn complete(&self) -> bool {
        self.tx_count > 0
            && self.rx_count > 0
            && self.tx.len() == self.tx_count
            && self.rx.len() == self.rx_count
    }

    /// The label of `dir`'s channel `channel`.
    pub fn get(&self, dir: Dir, channel: u16) -> Option<&str> {
        match dir {
            Dir::Tx => self.tx.get(&channel),
            Dir::Rx => self.rx.get(&channel),
        }
        .map(String::as_str)
    }
}

/// The device's counts and every channel's label.
pub fn labels(c: &mut impl Call) -> Result<Labels, ReadError> {
    let (_, counts) = ok(c.call(op::COUNTS, &[])?, op::COUNTS)?;
    let malformed = ReadError::Malformed { op: op::COUNTS };
    let tx_count = usize::from(read_u16(&counts, 2).ok_or(malformed)?);
    let rx_count = usize::from(read_u16(&counts, 4).ok_or(malformed)?);
    let directory = read_tx(&mut Pages(c), tx_count)?;
    let tx = directory
        .channels()
        .iter()
        .map(|ch| (ch.id, ch.name.clone().unwrap_or_else(|| ch.default.clone())))
        .collect();
    let rx = rx_names(c, rx_count)?;
    Ok(Labels {
        tx_count,
        rx_count,
        tx,
        rx,
    })
}

/// One RX channel as 0x3000 lists it: its label and, when subscribed, the
/// TX channel and device it asks for and the subscription's status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RxChannel {
    /// The channel's number (1 up).
    pub channel: u16,
    pub label: String,
    /// The subscribed TX channel's name (`None`: not subscribed).
    pub tx_channel: Option<String>,
    /// The subscribed TX device's name (`None`: not subscribed).
    pub tx_device: Option<String>,
    /// The status code (descriptor bytes 12–15; 0 when not subscribed).
    pub status: u32,
}

impl RxChannel {
    /// The subscription status: the word's low 16 bits, when Dante's table
    /// holds them (`None` when not subscribed).
    pub fn sub_status(&self) -> Option<SubStatus> {
        SubStatus::from_code(self.status)
    }

    /// The receiver detail: the word's high 16 bits (`0x0101` connected;
    /// real devices also give `0x0301` while receiving, meaning unknown).
    pub fn receiver_detail(&self) -> u16 {
        crate::subscribe::sub_status::receiver_detail(self.status)
    }
}

/// A non-empty name at offset `at` of the datagram's body (0: none).
fn name_at(body: &[u8], at: u16) -> Option<String> {
    (at != 0)
        .then(|| string_at(body, at))
        .flatten()
        .filter(|n| !n.is_empty())
        .map(str::to_string)
}

/// A page of 0x3000 (the RX channels) with status `status`: each named
/// channel (a channel without a name is counted, not listed).
fn rx_page(status: u16, body: &[u8]) -> Option<Page<RxChannel>> {
    let (slots, count) = table(body, RX_ITEM)?;
    let items = (0..count)
        .filter_map(|i| {
            let at = 2 + i * RX_ITEM;
            let channel = read_u16(body, at).filter(|&id| id > 0)?;
            let label = name_at(body, read_u16(body, at + 10)?)?;
            let status = u32::from_be_bytes(body.get(at + 12..at + 16)?.try_into().ok()?);
            Some(RxChannel {
                channel,
                label,
                tx_channel: name_at(body, read_u16(body, at + 6)?),
                tx_device: name_at(body, read_u16(body, at + 8)?),
                status,
            })
        })
        .collect();
    Some(Page {
        more: status == STATUS_MORE,
        slots,
        count,
        items,
    })
}

/// Every RX channel of the device with its subscription (0x1000's RX
/// count, then 0x3000 page by page).
pub fn rx_channels(c: &mut impl Call) -> Result<Vec<RxChannel>, ReadError> {
    let (_, counts) = ok(c.call(op::COUNTS, &[])?, op::COUNTS)?;
    let total = usize::from(read_u16(&counts, 4).ok_or(ReadError::Malformed { op: op::COUNTS })?);
    rx_list(c, total)
}

fn rx_list(c: &mut impl Call, total: usize) -> Result<Vec<RxChannel>, ReadError> {
    read_list(&mut Pages(c), op::RX_CHANNELS, total, rx_page)
}

/// The RX channels' labels of a device with `total` RX channels.
fn rx_names(c: &mut impl Call, total: usize) -> Result<BTreeMap<u16, String>, ReadError> {
    Ok(rx_list(c, total)?
        .into_iter()
        .map(|ch| (ch.channel, ch.label))
        .collect())
}

/// Sets `dir`'s channel `channel` to `label` (`None`: its default).
pub fn rename_channel(
    c: &mut impl Call,
    dir: Dir,
    channel: u16,
    label: Option<&str>,
) -> Result<(), ReadError> {
    debug!(
        ?dir,
        channel,
        reset = label.is_none(),
        "ARC: a channel's label sent"
    );
    let req = rename_request(0, dir, channel, label);
    let op1 = match dir {
        Dir::Rx => op::RX_RENAME,
        Dir::Tx => op::TX_RENAME,
    };
    ok(c.call(op1, body_of(&req))?, op1).map(|_| ())
}

/// Renames the device to `name`.
pub fn rename_device(c: &mut impl Call, name: &str) -> Result<(), ReadError> {
    debug!("ARC: the device's new name sent");
    let req = device_rename_request(0, name);
    ok(c.call(SET_NAME, body_of(&req))?, SET_NAME).map(|_| ())
}

/// Subscribes the device's RX channel `rx` to `tx_channel` at `tx_device`.
/// A device answers OK also when it changed nothing (a name its rules
/// refuse, a channel it does not have): read the channel back with
/// [`rx_channels`] to see what it made of it.
pub fn subscribe(
    c: &mut impl Call,
    rx: u16,
    tx_channel: &str,
    tx_device: &str,
) -> Result<(), ReadError> {
    debug!(channel = rx, "ARC: a subscription sent");
    let req = subscribe_request(0, rx, tx_channel, tx_device);
    ok(c.call(op::RX_SUBSCRIBE, body_of(&req))?, op::RX_SUBSCRIBE).map(|_| ())
}

/// Ends the subscription of the device's RX channel `rx` (OK also when it
/// had none).
pub fn unsubscribe(c: &mut impl Call, rx: u16) -> Result<(), ReadError> {
    debug!(channel = rx, "ARC: an unsubscription sent");
    let req = unsubscribe_request(0, rx);
    ok(
        c.call(op::RX_UNSUBSCRIBE, body_of(&req))?,
        op::RX_UNSUBSCRIBE,
    )
    .map(|_| ())
}

/// The start code the settings read asks with.
pub const SETTINGS_START: u16 = 0x2809;

/// The settings' keys (0x1100) the stack names.
pub mod key {
    pub const SAMPLE_RATE: u16 = 0x8020;
    pub const PULLUP: u16 = 0x8021;
    pub const LATENCY_DEFAULT: u16 = 0x8204;
    pub const LATENCY_CONFIGURED: u16 = 0x8205;
    pub const LATENCY_ACTIVE: u16 = 0x8301;
    pub const LATENCY_MAX: u16 = 0x8302;
    pub const LATENCY_MIN: u16 = 0x8306;
}

/// The keys the settings read asks for: those [`key`] names. Every
/// hardware device on the site answers each (1.11: two Yamaha cards, a
/// Tascam, a wireless receiver, an ESI interface, DVS).
pub const SETTING_KEYS: [u16; 7] = [
    key::SAMPLE_RATE,
    key::PULLUP,
    key::LATENCY_DEFAULT,
    key::LATENCY_CONFIGURED,
    key::LATENCY_ACTIVE,
    key::LATENCY_MAX,
    key::LATENCY_MIN,
];

/// A device's settings (0x1100): what it answered, each named key read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    pub sample_rate: Option<u32>,
    pub pullup: Option<u32>,
    /// The device's latency, ns: as configured, in effect, its default,
    /// and the range it allows.
    pub latency_configured_ns: Option<u32>,
    pub latency_active_ns: Option<u32>,
    pub latency_default_ns: Option<u32>,
    pub latency_min_ns: Option<u32>,
    pub latency_max_ns: Option<u32>,
    /// Every key answered, with its value (the `u32` it points at for a
    /// key with the top bit set).
    pub all: BTreeMap<u16, u32>,
}

/// The settings request's body: the count, then the keys.
pub fn settings_request_body(keys: &[u16]) -> Vec<u8> {
    let mut out = u16::try_from(keys.len())
        .unwrap_or(u16::MAX)
        .to_be_bytes()
        .to_vec();
    for k in keys {
        out.extend_from_slice(&k.to_be_bytes());
    }
    out
}

/// A 0x1100 reply body: each pair that reads (a pointed value past the
/// body is left out, and so is key 0, which marks a key asked for that the
/// device does not have); `None` when the pairs pass the body.
pub fn parse_settings(body: &[u8]) -> Option<Settings> {
    let n = usize::from(*body.first()?);
    let mut all = BTreeMap::new();
    for k in 0..n {
        let at = 2 + 4 * k;
        let (key, value) = (read_u16(body, at)?, read_u16(body, at + 2)?);
        if key == 0 {
            continue;
        }
        let value = if key & 0x8000 != 0 {
            match usize::from(value)
                .checked_sub(HEADER_LEN)
                .and_then(|o| read_u32(body, o))
            {
                Some(v) => v,
                None => continue,
            }
        } else {
            u32::from(value)
        };
        all.insert(key, value);
    }
    let get = |k: u16| all.get(&k).copied();
    Some(Settings {
        sample_rate: get(key::SAMPLE_RATE),
        pullup: get(key::PULLUP),
        latency_configured_ns: get(key::LATENCY_CONFIGURED),
        latency_active_ns: get(key::LATENCY_ACTIVE),
        latency_default_ns: get(key::LATENCY_DEFAULT),
        latency_min_ns: get(key::LATENCY_MIN),
        latency_max_ns: get(key::LATENCY_MAX),
        all,
    })
}

/// The device's settings (0x1100 with [`SETTINGS_START`] and
/// [`SETTING_KEYS`]). A device that keeps none answers an empty table.
pub fn settings(c: &mut impl Call) -> Result<Settings, ReadError> {
    let (_, body) = ok(
        c.call_as(
            SETTINGS_START,
            op::ZEROS_1100,
            &settings_request_body(&SETTING_KEYS),
        )?,
        op::ZEROS_1100,
    )?;
    parse_settings(&body).ok_or(ReadError::Malformed { op: op::ZEROS_1100 })
}

/// One RX flow a device lists (0x3200).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RxFlow {
    /// The flow's slot, 1 up: the heartbeat's lateness and late counts
    /// list slot `n` at index `n - 1`.
    pub slot: u16,
    /// The flow's sample rate.
    pub rate: u32,
    /// The flow's latency, ns: a packet older than this plays late.
    pub latency_ns: u32,
    /// The RX channels it feeds, 1 up, in order.
    pub channels: Vec<u16>,
}

/// A big-endian `u32` at `at` of `body`.
fn read_u32(body: &[u8], at: usize) -> Option<u32> {
    let bytes = body.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes(bytes.try_into().ok()?))
}

/// The flow whose descriptor is at packet offset `main` of `body`; `None`
/// when any part of it lies outside the body.
fn rx_flow_at(body: &[u8], main: u16) -> Option<RxFlow> {
    let at = usize::from(main).checked_sub(HEADER_LEN)?;
    let slot = read_u16(body, at)?;
    let rate = read_u32(body, at + 4)?;
    let sockets = usize::from(read_u16(body, at + 12)?);
    let slots = usize::from(read_u16(body, at + 14)?);
    let words = usize::from(read_u16(body, at + 16)?);
    let masks = at + 18 + 2 * sockets;
    let mut fed = std::collections::BTreeSet::new();
    for k in 0..slots {
        let mask = usize::from(read_u16(body, masks + 2 * k)?).checked_sub(HEADER_LEN)?;
        for w in 0..words {
            let bits = read_u16(body, mask + 2 * w)?;
            for b in 0..16u16 {
                if bits & (1 << b) != 0 {
                    fed.insert(u16::try_from(w).ok()?.checked_mul(16)? + b + 1);
                }
            }
        }
    }
    let descriptor2 = usize::from(read_u16(body, masks + 2 * slots)?).checked_sub(HEADER_LEN)?;
    Some(RxFlow {
        slot,
        rate,
        latency_ns: read_u32(body, descriptor2 + 8)?,
        channels: fed.into_iter().collect(),
    })
}

/// A page of 0x3200 (the RX flows) with status `status`: each flow that
/// reads (one that does not is counted, not listed).
fn rx_flow_page(status: u16, body: &[u8]) -> Option<Page<RxFlow>> {
    let (slots, count) = table(body, FLOW_ITEM)?;
    let items = (0..count)
        .filter_map(|i| rx_flow_at(body, read_u16(body, 2 + i * FLOW_ITEM)?))
        .collect();
    Some(Page {
        more: status == STATUS_MORE,
        slots,
        count,
        items,
    })
}

/// Every RX flow the device lists (0x3200, page by page): what its
/// receivers' lateness is measured against.
pub fn rx_flows(c: &mut impl Call) -> Result<Vec<RxFlow>, ReadError> {
    read_list(&mut Pages(c), op::RX_FLOWS, MAX_RX_FLOWS, rx_flow_page)
}

#[cfg(test)]
pub(crate) mod tests;
