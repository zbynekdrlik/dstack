// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.
// Protocol facts (byte layouts) checked against a Dante card's exchange
// with Dante Controller (`tests/fixtures/arc_289_card.json`) and against
// netaudio's 2.8.9 readers (chris-ritsen/network-audio-controller,
// `netaudio-core/src/responses/channel_status.rs`); the code is our own.

//! ARC 2.8.9's reads (start code [`flows::START_2809`], dstack #216).
//! Dante Controller picks the protocol from a device's ARC TXT
//! (`arcp_vers`, [`super::ARCP_VERSION`]): to a 2.8.9 device it sends
//! these reads at its start, and it offers the Rename only to a 2.8.9
//! device whose 0x1000 states the rename bit ([`COUNTS_RENAME`]; #216
//! Finding 17). Its 0x1000 also decides which lists it reads: with bit
//! 0x1000 (segmented flows) 0x2400, 0x3400, 0x2600 and 0x3600, without it
//! the 2.7 lists (0x2000, 0x3000, 0x2200, 0x3200) under 0x2809. Our
//! devices leave 0x1000 clear (dstack #223): with it, Dante Controller
//! reads 0x2600, which needs the card's TX flow records, and those would
//! make dstack receivers up to v0.6.97 drop our multicast flows; with it
//! clear, Dante Controller works on the 2.7 lists. The 2.8.9 lists are
//! still answered for a controller that asks. Each
//! read below is answered in the card's layout with our values; the other
//! requests under 0x2809 (0x1100, 0x1102, 0x2600, 0x3300, 0x2320, 2.7's
//! writes) are answered as under 2.7 ([`super::handle`]). Dante
//! Controller keeps 2.7's writes for our devices (0x3010, 0x3001, 0x1001;
//! #216 Finding 18); netaudio writes with 2.8.9's own, which [`writes`]
//! answers (#220).
//!
//! | op1 | answer |
//! |---|---|
//! | 0x1000 | the 2.7 counts (word 0 with the rename bit 0x0001 set), then a media record of (TX, RX) pairs ([`counts`]) |
//! | 0x1003 | the names in 32-byte fields and a versions record ([`device_names`]) |
//! | 0x2010 | the 2.7 page of the TX channels whose name is not the default, as the card lists them |
//! | 0x2204 | TX flow labels: none (start code 0x2801, as both 2.8.9 devices answer) |
//! | 0x2400 | TX channels: number, current and default name, format ([`TX_CHANNELS`]) |
//! | 0x3400 | RX channels: the same, and the subscription with its status ([`RX_CHANNELS`]) |
//! | 0x3600 | RX flows: number, format, latency, our address, which RX channel each slot feeds ([`RX_FLOWS`]) |
//!
//! **Pages** (0x2400, 0x3400, 0x3600): the request body holds, from byte
//! 6, `1`, the media selector (1, audio), the first item (from 1) and the
//! last (0: to the end). The reply: 6 zero bytes, `[slots, count]`,
//! `slots` record offsets (from the datagram's first byte), then the
//! format (written once a page, from a multiple of 4), the strings and the
//! records (each record from a multiple of 4), paged as 2.7's
//! ([`super::paged`]). Another media than audio gets an empty
//! page; a request without its first item is refused.
//!
//! **Records** are chains of segments, each starting `[its words, the
//! words from it to the record's end]`, as the card writes them.

use super::{
    ArcOutcome, DeviceConfig, DeviceState, FLOW_PAGE, Header, Heap, Live, MAX_CHANNELS_IN_FLOW,
    MAX_FLOWS, Manager, REVISION, RX_PAGE, STATUS_OK, TX_PAGE, common_descriptor, flows,
    granted_flow, listed_subscription, op, paged, refuse, reply, stated_latency_ns, wire_u16,
};
use crate::device::MAX_NAME;
use crate::wire::{Body, read_u16};

mod writes;

