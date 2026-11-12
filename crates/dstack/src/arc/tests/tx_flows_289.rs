// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! Our ARC 2.8.9 TX flow list (0x2600 under 0x2809) in the layout real
//! 2.8.9 devices write (dstack #223): the card's
//! (`tests/fixtures/arc_289_card.json`), the UltimoX2's and the adapters'
//! (`arc/flows/tests.rs`) are all `14 27` records with the flow's name and
//! a format block at offsets. Ours were #148's own chain (a 12-word first
//! segment), which Dante Controller could not read once it read the
//! segmented lists (bit 0x1000): it re-read the device without end.
//!
//! Replies are read with this file's own record reader, never with the
//! code under test; [`flows::list_tx_flows`] is the code under test of the
//! last test (our own receivers' reader, across pages).

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
/// the format's (rate `u32`, bits `u32`), then zeros), 1 (16 bytes: 0, the
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

/// A receiver's request for a flow of our TX `channels` to `rx`, at `fpp`.
fn asked(
    host: &str,
    flow: &str,
    channels: &[Option<u16>],
    rx: SocketAddrV4,
    fpp: u16,
) -> FlowRequest {
    FlowRequest {
        fpp,
        ..tx_request(host, flow, channels, rx)
    }
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
}

/// The card's two flows, made on our device by its receivers' requests
/// (the same receivers, names, slots and frames per packet).
fn cards_flows(dev: &mut Dev) {
    let b = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 104), 0x381d);
    let a = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 30), 0xe0bf);
    let first = asked("receiver-device-b-01234567", "2", &[Some(2), None], b, 32);
    let second = asked("source-device-a", "2_1", &[Some(1), None], a, 32);
    for (slot, req) in [first, second].iter().enumerate() {
        assert_eq!(dev.tx.add(req, 0).unwrap().0, slot);
    }
}

#[test]
fn our_tx_flows_answer_0x2600_in_the_card_s_layout() {
    let (_, card_records) = tx_list(&card("replies", "0x2600"));
    let mut dev = Dev::new(config());
    cards_flows(&mut dev);
    let reply = sent_bytes(dev.send(&card("requests", "0x2600")));
    assert_eq!(u16_at(&reply, 8), STATUS_OK);
    assert!(reply.len() <= MAX_DATAGRAM);
    let (slots, records) = tx_list(&reply);
    // Our slots: the 32 TX flows our 0x1000 states (dstack #190 reads them
    // as the device's flow slots).
    assert_eq!(slots, usize::from(MAX_FLOWS));
    assert_eq!(records, card_records, "the card's records, field by field");
}

#[test]
fn our_multicast_flow_answers_0x2600_as_the_ultimo_x2_lists_one() {
    // The UltimoX2's multicast flow (flags 2): no receiver, its group.
    let ultimo = hex(ULTIMO_LIST);
    let (_, theirs) = tx_list(&ultimo);
    let their = &theirs[0];
    assert_eq!((their.flags, &their.receiver), (0x02, &None));
    let mut dev = Dev::new(config());
    let group = dev
        .tx
        .add_own_multicast(&dev.cfg, OUR_IP, 4, &[1, 2])
        .unwrap();
    let reply = sent_bytes(dev.send(&card("requests", "0x2600")));
    let (_, ours) = tx_list(&reply);
    assert_eq!(
        ours,
        [TxRecord {
            segments: their.segments.clone(),
            id: 5,
            local_id: 5,
            flags: 0x02,
            name: "5".into(),
            rate: dev.cfg.sample_rate,
            bits: u32::from(dev.cfg.encoding),
            receiver: None,
            destinations: vec![group],
            channels: vec![1, 2],
            fpp: dev.cfg.tx_fpp_max,
        }]
    );
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

/// Our own device behind [`Call`]: every request answered by [`handle`],
/// each reply at most one datagram.
struct Ours<'a> {
    dev: &'a mut Dev,
    pages: usize,
}

impl Call for Ours<'_> {
    fn call(&mut self, op1: u16, body: &[u8]) -> Result<(u16, Vec<u8>), ReadError> {
        self.call_as(0x27ff, op1, body)
    }

    fn call_as(&mut self, start: u16, op1: u16, body: &[u8]) -> Result<(u16, Vec<u8>), ReadError> {
        self.pages += 1;
        let reply = sent_bytes(self.dev.send(&request(start, 1, op1, 0, body)));
        assert!(reply.len() <= MAX_DATAGRAM, "{} bytes", reply.len());
        Ok((u16_at(&reply, 8), reply[HEADER_LEN..].to_vec()))
    }
}

#[test]
fn our_receivers_read_every_flow_of_a_full_list_page_by_page() {
    // 32 unicast flows with names of the longest kind: more than one
    // datagram holds.
    let mut dev = Dev::new(config());
    for k in 0..u16::from(32u8) {
        let rx = SocketAddrV4::new(PEER_IP, 40_000 + k);
        let host = format!("receiver-{k:02}-{}", "x".repeat(19));
        let req = asked(&host, &format!("flow-{k:02}"), &[Some(1 + k % 16)], rx, 16);
        assert_eq!(dev.tx.add(&req, 0).unwrap().0, usize::from(k));
    }
    let mut ours = Ours {
        dev: &mut dev,
        pages: 0,
    };
    let read = list_tx_flows(&mut ours).expect("the whole list reads");
    assert!(ours.pages > 1, "several pages");
    assert_eq!(read.unread, 0);
    let ids: Vec<u16> = read.flows.iter().map(|f| f.id).collect();
    assert_eq!(ids, (1..=32).collect::<Vec<u16>>(), "each flow once");
    assert!(read.flows.iter().all(|f| f.bits == 24 && !f.multicast()));
}
