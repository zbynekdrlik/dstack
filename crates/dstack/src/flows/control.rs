// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted from inferno (https://github.com/teodly/inferno at 9767558):
// inferno_aoip/src/protocol/flows_control.rs (the port, the start code, the
// statuses, the handle, the request as its client writes it, the update and
// the stop) and inferno_aoip/src/device_server/flows_control_server.rs (the
// request's, the stop's and the update's parse), Copyright (C) Teo and the
// inferno contributors, GPL-3.0-or-later OR AGPL-3.0-or-later, taken under
// GPL-3.0-or-later. Changes Copyright (C) 2026 iemmixer contributors.

//! The datagrams of flow control on UDP [`PORT`]: the request/response
//! header of [`crate::wire`] with the start code [`START`], then the body.
//!
//! A flow request ([`op::REQUEST`]), its body as inferno's client writes
//! it (offsets are packet offsets, the 10-byte header counted; `n` is the
//! flow's channel slots):
//!
//! | body bytes | field |
//! |---|---|
//! | 0–1 | the strings' offset: `0x26 + 2n + 10`, right after the fixed part |
//! | 2–5 | sample rate, Hz |
//! | 6–9 | bits per sample |
//! | 10–11 | 1 |
//! | 12–13 | `n` |
//! | 14–15 | the receiver's descriptor's offset |
//! | 16… | `n` TX channel ids, 1 for the first channel, 0 for an empty slot |
//! | then | `0x1c + 2n`, `0x0a00`, `0x0002` |
//! | then | frames per packet (fpp) |
//! | then | the flow name's offset |
//! | then | 12 zero bytes |
//! | strings | the receiver's device name and NUL, the flow name and NUL, zeros up to an offset that is a multiple of 8 |
//! | descriptor | `0x0802`, the receiver's UDP port, its IPv4 address |
//!
//! The answer to a request is status [`status::OK`] and the flow's
//! [`Handle`] (6 bytes) or a status alone; the stop ([`op::STOP`], the
//! handle) and the update ([`op::UPDATE`], the handle, `n` and `n` channel
//! ids) are answered by a status alone.
//!
//! Parsing never panics and never reads past the datagram: what inferno's
//! server cannot read (it logs, or its index panics) is [`Unread`].

use std::net::{Ipv4Addr, SocketAddrV4};

use crate::wire::{self, Body, HEADER_LEN, read_u16};

/// Flow control's UDP port.
pub const PORT: u16 = 4455;
/// The start code of flow control (inferno: "dbcp1"; its client always
/// sends it, its server never checks it, a reply echoes the request's).
pub const START: u16 = 0x1102;

/// The opcodes (op1).
pub mod op {
    /// A receiver asks for a flow.
    pub const REQUEST: u16 = 0x0100;
    /// A receiver ends its flow.
    pub const STOP: u16 = 0x0101;
    /// A receiver sets its flow's channels.
    pub const UPDATE: u16 = 0x0102;
}

/// The statuses of a reply (op2).
pub mod status {
    /// Done.
    pub const OK: u16 = 0x0001;
    /// No flow has the handle (inferno: also a flow that expired and was
    /// removed).
    pub const FLOW_NOT_FOUND: u16 = 0x0103;
    /// The request's sample rate is not the transmitter's.
    pub const SAMPLE_RATE_MISMATCH: u16 = 0x0301;
    /// A request the transmitter cannot carry out (inferno: a channel id
    /// past its channels, more than 256 frames per packet).
    pub const BAD_REQUEST: u16 = 0x0302;
    /// No free flow.
    pub const TOO_MANY_TX_FLOWS: u16 = 0x0315;
    /// A refusal of the flow's parameters with a free flow: a 2-channel
    /// wireless mic answered it to fpp 4 (iemmixer's capture, 31.10.2026,
    /// dstack #1). Read as "fpp too small": the receiver asks larger.
    pub const FPP_REFUSED: u16 = 0x0314;
}

/// A flow's handle: the transmitter's slot (`u32`) and a cookie (`u16`),
/// big-endian.
pub type Handle = [u8; 6];

/// The handle of `slot` with `cookie`.
pub fn handle(slot: u32, cookie: u16) -> Handle {
    let mut h = [0u8; 6];
    h[..4].copy_from_slice(&slot.to_be_bytes());
    h[4..].copy_from_slice(&cookie.to_be_bytes());
    h
}

/// A handle's slot and cookie.
pub fn split(h: &Handle) -> (u32, u16) {
    (
        u32::from_be_bytes([h[0], h[1], h[2], h[3]]),
        u16::from_be_bytes([h[4], h[5]]),
    )
}

