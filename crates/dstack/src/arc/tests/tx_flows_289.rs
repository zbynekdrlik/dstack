// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! Reading another device's ARC 2.8.9 TX flow list (0x2600) in the layout
//! real 2.8.9 devices write (dstack #223): the card's
//! (`tests/fixtures/arc_289_card.json`), the UltimoX2's and the adapters'
//! (`arc/flows/tests.rs`) are all `14 27` records with the flow's name and
//! a format block at offsets. Our own devices keep #148's records
//! ([`flows::list_reply_body`]; the card's would cost dstack receivers up
//! to v0.6.97 our multicast flows), so this file pins the reader:
//! [`flows::parse_flows`] takes the bits from the format block and
//! [`flows::list_tx_flows`] follows a device's pages.
//!
//! The fixtures' records are read with this file's own record reader,
//! never with the code under test.

use super::*;

use crate::arc::client::ReadError;
use crate::arc::control::Call;
use crate::arc::flows::list_tx_flows;
use crate::arc::flows::tests::{ULTIMO_LIST, hex};

const CARD: &str = include_str!("../../../tests/fixtures/arc_289_card.json");

/// The card's captured request (`"requests"`) or reply (`"replies"`) for
/// `op`, the whole datagram.
fn card(kind: &str, op: &str) -> Vec<u8> {
    let fixture: serde_json::Value = serde_json::from_str(CARD).expect("the fixture is JSON");
    hex(fixture[kind][op]
        .as_str()
        .expect("the frame is in the fixture"))
}

fn u32_at(p: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([p[at], p[at + 1], p[at + 2], p[at + 3]])
}

/// One TX flow record of a 0x2600 reply, as this file reads it.
#[derive(Debug, Clone, PartialEq)]
struct TxRecord {
    /// Each segment's first two bytes: `[its words, the words left]`.
    segments: Vec<[u8; 2]>,
    id: u16,
    local_id: u16,
    flags: u32,
    name: String,
    /// First-segment bytes 24-27: the UltimoX2's multicast flows carry its
    /// latency in ns there, a unicast flow 0.
    latency: u32,
    rate: u32,
    bits: u32,
    /// The receiver's device and flow names (a unicast flow).
    receiver: Option<(String, String)>,
    destinations: Vec<SocketAddrV4>,
    channels: Vec<u16>,
    fpp: u16,
}

/// The record at packet offset `at` of the reply `p`: segment 0 (40
/// bytes: id, 0, media 3, media-local id, 0, flags, 0, the name's offset,
/// the format's (rate `u32`, bits `u32`), the latency (`u32`), then
/// zeros), 1 (16 bytes: 0, the
/// receiver's device and flow name offsets, 1, then zeros), 2 (`[slots,
/// count]`, the destination descriptors' offsets), 3 (the slot count, each
/// slot's TX channel), 4 (frames per packet).
fn tx_record(p: &[u8], at: usize) -> TxRecord {
    assert_eq!(at % 4, 0, "a record from a multiple of 4");
    let end = at + 2 * usize::from(p[at + 1]);
    let mut starts = Vec::new();
    let mut s = at;
    while s < end {
        starts.push(s);
        s += 2 * usize::from(p[s]);
    }
    assert_eq!(s, end, "the segments fill the record");
    assert_eq!(starts.len(), 5, "five segments");
    let segments = starts.iter().map(|&s| [p[s], p[s + 1]]).collect();
    let [s0, s1, s2, s3, s4] = starts[..] else {
        unreachable!()
    };
    assert_eq!(p[s0], 0x14, "a 40-byte first segment");
    assert_eq!(u16_at(p, s0 + 6), 3, "audio");
    let format = usize::from(u16_at(p, s0 + 22));
    assert_eq!(p[s0 + 28..s0 + 40], [0; 12]);
    assert_eq!(p[s1], 0x08, "a 16-byte second segment");
    assert_eq!(u16_at(p, s1 + 8), 1);
    let (device, flow) = (u16_at(p, s1 + 4), u16_at(p, s1 + 6));
    let receiver = (device != 0).then(|| (name_at(p, device), name_at(p, flow)));
    let count = usize::from(p[s2 + 3]);
    let destinations = (0..count)
        .map(|k| {
            let d = usize::from(u16_at(p, s2 + 4 + 2 * k));
            assert_eq!(p[d..d + 2], [8, 2], "a socket descriptor");
            let ip = Ipv4Addr::new(p[d + 4], p[d + 5], p[d + 6], p[d + 7]);
            SocketAddrV4::new(ip, u16_at(p, d + 2))
        })
        .collect();
    let n = usize::from(u16_at(p, s3 + 2));
    assert_eq!(usize::from(p[s3]), 3 + n, "count, channels and a zero word");
    TxRecord {
        segments,
        id: u16_at(p, s0 + 2),
        local_id: u16_at(p, s0 + 8),
        flags: u32_at(p, s0 + 12),
        name: name_at(p, u16_at(p, s0 + 20)),
        latency: u32_at(p, s0 + 24),
        rate: u32_at(p, format),
        bits: u32_at(p, format + 4),
        receiver,
        destinations,
        channels: (0..n).map(|k| u16_at(p, s3 + 4 + 2 * k)).collect(),
        fpp: u16_at(p, s4 + 2),
    }
}