pub(super) use writes::write;

/// The TX channels' list.
pub(super) const TX_CHANNELS: u16 = 0x2400;
/// The RX channels' list.
pub(super) const RX_CHANNELS: u16 = 0x3400;
/// The RX flows' list.
pub(super) const RX_FLOWS: u16 = 0x3600;
/// The start code of the 0x2204 reply (both 2.8.9 devices on the site
/// answer with it).
const START_2801: u16 = 0x2801;
/// The media type of an audio channel or flow.
const MEDIA_AUDIO: u16 = 3;
/// A page request's media selector for audio.
const AUDIO_SELECTOR: u16 = 1;
/// The zero bytes before a page's `[slots, count]`.
const PAGE_PREFIX: usize = 6;
/// 0x1000's first word under 2.8.9: the 2.7 flags and this bit, the
/// device rename (word 0's 0x0001; Dante Controller greys the Rename
/// without it).
const COUNTS_RENAME: u8 = 0x01;
/// A TX channel's flags, the card's.
const TX_FLAGS: u32 = 0x0000_0007;
/// An RX channel's flags: the card's 6 (netaudio: rename allowed) and the
/// can-subscribe-self bit, as our 2.7 record states them
/// ([`super::RX_CHANNEL_FLAGS`]).
const RX_FLAGS: u32 = super::RX_CHANNEL_FLAGS as u32;
/// An RX flow's flags, the card's.
const RX_FLOW_FLAGS: u32 = 0x0000_0001;
/// An RX flow's receiver state, the card's for a receiving flow.
const RX_FLOW_STATE: u16 = 0x0101;
/// A name field of 0x1003: 31 bytes and a NUL at least.
const NAME_FIELD: usize = 32;
/// 0x1003's records and strings, packet offsets: the names record, the
/// versions record, the name, the factory name, the board name.
const NAMES_AT: u16 = 28;
const VERSIONS_AT: u16 = 40;
const NAME_AT: u16 = 60;
const FACTORY_AT: u16 = NAME_AT + NAME_FIELD as u16;
const BOARD_AT: u16 = FACTORY_AT + NAME_FIELD as u16;

/// Our answer to `h` when it is one of ARC 2.8.9's own reads; `None` for
/// any other request (2.7's answer then).
pub(super) fn read(
    h: &Header,
    body: &[u8],
    state: &DeviceState,
    cfg: &DeviceConfig,
    live: &Live<'_>,
) -> Option<ArcOutcome> {
    if h.start != flows::START_2809 {
        return None;
    }
    let ok = |out: &[u8]| ArcOutcome::Reply(reply(h, STATUS_OK, out));
    Some(match h.op1 {
        op::COUNTS => ok(&counts(cfg)),
        op::DEVICE_NAMES => ok(&device_names(state, cfg)),
        op::TX_NAMES => super::tx_names(h, body, state, |i| cfg.tx.get(i) != state.tx.get(i)),
        op::OP_2204 => flow_labels(h, body, cfg),
        TX_CHANNELS => {
            let n = state.tx.len();
            channel_page(h, body, n.min(TX_PAGE), n, |i, out| {
                tx_channel(i, state, cfg, out)
            })
        }
        RX_CHANNELS => {
            let n = state.rx.len();
            channel_page(h, body, n.min(RX_PAGE), n, |i, out| {
                rx_channel(i, state, cfg, live.rx, out)
            })
        }
        RX_FLOWS => {
            let slots = usize::from(FLOW_PAGE);
            channel_page(h, body, slots, usize::from(MAX_FLOWS), |slot, out| {
                rx_flow(slot, cfg, live.rx, out)
            })
        }
        _ => return None,
    })
}