/// A flow request: what a receiver asks a transmitter for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowRequest {
    /// The receiving device's name.
    pub rx_host: String,
    /// The receiver's name for the flow.
    pub rx_flow_name: String,
    pub sample_rate: u32,
    pub bits: u32,
    /// The flow's slots in order: the transmitter's channel number (1 for
    /// its first channel) or `None` for an empty slot (id 0).
    pub channels: Vec<Option<u16>>,
    /// Frames per packet.
    pub fpp: u16,
    /// Where the audio goes: the receiver's address and port.
    pub rx: SocketAddrV4,
}

/// Why a datagram's body was not read: inferno's server answers none of
/// these (it logs, or its index or `unwrap` panics).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Unread {
    #[error("a request body of 16 bytes or less")]
    Short,
    #[error("the channel ids or the fields after them run past the end")]
    Cut,
    #[error("the device or the flow name is missing or not UTF-8")]
    NoString,
    #[error("the receiver's descriptor runs past the end")]
    NoDescriptor,
    #[error("a stop's body is not exactly the 6-byte handle")]
    NotAHandle,
}

/// The receiver descriptor's first two bytes (never checked on a read).
const DESCRIPTOR_TAG: u16 = 0x0802;

/// Where each part of a flow request sits, as packet offsets (the 10-byte
/// header counted), for `n` channel slots and names of the given lengths.
/// The fixed fields come first (body 0–15), then the ids, a 22-byte middle
/// (three constants, fpp, the flow name's offset, 12 zero bytes), the two
/// names, padding to a multiple of 8 and the 8-byte descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    /// The first channel id.
    ids: usize,
    /// The middle part, right after the last id.
    middle: usize,
    /// The receiver's device name.
    host: usize,
    /// The flow's name.
    flow: usize,
    /// The receiver's descriptor.
    descriptor: usize,
    /// One past the datagram's last byte.
    end: usize,
}

impl Layout {
    /// The middle part's length: three `u16` constants, fpp, the flow
    /// name's offset, then 12 zero bytes.
    const MIDDLE: usize = 2 * 5 + 12;

    fn of(n: usize, host_len: usize, flow_len: usize) -> Layout {
        let ids = HEADER_LEN + 16;
        let middle = ids + 2 * n;
        let host = middle + Self::MIDDLE;
        let flow = host + host_len + 1;
        let descriptor = (flow + flow_len + 1).next_multiple_of(8);
        Layout {
            ids,
            middle,
            host,
            flow,
            descriptor,
            end: descriptor + 8,
        }
    }
}

/// A flow request datagram (the layout a Dante receiver sends).
///
/// # Panics
///
/// When the request is longer than a datagram can say (names of tens of
/// KiB); ours are bounded far below that (names of at most 31 bytes,
/// at most 8 channels).
pub fn build_request(seq: u16, req: &FlowRequest) -> Vec<u8> {
    let n = req.channels.len();
    let at = Layout::of(n, req.rx_host.len(), req.rx_flow_name.len());
    let word = |v: usize| -> [u8; 2] {
        u16::try_from(v)
            .expect("a request under 64 KiB")
            .to_be_bytes()
    };
    let mut d = vec![0u8; at.end];
    let mut put = |pos: usize, bytes: &[u8]| d[pos..pos + bytes.len()].copy_from_slice(bytes);
    let fixed = HEADER_LEN;
    put(fixed, &word(at.host));
    put(fixed + 2, &req.sample_rate.to_be_bytes());
    put(fixed + 6, &req.bits.to_be_bytes());
    put(fixed + 10, &1u16.to_be_bytes());
    put(fixed + 12, &word(n));
    put(fixed + 14, &word(at.descriptor));
    for (i, ch) in req.channels.iter().enumerate() {
        put(at.ids + 2 * i, &ch.unwrap_or(0).to_be_bytes());
    }
    put(at.middle, &word(0x1c + 2 * n));
    put(at.middle + 2, &0x0a00u16.to_be_bytes());
    put(at.middle + 4, &0x0002u16.to_be_bytes());
    put(at.middle + 6, &req.fpp.to_be_bytes());
    put(at.middle + 8, &word(at.flow));
    put(at.host, req.rx_host.as_bytes());
    put(at.flow, req.rx_flow_name.as_bytes());
    put(at.descriptor, &DESCRIPTOR_TAG.to_be_bytes());
    put(at.descriptor + 2, &req.rx.port().to_be_bytes());
    put(at.descriptor + 4, &req.rx.ip().octets());
    wire::request(START, seq, op::REQUEST, 0, &d[HEADER_LEN..])
}