/// A 0x2600 reply's `[slots, count]` and its records.
fn tx_list(p: &[u8]) -> (usize, Vec<TxRecord>) {
    assert_eq!(p[HEADER_LEN..HEADER_LEN + 6], [0; 6]);
    let (slots, count) = (usize::from(p[16]), usize::from(p[17]));
    let records = (0..count)
        .map(|k| tx_record(p, usize::from(u16_at(p, 18 + 2 * k))))
        .collect();
    (slots, records)
}

#[test]
fn the_card_s_tx_flows_read_as_the_fixture_states() {
    let (slots, records) = tx_list(&card("replies", "0x2600"));
    assert_eq!(slots, 2);
    let b = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 104), 0x381d);
    let a = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 30), 0xe0bf);
    let unicast = |id: u16, rx: (&str, &str), to, channel| TxRecord {
        segments: vec![
            [0x14, 0x27],
            [0x08, 0x13],
            [0x04, 0x0b],
            [0x05, 0x07],
            [0x02, 0x00],
        ],
        id,
        local_id: id,
        flags: 0x11,
        name: id.to_string(),
        latency: 0,
        rate: 96_000,
        bits: 24,
        receiver: Some((rx.0.to_string(), rx.1.to_string())),
        destinations: vec![to],
        channels: vec![channel, 0],
        fpp: 32,
    };
    assert_eq!(
        records,
        [
            unicast(1, ("receiver-device-b-01234567", "2"), b, 2),
            unicast(2, ("source-device-a", "2_1"), a, 1),
        ]
    );
    // The UltimoX2's multicast flow: flags 2, its latency (1 ms) at bytes
    // 24-27 of the first segment, no receiver, its group.
    let (_, ultimo) = tx_list(&hex(ULTIMO_LIST));
    let m = &ultimo[0];
    assert_eq!((m.flags, m.latency, &m.receiver), (0x02, 1_000_000, &None));
    assert_eq!(m.destinations[0].ip(), &Ipv4Addr::new(239, 255, 0, 1));
}

#[test]
fn the_list_reader_takes_the_bits_from_the_format_block() {
    // The card's list with its format's bits set to 32: the format's offset
    // (0x18) is no bit count.
    let mut list = card("replies", "0x2600");
    list[28..32].copy_from_slice(&32u32.to_be_bytes());
    let read = flows::parse_flows(&list[HEADER_LEN..]).expect("the table reads");
    assert_eq!(read.unread, 0);
    let bits: Vec<u16> = read.flows.iter().map(|f| f.bits).collect();
    assert_eq!(bits, [32, 32]);
}

/// A device that answers every page with one recorded reply body and
/// status, and keeps the first item each request asked from.
struct Stuck {
    status: u16,
    body: Vec<u8>,
    asked: Vec<u16>,
}

impl Call for Stuck {
    fn call(&mut self, op1: u16, body: &[u8]) -> Result<(u16, Vec<u8>), ReadError> {
        self.call_as(0x27ff, op1, body)
    }