/// 0x1000: the 2.7 counts with word 0's [`COUNTS_RENAME`], the media record's
/// offset and a zero word, then the record (`0x1414`, 40 bytes): audio,
/// then the pairs (1, 1), (TX, RX channels), (2, 2), (TX, RX flows), (2,
/// 2), (TX, RX channels), (TX, RX channels in a flow), 8 zero bytes. The
/// constant pairs are both 2.8.9 devices' (a card, and an adapter with no
/// TX). The three count pairs read (2, 2) on the card and (0, 2) on the
/// adapter, as their channels and their flows both do, so which pair is
/// which is not known: we state channels, flows, channels (2.7 states
/// `MAX_FLOWS` flows too). A direction with no channel states no flow (the
/// adapter states 0 TX flows).
fn counts(cfg: &DeviceConfig) -> Vec<u8> {
    let mut words = super::counts(cfg);
    words[1] |= COUNTS_RENAME;
    let mut out = Body::new();
    out.bytes(&words);
    let record = out.offset() + 4;
    out.u16(record);
    out.u16(0);
    out.bytes(&[0x14, 0x14]);
    out.u16(MEDIA_AUDIO);
    let (tx, rx) = (wire_u16(cfg.tx.len()), wire_u16(cfg.rx.len()));
    let flows = |channels: u16| if channels == 0 { 0 } else { MAX_FLOWS };
    for (t, r) in [
        (1, 1),
        (tx, rx),
        (2, 2),
        (flows(tx), flows(rx)),
        (2, 2),
        (tx, rx),
        (MAX_CHANNELS_IN_FLOW, MAX_CHANNELS_IN_FLOW),
    ] {
        out.u16(t);
        out.u16(r);
    }
    out.zeros(8);
    out.into_vec()
}

/// 0x1003: the offsets of the names record (twice) and of the versions
/// record, the board name's and the revision's, 8 zero bytes (the card's
/// are stale), the names record (`[05 00]`, the name, the factory name,
/// the name, then two zero words), the versions record (`[0a 0a]`, 0,
/// router 4.3, 0, ARC 2.8.9, ARC min 0.2.4, the card's 0x1200 and 0x1004,
/// two zero words), then the name and the factory name in
/// [`NAME_FIELD`]-byte fields, the board name and the revision.
fn device_names(state: &DeviceState, cfg: &DeviceConfig) -> Vec<u8> {
    let mut out = Body::new();
    #[rustfmt::skip]
    let head: [u16; 25] = [
        NAMES_AT, NAMES_AT, VERSIONS_AT, BOARD_AT, 0, 0, 0, 0, 0,
        0x0500, NAME_AT, FACTORY_AT, NAME_AT, 0, 0,
        0x0a0a, 0, 0x0403, 0, 0x2809, 0x0204, 0x1200, 0x1004, 0, 0,
    ];
    for word in head {
        out.u16(word);
    }
    debug_assert_eq!(out.offset(), NAME_AT);
    fixed(&mut out, &state.name);
    fixed(&mut out, &cfg.name);
    out.cstr(&cfg.model);
    let revision = out.cstr(REVISION);
    out.set_u16(18, revision);
    out.into_vec()
}

/// `s` in a [`NAME_FIELD`]-byte field: its bytes, at most
/// [`MAX_NAME`] (a device name holds no more, ASCII only:
/// `valid_device_name`), then zeros, a NUL at least.
fn fixed(out: &mut Body, s: &str) {
    let name = &s.as_bytes()[..s.len().min(MAX_NAME)];
    out.bytes(name);
    out.zeros(NAME_FIELD - name.len());
}

const _: () = assert!(MAX_NAME < NAME_FIELD);

/// 0x2204, the TX flow labels: we keep none. A page with a slot per TX
/// flow of a page ([`FLOW_PAGE`], none without TX channels) and no item,
/// under start code 0x2801 as both 2.8.9 devices answer.
fn flow_labels(h: &Header, req: &[u8], cfg: &DeviceConfig) -> ArcOutcome {
    let h = Header {
        start: START_2801,
        ..*h
    };
    let slots = if cfg.tx.is_empty() {
        0
    } else {
        usize::from(FLOW_PAGE)
    };
    super::page(&h, req, slots, 2, std::iter::empty::<()>(), |_, _| None)
}