/// Reads a flow request from the whole datagram (its header included:
/// the body's offsets are packet offsets). Only what is needed to read it
/// is checked; the values are checked by [`super::server`].
pub fn parse_request(datagram: &[u8]) -> Result<FlowRequest, Unread> {
    // A body of 16 bytes or less carries no channel at all.
    if datagram.len() <= HEADER_LEN + 16 {
        return Err(Unread::Short);
    }
    let u16_at = |pos: usize| read_u16(datagram, pos);
    let u32_at = |pos: usize| {
        datagram
            .get(pos..pos + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    let n = usize::from(u16_at(HEADER_LEN + 12).ok_or(Unread::Short)?);
    // Names have no length here, so the layout gives the ids and middle.
    let at = Layout::of(n, 0, 0);
    let fpp = u16_at(at.middle + 6).ok_or(Unread::Cut)?;
    let flow_name_at = u16_at(at.middle + 8).ok_or(Unread::Cut)?;
    let channels = (0..n)
        .map(|i| u16_at(at.ids + 2 * i).map(|id| (id != 0).then_some(id)))
        .collect::<Option<Vec<_>>>()
        .ok_or(Unread::Cut)?;
    let host_at = u16_at(HEADER_LEN).ok_or(Unread::Short)?;
    let rx_host = cstr(datagram, host_at).ok_or(Unread::NoString)?;
    let rx_flow_name = cstr(datagram, flow_name_at).ok_or(Unread::NoString)?;
    let descriptor_at = usize::from(u16_at(HEADER_LEN + 14).ok_or(Unread::Short)?);
    let port = u16_at(descriptor_at + 2);
    let ip = datagram.get(descriptor_at + 4..descriptor_at + 8);
    let (Some(port), Some(ip)) = (port, ip) else {
        return Err(Unread::NoDescriptor);
    };
    Ok(FlowRequest {
        rx_host: rx_host.to_string(),
        rx_flow_name: rx_flow_name.to_string(),
        sample_rate: u32_at(HEADER_LEN + 2).ok_or(Unread::Short)?,
        bits: u32_at(HEADER_LEN + 6).ok_or(Unread::Short)?,
        channels,
        fpp,
        rx: SocketAddrV4::new(Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3]), port),
    })
}

/// A stop datagram.
pub fn build_stop(seq: u16, h: &Handle) -> Vec<u8> {
    wire::request(START, seq, op::STOP, 0, h)
}

/// A stop's handle: the body must be exactly the 6 bytes (inferno's
/// `try_into`).
pub fn parse_stop(body: &[u8]) -> Result<Handle, Unread> {
    body.try_into().map_err(|_| Unread::NotAHandle)
}

/// An update datagram: the handle, the slot count and the channel ids.
///
/// # Panics
///
/// Never for a flow's channels (at most a few hundred slots); as
/// [`wire::request`] for a body of 64 KiB.
pub fn build_update(seq: u16, h: &Handle, channels: &[Option<u16>]) -> Vec<u8> {
    let mut b = Body::new();
    for &byte in h {
        b.u8(byte);
    }
    b.u16(u16::try_from(channels.len()).expect("a flow's channels"));
    for ch in channels {
        b.u16(ch.unwrap_or(0));
    }
    wire::request(START, seq, op::UPDATE, 0, &b.into_vec())
}

/// An update's handle and channels; bytes after the channel ids are
/// ignored (as inferno reads them).
pub fn parse_update(body: &[u8]) -> Result<(Handle, Vec<Option<u16>>), Unread> {
    let h: Handle = body
        .get(..6)
        .and_then(|h| h.try_into().ok())
        .ok_or(Unread::Cut)?;
    let n = usize::from(read_u16(body, 6).ok_or(Unread::Cut)?);
    if body.len() < 8 + 2 * n {
        return Err(Unread::Cut);
    }
    let channels = (0..n)
        .map(|i| match field(body, 8 + 2 * i) {
            0 => None,
            id => Some(id),
        })
        .collect();
    Ok((h, channels))
}

/// The handle in an OK reply's body: its first 6 bytes (inferno's client
/// refuses a shorter body).
pub fn reply_handle(body: &[u8]) -> Option<Handle> {
    body.get(..6)?.try_into().ok()
}

/// The big-endian `u16` at `at`, which the caller checked is inside `c`.
fn field(c: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([c[at], c[at + 1]])
}

/// The string at packet offset `offset` of the whole datagram: up to its
/// NUL or the datagram's end, as inferno's `read_0term_str_from_buffer`
/// reads it (an offset inside the header too); `None` past the end or for
/// bytes that are not UTF-8.
fn cstr(datagram: &[u8], offset: u16) -> Option<&str> {
    let rest = datagram
        .get(usize::from(offset)..)
        .filter(|r| !r.is_empty())?;
    let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    std::str::from_utf8(&rest[..end]).ok()
}

#[cfg(test)]
mod tests;