    fn call_as(&mut self, _: u16, _: u16, body: &[u8]) -> Result<(u16, Vec<u8>), ReadError> {
        self.asked.push(u16_at(body, 10));
        Ok((self.status, self.body.clone()))
    }
}

#[test]
fn a_list_that_says_more_but_never_moves_on_is_malformed_not_asked_again() {
    // The card's two flows (ids 1 and 2), always with "more follow": the
    // second page names no flow past the one it was asked from (3).
    let list = card("replies", "0x2600");
    let mut stuck = Stuck {
        status: crate::wire::STATUS_MORE,
        body: list[HEADER_LEN..].to_vec(),
        asked: Vec::new(),
    };
    let got = list_tx_flows(&mut stuck);
    assert!(
        matches!(got, Err(ReadError::Malformed { op: 0x2600 })),
        "{got:?}"
    );
    assert_eq!(
        stuck.asked,
        [1, 3],
        "from flow 1, then from flow 3, then no more"
    );
    // A page with no flow that says more: malformed at once.
    let mut empty = Stuck {
        status: crate::wire::STATUS_MORE,
        body: vec![0, 0, 0, 0, 0, 0, 32, 0],
        asked: Vec::new(),
    };
    assert!(list_tx_flows(&mut empty).is_err());
    assert_eq!(empty.asked, [1]);
    // Any status but OK and more: refused.
    let mut refused = Stuck {
        status: crate::wire::STATUS_ERROR,
        body: Vec::new(),
        asked: Vec::new(),
    };
    assert!(matches!(
        list_tx_flows(&mut refused),
        Err(ReadError::Refused { op: 0x2600, .. })
    ));
}

/// A device that answers its pages in turn: `replies[k]` (status, body)
/// to the `k`-th request.
struct Paged {
    replies: Vec<(u16, Vec<u8>)>,
    asked: Vec<u16>,
}

impl Call for Paged {
    fn call(&mut self, op1: u16, body: &[u8]) -> Result<(u16, Vec<u8>), ReadError> {
        self.call_as(0x27ff, op1, body)
    }

    fn call_as(&mut self, _: u16, _: u16, body: &[u8]) -> Result<(u16, Vec<u8>), ReadError> {
        self.asked.push(u16_at(body, 10));
        Ok(self.replies[self.asked.len() - 1].clone())
    }
}

/// The card's list body with its two records' ids (global and
/// media-local) set to `ids`.
fn card_list_of(ids: [u16; 2]) -> Vec<u8> {
    let mut list = card("replies", "0x2600");
    for (record, id) in [0x20usize, 0x98].into_iter().zip(ids) {
        list[record + 2..record + 4].copy_from_slice(&id.to_be_bytes());
        list[record + 8..record + 10].copy_from_slice(&id.to_be_bytes());
    }
    list[HEADER_LEN..].to_vec()
}

#[test]
fn a_page_s_flows_before_its_first_are_not_taken_twice_and_unread_ones_add_up() {
    // Page 1: flows 1 and 2, more follow; page 2 (from 3) lists 2 again
    // and 3.
    let mut dev = Paged {
        replies: vec![
            (crate::wire::STATUS_MORE, card_list_of([1, 2])),
            (STATUS_OK, card_list_of([2, 3])),
        ],
        asked: Vec::new(),
    };
    let read = list_tx_flows(&mut dev).unwrap();
    assert_eq!(dev.asked, [1, 3]);
    let ids: Vec<u16> = read.flows.iter().map(|f| f.local_id).collect();
    assert_eq!(ids, [1, 2, 3], "flow 2 once");
    // A record that does not read (its destination offset past the reply)
    // is counted on every page it is on.
    let mut broken = card_list_of([3, 4]);
    broken[0x5c - HEADER_LEN..0x5e - HEADER_LEN].copy_from_slice(&0x0400u16.to_be_bytes());
    let mut dev = Paged {
        replies: vec![
            (crate::wire::STATUS_MORE, card_list_of([1, 2])),
            (STATUS_OK, broken),
        ],
        asked: Vec::new(),
    };
    let read = list_tx_flows(&mut dev).unwrap();
    let ids: Vec<u16> = read.flows.iter().map(|f| f.local_id).collect();
    assert_eq!((ids, read.unread), (vec![1, 2, 4], 1));
}