/// A 2.8.9 page of `n` items, `slots` a page: the request's media
/// selector, first and last item (body bytes 8, 10, 12). `write` writes
/// one item (from 0) to the heap and returns its record's offset.
fn channel_page(
    h: &Header,
    req: &[u8],
    slots: usize,
    n: usize,
    mut write: impl FnMut(usize, &mut Heap) -> Option<u16>,
) -> ArcOutcome {
    let (Some(media), Some(first)) = (read_u16(req, 8), read_u16(req, 10).filter(|&f| f > 0))
    else {
        return refuse(h, "a page request without its first item");
    };
    if media != AUDIO_SELECTOR {
        return ArcOutcome::Reply(reply(h, STATUS_OK, &[0; PAGE_PREFIX + 2]));
    }
    let end = match read_u16(req, 12).unwrap_or(0) {
        0 => n,
        last => n.min(usize::from(last)),
    };
    paged(h, PAGE_PREFIX, first, slots, 2, 0..end, |i, out| {
        write(i, out).map(|at| at.to_be_bytes().to_vec())
    })
}

/// A channel record's first segment (40 bytes): `segment`, the channel
/// (from 1), 0, audio, the channel again (its media-local id), 0, `flags`,
/// 0, the name's offset, the format's, 6 zero bytes, the default name's,
/// 8 zero bytes.
fn channel_head(
    out: &mut Body,
    segment: [u8; 2],
    index: usize,
    flags: u32,
    [name, format, default]: [u16; 3],
) {
    let id = wire_u16(index + 1);
    out.bytes(&segment);
    for word in [id, 0, MEDIA_AUDIO, id, 0] {
        out.u16(word);
    }
    out.u32(flags);
    out.u32(0);
    out.u16(name);
    out.u16(format);
    out.zeros(6);
    out.u16(default);
    out.zeros(8);
}

/// TX channel `index`'s record (`0x1414`, 40 bytes) and its strings; the
/// format is 2.7's common descriptor, written once a page.
fn tx_channel(
    index: usize,
    state: &DeviceState,
    cfg: &DeviceConfig,
    out: &mut Heap,
) -> Option<u16> {
    let format = out.shared(|b| {
        b.align(4);
        common_descriptor(b, cfg)
    });
    let name = state.tx.get(index)?;
    let name_at = out.cstr(name);
    let default_at = out.cstr(cfg.tx.get(index).unwrap_or(name));
    out.align(4);
    let at = out.offset();
    channel_head(
        out,
        [0x14, 0x14],
        index,
        TX_FLAGS,
        [name_at, format, default_at],
    );
    Some(at)
}

/// RX channel `index`'s record (`0x141c`, 56 bytes): the TX record's first
/// segment, then `[06 08]`, 0, the subscribed TX channel's and device's
/// names' offsets (0 without a subscription), the status and the receiver
/// detail (2.7's status word in two halves, [`listed_subscription`]),
/// then `[02 02]`, 0.
fn rx_channel(
    index: usize,
    state: &DeviceState,
    cfg: &DeviceConfig,
    manager: &Manager,
    out: &mut Heap,
) -> Option<u16> {
    let format = out.shared(|b| {
        b.align(4);
        common_descriptor(b, cfg)
    });
    let ((channel_at, device_at), code) = match listed_subscription(state, index, manager) {
        Some((sub, code)) => ((out.cstr(&sub.tx_channel), out.cstr(&sub.tx_device)), code),
        None => ((0, 0), 0),
    };
    let name = state.rx.get(index)?;
    let name_at = out.cstr(name);
    let default_at = out.cstr(cfg.rx.get(index).unwrap_or(name));
    out.align(4);
    let at = out.offset();
    channel_head(
        out,
        [0x14, 0x1c],
        index,
        RX_FLAGS,
        [name_at, format, default_at],
    );
    let [d0, d1, v0, v1] = code.to_be_bytes();
    out.bytes(&[0x06, 0x08, 0, 0]);
    out.u16(channel_at);
    out.u16(device_at);
    out.bytes(&[v0, v1, d0, d1]);
    out.bytes(&[0x02, 0x02, 0, 0]);
    Some(at)
}

