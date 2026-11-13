// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.
// Protocol facts (byte layout, opcode, paging) checked against a Yamaha
// AIC128-D (Audinate DCM, ARC 2.7.41) answering live (dstack #271) and
// against inferno's `query_tx_flows`; the code is our own.

//! Another device's TX flows over ARC 2.7 (0x2200, start code 0x27ff,
//! dstack #271): the list a device that does not know 2.8.9's 0x2600
//! answers. [`super::read_tx_flows`] asks it when 0x2600 is refused with
//! [`super::STATUS_UNKNOWN_OP`].
//!
//! **The request** is the usual page request ([`page_request`]: `00 01`,
//! the first flow, `00 00`); a 4-byte or empty body is refused (status
//! 0x0022, the card).
//!
//! **The reply body** is `[slots, count]` (the page's slots, 16), then
//! `count` record offsets (from the datagram's first byte, the header
//! included). A record: the flow id, its type (0x0011 unicast, 0x0002
//! multicast), the rate (`u32`), 0, the encoding (bits per sample), the
//! count N of socket descriptor offsets, the channel slots S, the N
//! socket offsets (0 for an unused one), the S slots' TX channels (0 for
//! an empty slot), the names descriptor's offset. A socket descriptor is
//! `08 02 <port u16> <IPv4>` (the card's; our own writer, `arc::tx_flows`,
//! writes `80 02`, inferno's, and is read too). The names descriptor's word
//! at bytes 8–9 is the flow's frames per packet: the card's read 4, 16 and
//! 32 across its flows, each a value a Dante receiver asks for (evidence,
//! not proof); 0 when the record has no names descriptor.
//!
//! **Pages:** the request's first item is a flow id (a slot, from 1: our
//! own 0x2200 passes over empty slots without counting them, and leaves an
//! item that did not fit uncounted). A page that says more follow
//! ([`STATUS_MORE`]) is followed by the page from the flow after its
//! highest id; its flows below the one asked from are left out (already
//! read). One that says more follow but names no flow past the one asked
//! from is malformed. A page of status OK ends the list. The channel
//! lists' pager ([`client`](super::super::client)'s `read_list`) is not
//! used: it counts items rather than ids, needs the total up front and
//! counts no unread record.
//!
//! A record that does not read (a field past the reply, a socket offset
//! that is not a descriptor) is left out and counted ([`Flows::unread`]).

use std::net::{Ipv4Addr, SocketAddrV4};

use super::super::client::{MAX_PAGES, ReadError, START, page_request, table};
use super::super::control::Call;
use super::{Flows, TxFlow};
use crate::wire::{HEADER_LEN, STATUS_MORE, STATUS_OK, read_u16};

/// ARC 2.7's TX flow list.
pub const LIST_27: u16 = super::super::op::TX_FLOWS;

/// The socket descriptor tags read: the card's (`08 02`) and inferno's,
/// which our own 0x2200 writes (`80 02`).
const SOCKET_TAGS: [[u8; 2]; 2] = [[0x08, 0x02], [0x80, 0x02]];

/// One page of a 0x2200 reply body: its count and the records that read;
/// `None` when its table does not read.
fn page(body: &[u8]) -> Option<(usize, Vec<TxFlow>)> {
    let (_, count) = table(body, 2)?;
    let flows = (0..count)
        .filter_map(|k| {
            let at = usize::from(read_u16(body, 2 + 2 * k)?).checked_sub(HEADER_LEN)?;
            record(body, at)
        })
        .collect();
    Some((count, flows))
}

/// The record at body offset `at` (see the module).
fn record(body: &[u8], at: usize) -> Option<TxFlow> {
    let r = body.get(at..)?;
    let word = |i: usize| read_u16(r, 2 * i);
    let id = word(0)?;
    let kind = word(1)?;
    let encoding = word(5)?;
    let sockets = usize::from(word(6)?);
    let slots = usize::from(word(7)?);
    let mut destinations = Vec::new();
    for i in 0..sockets {
        let o = word(8 + i)?;
        if o != 0 {
            destinations.push(socket(body, o)?);
        }
    }
    let channels = (0..slots)
        .map(|i| word(8 + sockets + i))
        .collect::<Option<Vec<_>>>()?;
    let names = word(8 + sockets + slots)?;
    let fpp = match names {
        0 => 0,
        n => read_u16(body, usize::from(n).checked_sub(HEADER_LEN)? + 8)?,
    };
    Some(TxFlow {
        id,
        local_id: id,
        flags: u32::from(kind),
        channels,
        fpp,
        bits: encoding,
        destinations,
    })
}

/// The socket descriptor at packet offset `at`.
fn socket(body: &[u8], at: u16) -> Option<SocketAddrV4> {
    let d = usize::from(at).checked_sub(HEADER_LEN)?;
    let tag: [u8; 2] = body.get(d..d + 2)?.try_into().ok()?;
    if !SOCKET_TAGS.contains(&tag) {
        return None;
    }
    let port = read_u16(body, d + 2)?;
    let ip: [u8; 4] = body.get(d + 4..d + 8)?.try_into().ok()?;
    Some(SocketAddrV4::new(Ipv4Addr::from(ip), port))
}

/// The device's TX flows (0x2200 under start code 0x27ff), page by page
/// (see the module), at most [`MAX_PAGES`] pages. [`Flows::slots`] is 0:
/// a 2.7 page's table states the page's slots, not the device's flow
/// slots.
pub fn list_tx_flows_27(c: &mut impl Call) -> Result<Flows, ReadError> {
    let mut out = Flows::default();
    let mut first: u16 = 1;
    for _ in 0..MAX_PAGES {
        let request = page_request(0, LIST_27, first);
        let (status, body) = c.call_as(START, LIST_27, &request[HEADER_LEN..])?;
        if status != STATUS_OK && status != STATUS_MORE {
            return Err(ReadError::Refused {
                op: LIST_27,
                status,
            });
        }
        let (count, mut flows) = page(&body).ok_or(ReadError::Malformed { op: LIST_27 })?;
        out.unread += count - flows.len();
        flows.retain(|f| f.id >= first);
        let last = flows.iter().map(|f| f.id).max();
        out.flows.extend(flows);
        if status == STATUS_OK {
            return Ok(out);
        }
        match last
            .and_then(|l| l.checked_add(1))
            .filter(|&next| next > first)
        {
            Some(next) => first = next,
            None => break,
        }
    }
    Err(ReadError::Malformed { op: LIST_27 })
}
