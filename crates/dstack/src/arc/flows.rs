// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.
// Protocol facts (byte layouts, opcodes, behaviour) checked against the
// Dante Controller exchanges netaudio records (chris-ritsen/
// network-audio-controller, `tests/fixtures/multicast_creation_2809.json`)
// and against a UltimoX2 (platform software 4.2.5.1, dstack #148); the code
// is our own.

//! Another device's TX flows over ARC 2.8.9 (UDP 4440, start code
//! [`START_2809`], dstack #148): the list ([`list_tx_flows`], 0x2600) and
//! the creation of a multicast flow ([`create_multicast`], 0x2601), so an
//! application (the web controller) can give a transmitter with few flow
//! slots one flow that every receiver shares.
//!
//! **The list** (0x2600): the request body is 24 bytes, `00 01 00 01 00 01`
//! at bytes 6–11, the rest 0 (netaudio's `build_query_tx_flows` for a 2.8.9
//! device). The reply body is `0000 0000 0000`, then `[slots, count]`, then
//! `count` record offsets (from the datagram's first byte, the header
//! included).
//!
//! **A flow record** is a chain of segments. Each segment starts with
//! `[words, words left]`: its own length in 16-bit words, then the words
//! from it to the record's end (the first segment's is the whole record's).
//! Read by their place in the chain:
//!
//! - 0: the flow's global id (bytes 2–3), its media-local id (8–9), its
//!   flags (12–15, `u32`; [`MULTICAST`] set for a multicast flow, a unicast
//!   one reads 0x11) and the bits per sample (22–23).
//! - 2: the destinations: `[slots, count]`, then `count` offsets, each of
//!   an 8-byte descriptor `08 02 <port u16> <IPv4>`.
//! - 3: the channels: their count (2–3), then each TX channel id.
//! - 4: the frames per packet (2–3).
//!
//! A record whose chain does not read (a segment past the record or the
//! datagram, too few segments, a destination offset past the reply) is left
//! out and counted.
//!
//! **The create** (0x2601) is Dante Controller's own, as captured: the
//! native transport, options word 0x0001 (or 0x0071, the AES3 adapter's),
//! no name, a global id of 0 (the device picks it), frames per packet 0
//! (the device's default), no destination (the device picks the group).
//! Only the media-local id and the channels are ours to choose. The reply
//! carries the new flow's record in the list's form, with no destination:
//! the group is read from the list after. A reply with status OK
//! and **no record** made no flow: netaudio's request with options 0 and a
//! name got exactly that from a UltimoX2 (8.11.2026). [`create_multicast`]
//! reads it as [`CreateError::NotCreated`].
//!
//! **The delete** (0x2602, [`delete_multicast`]): a 24-byte body, `0001` at
//! bytes 6–7, the media type `0003` (audio) at 8–9 and the flow's
//! media-local id at 12–13, the rest 0 (netaudio's `build_delete_tx_flow`,
//! segmented). A UltimoX2 (DT_20, 4.2.5.1) answered it with status OK and an
//! empty body, and its next list no longer held the flow (11.11.2026, live,
//! with the owner's OK); Dante Controller's own delete was not captured.
//!
//! Nothing here asks again, and nothing changes but what the caller names
//! on the one device it talks to. Whether a device may be changed at all is
//! the application's call (the owner's rule: no foreign device without the
//! owner).

use std::net::{Ipv4Addr, SocketAddrV4};

use super::client::ReadError;
use super::control::Call;
use crate::wire::{HEADER_LEN, STATUS_OK, read_u16};

/// ARC 2.8.9's start code, which these requests carry (Dante Controller's
/// and netaudio's).
pub const START_2809: u16 = 0x2809;
/// The TX flows' list.
pub const LIST: u16 = 0x2600;
/// A TX flow's creation.
pub const CREATE: u16 = 0x2601;
/// A TX flow's deletion.
pub const DELETE: u16 = 0x2602;
/// A record's flags: a multicast flow.
pub const MULTICAST: u32 = 0x0000_0002;
/// A unicast flow's flags, as a UltimoX2 lists one.
pub const UNICAST: u32 = 0x0000_0011;
/// The most channels one created flow carries (a record's word counts are
/// bytes, and Dante's flows carry at most 8).
pub const MAX_FLOW_CHANNELS: usize = 8;
/// Dante Controller's options words, as captured (the native adapters' and
/// the AES3 adapter's).
pub const OPTIONS: [u16; 2] = [0x0001, 0x0071];