/// Our RX flow in `slot` (granted, [`granted_flow`]) as a record
/// (`0x1422`): the flow (from 1), 0, audio, the flow again, 0, the card's
/// flags, 0, its name's offset (its number), the format's (written once a
/// page: rate, bits, `u32` each), its latency in ns, 12 zero bytes; then
/// `[0a 0e]`, 10 zero bytes, its slots, the mapping's offset, 4 zero
/// bytes; then `[04 00]`, the receiver state, the endpoint's offset, 0.
/// The endpoint (`08 02`, the slot's port, our address) and the mapping
/// ([`mapping`]) follow the record, where netaudio reads them. A granted
/// flow has slots (the transmitter's flow has at least one channel).
fn rx_flow(slot: usize, cfg: &DeviceConfig, manager: &Manager, out: &mut Heap) -> Option<u16> {
    let f = granted_flow(manager, slot)?;
    let format = out.shared(|b| {
        b.align(4);
        let at = b.offset();
        b.u32(cfg.sample_rate);
        b.u32(u32::from(cfg.encoding));
        at
    });
    let id = wire_u16(slot + 1);
    let name = out.cstr(&id.to_string());
    out.align(4);
    let at = out.offset();
    out.bytes(&[0x14, 0x22]);
    for word in [id, 0, MEDIA_AUDIO, id, 0] {
        out.u16(word);
    }
    out.u32(RX_FLOW_FLAGS);
    out.u32(0);
    out.u16(name);
    out.u16(format);
    out.u32(stated_latency_ns(f.latency_ns, cfg.sample_rate));
    out.zeros(12);
    out.bytes(&[0x0a, 0x0e]);
    out.zeros(10);
    out.u16(wire_u16(f.rx.len()));
    out.u16(at + 76);
    out.zeros(4);
    out.bytes(&[0x04, 0x00]);
    out.u16(RX_FLOW_STATE);
    out.u16(at + 68);
    out.u16(0);
    debug_assert_eq!(out.offset(), at + 68);
    out.bytes(&[0x08, 0x02]);
    out.u16(f.port);
    out.bytes(&manager.ip().octets());
    mapping(out, &f.rx);
    Some(at)
}

/// Which RX channel each of a flow's slots feeds (`slots[s]`: the RX
/// channels, from 0, slot `s` feeds), as one mapping node: the first RX
/// channel fed (from 1), the node's byte count, 0 (no next node), then a
/// byte per RX channel from the first: the slot feeding it (from 1), 0 for
/// none; the count rounded up to even (at least 2).
fn mapping(out: &mut Body, slots: &[Vec<usize>]) {
    let fed: Vec<(usize, u8)> = slots
        .iter()
        .enumerate()
        .flat_map(|(s, channels)| {
            let slot = u8::try_from(s + 1).unwrap_or(u8::MAX);
            channels.iter().map(move |&c| (c, slot))
        })
        .collect();
    let first = fed.iter().map(|&(c, _)| c).min().unwrap_or(0);
    let last = fed.iter().map(|&(c, _)| c).max().unwrap_or(0);
    let span = (last - first + 1).next_multiple_of(2);
    let mut bytes = vec![0u8; span];
    for (c, slot) in fed {
        bytes[c - first] = slot;
    }
    out.u16(wire_u16(first + 1));
    out.u16(wire_u16(span));
    out.u16(0);
    out.bytes(&bytes);
}