/// One TX flow as the device lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxFlow {
    /// The flow's global id.
    pub id: u16,
    /// Its media-local id (the slot).
    pub local_id: u16,
    pub flags: u32,
    /// The TX channel ids of its channel slots, in order.
    pub channels: Vec<u16>,
    /// Frames per packet.
    pub fpp: u16,
    /// Bits per sample.
    pub bits: u16,
    /// Where it is sent: the group of a multicast flow, the receiver of a
    /// unicast one.
    pub destinations: Vec<SocketAddrV4>,
}

impl TxFlow {
    pub fn multicast(&self) -> bool {
        self.flags & MULTICAST != 0
    }
}

/// The records of a list (or a create) reply.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Flows {
    /// The transmitter's TX flow slots, as the reply's table states
    /// (`[slots, count]`): an UltimoX2 has 2. A controller shows "flows
    /// N/slots" with it (dstack #185).
    pub slots: u16,
    pub flows: Vec<TxFlow>,
    /// The records counted that did not read.
    pub unread: usize,
}

/// Why a create made no flow.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CreateError {
    /// The request was not built: no channel, a channel 0 or twice, more
    /// than [`MAX_FLOW_CHANNELS`], a media-local id 0, an options word that
    /// was never captured.
    #[error("not a request Dante Controller sends: {0}")]
    Invalid(&'static str),
    #[error(transparent)]
    Read(#[from] ReadError),
    /// The device answered OK with no flow record: it made no flow.
    #[error("the device answered without a flow: it made none")]
    NotCreated,
}

/// The list request's body.
pub fn list_body() -> [u8; 24] {
    let mut body = [0u8; 24];
    body[6..12].copy_from_slice(&[0, 1, 0, 1, 0, 1]);
    body
}

/// The records of a 0x2600 or 0x2601 reply body; `None` when its table
/// does not read.
pub fn parse_flows(body: &[u8]) -> Option<Flows> {
    let (&slots, &count) = (body.get(6)?, body.get(7)?);
    let (slots, count) = (usize::from(slots), usize::from(count));
    if count > slots {
        return None;
    }
    let mut out = Flows {
        slots: u16::from(body[6]),
        ..Flows::default()
    };
    for k in 0..count {
        let at = read_u16(body, 8 + 2 * k)?;
        match usize::from(at)
            .checked_sub(HEADER_LEN)
            .and_then(|o| record(body, o))
        {
            Some(f) => out.flows.push(f),
            None => out.unread += 1,
        }
    }
    Some(out)
}

/// The segments of the record at body offset `at`: each one's offset.
fn segments(body: &[u8], at: usize) -> Option<Vec<usize>> {
    let end = at.checked_add(2 * usize::from(*body.get(at + 1)?))?;
    if end > body.len() {
        return None;
    }
    let mut out = Vec::new();
    let mut s = at;
    while s < end {
        let words = usize::from(*body.get(s)?);
        if words == 0 || s + 2 * words > end {
            return None;
        }
        out.push(s);
        s += 2 * words;
    }
    Some(out)
}

/// A 0x2600 reply body listing `flows` (at most 255, and the table states
/// [`super::MAX_FLOWS`] slots, our devices' TX flow slots), in the form
/// [`parse_flows`] reads: the table, then each record's segment chain
/// (0: ids, flags, bits; 1: empty; 2: its destinations; 3: its channels;
/// 4: its frames per packet), then the destinations' descriptors. Our own
/// devices answer the list with it (dstack #148: every receiver reads
/// each transmitter's list once a minute; unanswered, each read waited
/// 1 s and the transmitter logged it).
// The table's slot count is one byte.
const _: () = assert!(super::MAX_FLOWS <= 255);

pub fn list_reply_body(flows: &[TxFlow]) -> Vec<u8> {
    let flows = &flows[..flows.len().min(255)];
    let count = flows.len() as u8;
    let mut out = vec![0u8; 6];
    out.extend([super::MAX_FLOWS as u8, count]);
    let table = out.len();
    out.resize(table + 2 * flows.len(), 0);
    // The descriptors' offsets are known only after every record.
    let mut pending: Vec<(usize, SocketAddrV4)> = Vec::new();
    for (k, f) in flows.iter().enumerate() {
        let at = out.len();
        let at_wire = u16::try_from(HEADER_LEN + at).unwrap_or(u16::MAX);
        out[table + 2 * k..table + 2 * k + 2].copy_from_slice(&at_wire.to_be_bytes());
        let dests = f.destinations.len().min(255);
        let chans = f.channels.len().min(255);
        // Segment lengths in words: 12, 2, 2 + the offsets, 2 + the ids, 2.
        let words = [12, 2, 2 + dests, 2 + chans, 2];
        let total: usize = words.iter().sum();
        let mut left = total;
        let mut seg = |out: &mut Vec<u8>, w: usize| {
            out.push(w as u8);
            out.push(left as u8);
            left -= w;
        };
        seg(&mut out, words[0]);
        out.extend(f.id.to_be_bytes());
        out.extend([0; 4]);
        out.extend(f.local_id.to_be_bytes());
        out.extend([0; 2]);
        out.extend(f.flags.to_be_bytes());
        out.extend([0; 6]);
        out.extend(f.bits.to_be_bytes());
        seg(&mut out, words[1]);
        out.extend([0; 2]);
        seg(&mut out, words[2]);
        out.extend([dests as u8, dests as u8]);
        for d in &f.destinations[..dests] {
            pending.push((out.len(), *d));
            out.extend([0; 2]);
        }
        seg(&mut out, words[3]);
        out.extend((chans as u16).to_be_bytes());
        for c in &f.channels[..chans] {
            out.extend(c.to_be_bytes());
        }
        seg(&mut out, words[4]);
        out.extend(f.fpp.to_be_bytes());
    }
    for (at, d) in pending {
        let here = u16::try_from(HEADER_LEN + out.len()).unwrap_or(u16::MAX);
        out[at..at + 2].copy_from_slice(&here.to_be_bytes());
        out.extend([8, 2]);
        out.extend(d.port().to_be_bytes());
        out.extend(d.ip().octets());
    }
    out
}

/// One flow record at body offset `at`.
fn record(body: &[u8], at: usize) -> Option<TxFlow> {
    let seg = segments(body, at)?;
    // Each field is read inside its own segment, never past it into the
    // next one.
    let part = |i: usize| {
        let s = *seg.get(i)?;
        body.get(s..s + 2 * usize::from(*body.get(s)?))
    };
    let (first, dests, chans, frames) = (part(0)?, part(2)?, part(3)?, part(4)?);
    let flags = u32::from_be_bytes(first.get(12..16)?.try_into().ok()?);
    let n = usize::from(read_u16(chans, 2)?);
    let channels = (0..n)
        .map(|i| read_u16(chans, 4 + 2 * i))
        .collect::<Option<Vec<_>>>()?;
    let count = usize::from(*dests.get(3)?);
    let destinations = (0..count)
        .map(|i| {
            let d = usize::from(read_u16(dests, 4 + 2 * i)?).checked_sub(HEADER_LEN)?;
            if body.get(d..d + 2)? != [8, 2] {
                return None;
            }
            let port = read_u16(body, d + 2)?;
            let ip: [u8; 4] = body.get(d + 4..d + 8)?.try_into().ok()?;
            Some(SocketAddrV4::new(Ipv4Addr::from(ip), port))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(TxFlow {
        id: read_u16(first, 2)?,
        local_id: read_u16(first, 8)?,
        flags,
        channels,
        fpp: read_u16(frames, 2)?,
        bits: read_u16(first, 22)?,
        destinations,
    })
}

/// The device's TX flows (0x2600).
pub fn list_tx_flows(c: &mut impl Call) -> Result<Flows, ReadError> {
    let (status, body) = c.call_as(START_2809, LIST, &list_body())?;
    if status != STATUS_OK {
        return Err(ReadError::Refused { op: LIST, status });
    }
    parse_flows(&body).ok_or(ReadError::Malformed { op: LIST })
}

/// Dante Controller's create body (see the module) for a multicast flow of
/// `channels` in the media-local slot `local_id`, with options word
/// `options`.
pub fn create_body(local_id: u16, channels: &[u16], options: u16) -> Result<Vec<u8>, CreateError> {
    if !OPTIONS.contains(&options) {
        return Err(CreateError::Invalid("an options word never captured"));
    }
    if local_id == 0 {
        return Err(CreateError::Invalid("media-local id 0"));
    }
    if channels.is_empty() || channels.len() > MAX_FLOW_CHANNELS || channels.contains(&0) {
        return Err(CreateError::Invalid("the channels"));
    }
    if (1..channels.len()).any(|i| channels[..i].contains(&channels[i])) {
        return Err(CreateError::Invalid("a channel twice"));
    }
    let c = channels.len();
    // The record: 41 words and one per channel; its segments 22, 10, 4,
    // 3 + c and 2 words long.
    let record_len = 82 + 2 * c;
    let mut body = Vec::with_capacity(10 + record_len);
    body.extend_from_slice(&0u16.to_be_bytes());
    body.extend_from_slice(&options.to_be_bytes());
    body.extend_from_slice(&0u16.to_be_bytes());
    // One record, at packet offset 0x14 (right after this table).
    body.extend_from_slice(&[1, 1, 0x00, 0x14]);
    let r = body.len();
    body.resize(r + record_len, 0);
    let rec = &mut body[r..];
    let words = |n: usize| u8::try_from(n).expect("a record of at most 8 channels");
    rec[0..2].copy_from_slice(&[22, words(41 + c)]);
    rec[6..8].copy_from_slice(&3u16.to_be_bytes());
    rec[8..10].copy_from_slice(&local_id.to_be_bytes());
    rec[12..16].copy_from_slice(&MULTICAST.to_be_bytes());
    rec[44..46].copy_from_slice(&[10, words(19 + c)]);
    // The native transport.
    rec[52..54].copy_from_slice(&1u16.to_be_bytes());
    rec[64..66].copy_from_slice(&[4, words(9 + c)]);
    rec[72..74].copy_from_slice(&[words(3 + c), words(5 + c)]);
    rec[74..76].copy_from_slice(&u16::try_from(c).expect("at most 8").to_be_bytes());
    for (i, ch) in channels.iter().enumerate() {
        rec[76 + 2 * i..78 + 2 * i].copy_from_slice(&ch.to_be_bytes());
    }
    // Frames per packet 0: the device's default.
    rec[78 + 2 * c..80 + 2 * c].copy_from_slice(&[2, 0]);
    Ok(body)
}

/// Creates a multicast flow of `channels` in the media-local slot
/// `local_id` (one the device lists no flow in): the flow the device made,
/// as its reply records it. The reply names no destination (the UltimoX2's
/// and Dante Controller's captured ones): the group the device picked is in
/// the next [`list_tx_flows`].
pub fn create_multicast(
    c: &mut impl Call,
    local_id: u16,
    channels: &[u16],
    options: u16,
) -> Result<TxFlow, CreateError> {
    let body = create_body(local_id, channels, options)?;
    let (status, reply) = c.call_as(START_2809, CREATE, &body)?;
    if status != STATUS_OK {
        return Err(ReadError::Refused { op: CREATE, status }.into());
    }
    let flows = parse_flows(&reply).ok_or(ReadError::Malformed { op: CREATE })?;
    if flows.unread > 0 {
        return Err(ReadError::Malformed { op: CREATE }.into());
    }
    // The flow asked for: its slot and its channels, multicast. A reply
    // naming another flow made none of ours.
    flows
        .flows
        .into_iter()
        .find(|f| f.local_id == local_id && f.channels == channels && f.multicast())
        .ok_or(CreateError::NotCreated)
}

/// The delete request's body for the flow in media-local slot `local_id`
/// (see the module).
pub fn delete_body(local_id: u16) -> [u8; 24] {
    let mut body = [0u8; 24];
    body[6..8].copy_from_slice(&1u16.to_be_bytes());
    body[8..10].copy_from_slice(&3u16.to_be_bytes());
    body[12..14].copy_from_slice(&local_id.to_be_bytes());
    body
}

/// Deletes the TX flow in media-local slot `local_id` (as
/// [`list_tx_flows`] names it, [`TxFlow::local_id`]). Status OK is the
/// device's acceptance; the next [`list_tx_flows`] shows whether the flow
/// is gone.
pub fn delete_multicast(c: &mut impl Call, local_id: u16) -> Result<(), ReadError> {
    let (status, _) = c.call_as(START_2809, DELETE, &delete_body(local_id))?;
    if status != STATUS_OK {
        return Err(ReadError::Refused { op: DELETE, status });
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
