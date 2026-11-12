// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! ARC 2.8.9 (start code 0x2809, dstack #216) against a real card's
//! exchange with Dante Controller (`tests/fixtures/arc_289_card.json`).
//! Dante Controller picks the protocol from a device's ARC TXT: to a
//! device that announces 2.8.9 it sends these reads when it starts, and
//! only such a device gets an editable Rename. Our devices announced 2.7
//! and left 0x2400, 0x3400 and 0x3600 unanswered.
//!
//! Each record is read here with this file's own parser, the same for the
//! card's replies and ours, never with the code under test.

use super::*;

use crate::arc::flows::tests::hex;

const CARD: &str = include_str!("../../../tests/fixtures/arc_289_card.json");

/// ARC 2.8.9's start code, as Dante Controller sends it.
const START_2809: u16 = 0x2809;
/// The start code of both 2.8.9 devices' 0x2204 reply.
const START_2801: u16 = 0x2801;
/// The media type of an audio channel or flow.
const AUDIO: u16 = 3;

fn fixture() -> serde_json::Value {
    serde_json::from_str(CARD).expect("the fixture is JSON")
}

/// The card's captured request (`"requests"`) or reply (`"replies"`) for
/// `op`, the whole datagram.
fn card(kind: &str, op: &str) -> Vec<u8> {
    hex(fixture()[kind][op]
        .as_str()
        .expect("the frame is in the fixture"))
}

/// The second 2.8.9 device's reply to `op` (0 TX, 2 RX channels).
fn second(op: &str) -> Vec<u8> {
    hex(fixture()["second_device"][op]
        .as_str()
        .expect("the frame is in the fixture"))
}

/// Our device's reply to the card's captured request `op`.
fn ours(dev: &mut Dev, op: &str) -> Vec<u8> {
    sent_bytes(dev.send(&card("requests", op)))
}

/// The card's request `op` with packet bytes `at..` set to `words`.
fn asked(op: &str, at: usize, words: &[u16]) -> Vec<u8> {
    let mut req = card("requests", op);
    for (k, w) in words.iter().enumerate() {
        req[at + 2 * k..at + 2 * k + 2].copy_from_slice(&w.to_be_bytes());
    }
    req
}

fn u32_at(p: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([p[at], p[at + 1], p[at + 2], p[at + 3]])
}

/// The header's start code, sequence, opcode and status.
fn head(p: &[u8]) -> (u16, u16, u16, u16) {
    assert_eq!(usize::from(u16_at(p, 2)), p.len(), "the length field");
    (u16_at(p, 0), u16_at(p, 4), u16_at(p, 6), u16_at(p, 8))
}

/// A 2.8.9 page: 6 zero bytes, `[slots, count]`, `slots` record offsets
/// (only `count` used), then what they point at.
struct Page289 {
    status: u16,
    slots: usize,
    records: Vec<usize>,
}

fn page_289(p: &[u8]) -> Page289 {
    assert!(p.len() <= MAX_DATAGRAM);
    assert_eq!(p[10..16], [0; 6]);
    let (slots, count) = (usize::from(p[16]), usize::from(p[17]));
    assert!(count <= slots);
    let table_end = 18 + 2 * slots;
    let records: Vec<usize> = (0..count)
        .map(|k| usize::from(u16_at(p, 18 + 2 * k)))
        .collect();
    for &r in &records {
        assert!(r >= table_end && r % 4 == 0, "a record at {r}");
    }
    Page289 {
        status: u16_at(p, 8),
        slots,
        records,
    }
}

/// One channel record of 0x2400 (TX) or 0x3400 (RX).
#[derive(Debug, PartialEq)]
struct Channel {
    number: u16,
    flags: u32,
    name: String,
    default: String,
    format: Vec<u8>,
    /// RX: the TX channel and device, when subscribed.
    sub: Option<(String, String)>,
    status: u16,
    detail: u16,
}

fn channel(p: &[u8], at: usize, rx: bool) -> Channel {
    let (head, len) = if rx {
        ([0x14, 0x1c], 56)
    } else {
        ([0x14, 0x14], 40)
    };
    assert!(at + len <= p.len());
    assert_eq!(p[at..at + 2], head, "the record's segment");
    let number = u16_at(p, at + 2);
    assert_eq!(u16_at(p, at + 4), 0);
    assert_eq!(u16_at(p, at + 6), AUDIO);
    assert_eq!(u16_at(p, at + 8), number, "the media-local id");
    assert_eq!(u16_at(p, at + 10), 0);
    assert_eq!(u32_at(p, at + 16), 0);
    assert_eq!(p[at + 24..at + 30], [0; 6]);
    assert_eq!(p[at + 32..at + 40], [0; 8]);
    let format = usize::from(u16_at(p, at + 22));
    assert_eq!(
        format % 4,
        0,
        "the format at a multiple of 4, as the card's"
    );
    let (mut sub, mut status, mut detail) = (None, 0, 0);
    if rx {
        assert_eq!(p[at + 40..at + 44], [0x06, 0x08, 0, 0]);
        assert_eq!(p[at + 52..at + 56], [0x02, 0x02, 0, 0]);
        let (c, d) = (u16_at(p, at + 44), u16_at(p, at + 46));
        if c != 0 || d != 0 {
            sub = Some((name_at(p, c), name_at(p, d)));
        }
        status = u16_at(p, at + 48);
        detail = u16_at(p, at + 50);
    }
    Channel {
        number,
        flags: u32_at(p, at + 12),
        name: name_at(p, u16_at(p, at + 20)),
        default: name_at(p, u16_at(p, at + 30)),
        format: p[format..format + 16].to_vec(),
        sub,
        status,
        detail,
    }
}

/// Every channel record of a 0x2400 or 0x3400 page.
fn channels(p: &[u8], rx: bool) -> Vec<Channel> {
    page_289(p)
        .records
        .iter()
        .map(|&at| channel(p, at, rx))
        .collect()
}

/// One RX flow record of 0x3600.
#[derive(Debug, PartialEq)]
struct RxFlow {
    number: u16,
    flags: u32,
    name: String,
    rate: u32,
    bits: u32,
    latency_ns: u32,
    slots: u16,
    endpoint: SocketAddrV4,
    /// The first RX channel, then each one's feeding slot (0: none).
    mapping: (u16, Vec<u8>),
}

fn rx_flow(p: &[u8], at: usize) -> RxFlow {
    assert_eq!(p[at..at + 2], [0x14, 0x22], "segment 0");
    let number = u16_at(p, at + 2);
    assert_eq!(u16_at(p, at + 4), 0);
    assert_eq!(u16_at(p, at + 6), AUDIO);
    assert_eq!(u16_at(p, at + 8), number, "the media-local id");
    assert_eq!(u16_at(p, at + 10), 0);
    assert_eq!(u32_at(p, at + 16), 0);
    assert_eq!(p[at + 28..at + 40], [0; 12]);
    assert_eq!(p[at + 40..at + 42], [0x0a, 0x0e], "segment 1");
    assert_eq!(p[at + 42..at + 52], [0; 10]);
    assert_eq!(p[at + 56..at + 60], [0; 4]);
    assert_eq!(p[at + 60..at + 62], [0x04, 0x00], "segment 2");
    assert_eq!(u16_at(p, at + 62), 0x0101);
    assert_eq!(u16_at(p, at + 66), 0);
    // The endpoint and the mapping follow the record (netaudio reads them
    // at these places).
    let ep = usize::from(u16_at(p, at + 64));
    assert_eq!(ep, at + 68);
    assert_eq!(p[ep..ep + 2], [0x08, 0x02]);
    let ip: [u8; 4] = p[ep + 4..ep + 8].try_into().unwrap();
    let map = usize::from(u16_at(p, at + 54));
    assert_eq!(map, at + 76);
    let span = usize::from(u16_at(p, map + 2));
    assert!(span >= 2 && span % 2 == 0, "span {span}");
    assert_eq!(u16_at(p, map + 4), 0, "no next node");
    assert!(map + 6 + span <= p.len(), "the mapping inside the reply");
    let format = usize::from(u16_at(p, at + 22));
    assert_eq!(
        format % 4,
        0,
        "the format at a multiple of 4, as the card's"
    );
    assert!(format + 8 <= at, "the format before the record");
    RxFlow {
        number,
        flags: u32_at(p, at + 12),
        name: name_at(p, u16_at(p, at + 20)),
        rate: u32_at(p, format),
        bits: u32_at(p, format + 4),
        latency_ns: u32_at(p, at + 24),
        slots: u16_at(p, at + 52),
        endpoint: SocketAddrV4::new(Ipv4Addr::from(ip), u16_at(p, ep + 2)),
        mapping: (u16_at(p, map), p[map + 6..map + 6 + span].to_vec()),
    }
}

/// The 16-byte format our channels state (2.7's common descriptor).
fn our_format(cfg: &DeviceConfig) -> Vec<u8> {
    let bits = u16::from(cfg.encoding).to_be_bytes();
    let mut want = cfg.sample_rate.to_be_bytes().to_vec();
    want.extend_from_slice(&[1, 1, bits[0], bits[1], 0x04, 0x00]);
    want.extend_from_slice(&[bits[0], bits[1], bits[0], bits[1], 0x00, 0x0e]);
    want
}

#[test]
fn the_card_s_replies_read_as_the_fixture_states() {
    // The card: 2 TX (TX 2 renamed), 2 RX, both subscribed and receiving.
    let tx = channels(&card("replies", "0x2400"), false);
    assert_eq!(tx.len(), 2);
    assert_eq!(
        (tx[0].number, tx[0].name.as_str(), tx[0].default.as_str()),
        (1, "Default name 001", "Default name 001")
    );
    assert_eq!(
        (tx[1].number, tx[1].name.as_str(), tx[1].default.as_str()),
        (2, "Custom 2", "Default name 0002")
    );
    assert_eq!(u32_at(&tx[0].format, 0), 96_000);
    let rx = channels(&card("replies", "0x3400"), true);
    assert_eq!(rx.len(), 2);
    for (c, src) in rx.iter().zip(["Out 1L", "Out 1R"]) {
        assert_eq!(
            c.sub,
            Some((src.to_string(), "source-device-a".to_string()))
        );
        assert_eq!((c.status, c.detail), (9, 0x0101), "receiving");
    }
    let flows = card("replies", "0x3600");
    let page = page_289(&flows);
    assert_eq!((page.slots, page.records.len()), (2, 1));
    let f = rx_flow(&flows, page.records[0]);
    assert_eq!(
        (
            f.number,
            f.name.as_str(),
            f.rate,
            f.bits,
            f.latency_ns,
            f.slots
        ),
        (1, "1", 96_000, 24, 5_000_000, 2)
    );
    assert_eq!(f.endpoint.ip(), &Ipv4Addr::new(192, 0, 2, 103));
    assert_eq!(f.mapping, (1, vec![1, 2]), "RX 1 from slot 1, RX 2 from 2");
    // The TX flow list reads with our 2.8.9 list reader (dstack #148).
    let list = card("replies", "0x2600");
    let read = flows::parse_flows(&list[HEADER_LEN..]).expect("the table reads");
    assert_eq!((read.slots, read.flows.len(), read.unread), (2, 2, 0));
    assert_eq!(read.flows[0].fpp, 32);
}

#[test]
fn our_counts_take_the_card_s_layout_with_our_numbers() {
    // The media record's pairs: (TX, RX) of each count. The card has 2 of
    // everything; the second device 0 TX and 2 RX channels and flows, so
    // a pair's first word is the TX side.
    let pairs = |p: &[u8]| -> Vec<(u16, u16)> {
        assert_eq!(p.len(), 88);
        assert_eq!(p[32..44], [0; 12]);
        assert_eq!(
            (u16_at(p, 44), u16_at(p, 46)),
            (48, 0),
            "the record's offset"
        );
        assert_eq!(p[48..50], [0x14, 0x14]);
        assert_eq!(u16_at(p, 50), AUDIO);
        assert_eq!(p[80..88], [0; 8]);
        (0..7)
            .map(|k| (u16_at(p, 52 + 4 * k), u16_at(p, 54 + 4 * k)))
            .collect()
    };
    let card_pairs = pairs(&card("replies", "0x1000"));
    let second_pairs = pairs(&second("0x1000"));
    assert_eq!(
        card_pairs,
        [(1, 1), (2, 2), (2, 2), (2, 2), (2, 2), (2, 2), (8, 8)]
    );
    assert_eq!(
        second_pairs,
        [(1, 1), (0, 2), (2, 2), (0, 2), (2, 2), (0, 2), (8, 8)]
    );
    // Both devices set word 0's 0x1000 (segmented flows, netaudio) and
    // 0x0001 (the rename, #216 Finding 17).
    for p in [card("replies", "0x1000"), second("0x1000")] {
        assert_eq!(
            (p[10] & 0x10, p[11] & 0x01),
            (0x10, 0x01),
            "word 0 under 2.8.9"
        );
    }
    // Ours: 3 TX, 5 RX.
    let mut dev = Dev::new(config_sized(5, 3));
    let reply = ours(&mut dev, "0x1000");
    assert_eq!(head(&reply).3, STATUS_OK);
    assert_eq!(
        pairs(&reply),
        [(1, 1), (3, 5), (2, 2), (32, 32), (2, 2), (3, 5), (8, 8)]
    );
    // The 2.7 counts lead, word 0 with the rename's 0x0001 set (Dante
    // Controller offers the rename only then) and without 0x1000: with it
    // Dante Controller reads the segmented lists and leaves our device out
    // of its list (#216 Finding 17); without it, it reads the 2.7 lists.
    let mut counts_27 = dev.ask(1, op::COUNTS, &[]).body;
    assert_eq!(counts_27[..2], [0x00, 0x10], "2.7: TX channel rename only");
    counts_27[1] |= 0x01;
    assert_eq!(reply[10..32], counts_27[..22]);
    assert_eq!(reply[10] & 0x10, 0, "no segmented flows");
    // A device without TX states no TX flows; one without RX no RX flows.
    let mut rx_only = Dev::new(config_sized(5, 0));
    assert_eq!(
        pairs(&ours(&mut rx_only, "0x1000")),
        [(1, 1), (0, 5), (2, 2), (0, 32), (2, 2), (0, 5), (8, 8)]
    );
    let mut tx_only = Dev::new(config_sized(0, 3));
    assert_eq!(
        pairs(&ours(&mut tx_only, "0x1000"))[3],
        (32, 0),
        "TX flows only"
    );
}

#[test]
fn our_device_names_take_the_card_s_layout() {
    let check = |p: &[u8]| {
        assert_eq!(head(p).3, STATUS_OK);
        assert_eq!(
            (u16_at(p, 10), u16_at(p, 12), u16_at(p, 14)),
            (0x1c, 0x1c, 0x28)
        );
        assert_eq!(p[28..30], [0x05, 0x00]);
        assert_eq!(
            (u16_at(p, 30), u16_at(p, 32), u16_at(p, 34)),
            (60, 92, 60),
            "name, factory name, name"
        );
        assert_eq!(p[36..40], [0; 4]);
        // The name and the factory name in 32-byte fields, NUL-padded.
        for at in [60, 92] {
            let n = name_at(p, at as u16).len();
            assert!(n < 32);
            assert!(p[at + n..at + 32].iter().all(|&b| b == 0));
        }
        let board = u16_at(p, 16);
        assert_eq!(board, 124);
        let revision = u16_at(p, 18);
        assert_eq!(usize::from(revision), 124 + name_at(p, board).len() + 1);
        assert_eq!(
            p.len(),
            usize::from(revision) + name_at(p, revision).len() + 1
        );
    };
    let card_reply = card("replies", "0x1003");
    check(&card_reply);
    assert_eq!(name_at(&card_reply, 60), "card-a");
    assert_eq!(name_at(&card_reply, 92), "factory-name-00");
    let mut dev = Dev::new(config());
    dev.state.name = "renamed-device".into();
    let reply = ours(&mut dev, "0x1003");
    check(&reply);
    assert_eq!(reply[20..28], [0; 8], "the card's stale bytes, zero");
    // The versions record as the card's: router 4.3, ARC 2.8.9, min 0.2.4.
    assert_eq!(reply[40..60], card_reply[40..60]);
    assert_eq!(
        (u16_at(&reply, 44), u16_at(&reply, 48), u16_at(&reply, 50)),
        (0x0403, 0x2809, 0x0204)
    );
    assert_eq!(name_at(&reply, 60), "renamed-device");
    assert_eq!(name_at(&reply, 92), "test-device");
    assert_eq!(name_at(&reply, 124), "iem-dante");
    assert_eq!(name_at(&reply, u16_at(&reply, 18)), ":705");
    // A name of 31 characters, the most Dante takes, fills its field.
    dev.state.name = "n".repeat(31);
    let reply = ours(&mut dev, "0x1003");
    check(&reply);
    assert_eq!(name_at(&reply, 60), "n".repeat(31));
}

#[test]
fn our_tx_channels_answer_0x2400_in_the_card_s_layout() {
    let card_tx = channels(&card("replies", "0x2400"), false);
    let mut dev = Dev::new(config_sized(2, 3));
    dev.state.tx[1] = "Mix 2".into();
    let reply = ours(&mut dev, "0x2400");
    let (start, seq, op1, status) = head(&reply);
    assert_eq!((start, op1, status), (START_2809, 0x2400, STATUS_OK));
    assert_eq!(seq, head(&card("requests", "0x2400")).1);
    let page = page_289(&reply);
    assert_eq!((page.slots, page.records.len()), (3, 3));
    let got = channels(&reply, false);
    for (k, c) in got.iter().enumerate() {
        assert_eq!(c.number, k as u16 + 1);
        assert_eq!(c.flags, card_tx[0].flags, "the card's TX flags");
        assert_eq!(c.default, dev.cfg.tx[k]);
        assert_eq!(c.format, our_format(&dev.cfg));
        assert_eq!(c.sub, None);
    }
    assert_eq!(
        got.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        ["T1", "Mix 2", "T3"]
    );
}

#[test]
fn our_rx_channels_answer_0x3400_in_the_card_s_layout() {
    let card_rx = channels(&card("replies", "0x3400"), true);
    // RX 1 receives a peer's TX 3; RX 2 is subscribed, not yet resolved;
    // RX 3 has no subscription.
    let mut dev = Dev::new(config_sized(3, 2));
    let net = Net(vec![("peer-device".into(), "TX 3".into(), adv(PEER_IP, 3))]);
    receiving(&mut dev.rx, &[(0, sub("TX 3", "peer-device"))], &net);
    let cfg = dev.cfg.clone();
    dev.state
        .set_subscription(&cfg, 0, Some(sub("TX 3", "peer-device")));
    dev.state
        .set_subscription(&cfg, 1, Some(sub("TX 9", "other-device")));
    dev.state.rx[2] = "Vocal".into();
    let reply = ours(&mut dev, "0x3400");
    assert_eq!(head(&reply).3, STATUS_OK);
    let got = channels(&reply, true);
    assert_eq!(got.len(), 3);
    for (k, c) in got.iter().enumerate() {
        assert_eq!(c.number, k as u16 + 1);
        // The card's, and netaudio's can-subscribe-self bit (0x0008): our
        // RX channels take a subscription to our own TX (#220).
        assert_eq!(c.flags, card_rx[0].flags | 0x0008, "the card's RX flags");
        assert_eq!(c.default, dev.cfg.rx[k]);
        assert_eq!(c.format, our_format(&dev.cfg));
    }
    assert_eq!(
        (got[0].sub.clone(), got[0].status, got[0].detail),
        (
            Some(("TX 3".into(), "peer-device".into())),
            card_rx[0].status,
            card_rx[0].detail
        ),
        "receiving, as the card's"
    );
    assert_eq!(
        (got[1].sub.clone(), got[1].status, got[1].detail),
        (Some(("TX 9".into(), "other-device".into())), 1, 0),
        "unresolved"
    );
    assert_eq!(
        (got[2].sub.clone(), got[2].status, got[2].detail),
        (None, 0, 0)
    );
    assert_eq!(
        (got[2].name.as_str(), got[2].default.as_str()),
        ("Vocal", "R3")
    );
}

#[test]
fn our_rx_flows_answer_0x3600_in_the_card_s_layout() {
    let card_flows = card("replies", "0x3600");
    let card_flow = rx_flow(&card_flows, page_289(&card_flows).records[0]);
    // RX 1 and RX 2 receive a peer's TX 3, RX 3 its TX 4: one flow of the
    // peer's 4 slots, its latency the peer's 2 ms.
    let mut dev = Dev::new(config());
    let net = Net(vec![
        ("peer-device".into(), "TX 3".into(), adv(PEER_IP, 3)),
        ("peer-device".into(), "TX 4".into(), adv(PEER_IP, 4)),
    ]);
    let s3 = sub("TX 3", "peer-device");
    let subs = [(0, s3.clone()), (1, s3), (2, sub("TX 4", "peer-device"))];
    receiving(&mut dev.rx, &subs, &net);
    let reply = ours(&mut dev, "0x3600");
    assert_eq!(head(&reply).3, STATUS_OK);
    let page = page_289(&reply);
    assert_eq!((page.slots, page.records.len()), (16, 1));
    assert!(
        reply[20..18 + 2 * 16].iter().all(|&b| b == 0),
        "unused slots"
    );
    let f = rx_flow(&reply, page.records[0]);
    assert_eq!(
        f,
        RxFlow {
            number: 1,
            flags: card_flow.flags,
            name: "1".into(),
            rate: 96_000,
            bits: 24,
            latency_ns: 2_000_000,
            slots: 4,
            endpoint: SocketAddrV4::new(OUR_IP, 50_000),
            mapping: (1, vec![1, 1, 2, 0]),
        }
    );
    // A flow that feeds RX 3 and RX 4 only: the mapping starts at RX 3
    // (TX 4's slot 1 feeds RX 3, TX 3's slot 2 RX 4) and holds 2 bytes.
    let mut late = Dev::new(config());
    let subs = [
        (2, sub("TX 4", "peer-device")),
        (3, sub("TX 3", "peer-device")),
    ];
    receiving(&mut late.rx, &subs, &net);
    let reply = ours(&mut late, "0x3600");
    let f = rx_flow(&reply, page_289(&reply).records[0]);
    assert_eq!((f.slots, f.mapping), (4, (3, vec![1, 2])));
    // A device with no RX flow lists none.
    let mut idle = Dev::new(config());
    let page = page_289(&ours(&mut idle, "0x3600"));
    assert_eq!(
        (page.slots, page.records.len(), page.status),
        (16, 0, STATUS_OK)
    );
}

/// Every channel of a 2.8.9 list, asked page by page as Dante Controller
/// does (the next page from the channel after the last one listed).
fn read_all(dev: &mut Dev, op: &str, rx: bool) -> (Vec<Channel>, usize) {
    let mut got = Vec::new();
    let mut pages = 0;
    let mut first = 1u16;
    loop {
        let reply = sent_bytes(dev.send(&asked(op, 20, &[first])));
        pages += 1;
        let page = page_289(&reply);
        let listed = channels(&reply, rx);
        assert!(!listed.is_empty() || page.status == STATUS_OK);
        first = listed.last().map_or(first, |c| c.number + 1);
        got.extend(listed);
        if page.status == STATUS_OK {
            return (got, pages);
        }
        assert_eq!(page.status, STATUS_MORE);
        assert!(pages < 64, "the pages never end");
    }
}

#[test]
fn long_channel_lists_take_several_pages_each_channel_once() {
    let long = |prefix: &str, n: usize| -> Vec<String> {
        (1..=n).map(|k| format!("{prefix}{k:0>28}")).collect()
    };
    let mut dev = Dev::new(DeviceConfig {
        rx: long("R", 40),
        tx: long("T", 40),
        ..config()
    });
    for (op, rx) in [("0x2400", false), ("0x3400", true)] {
        let (got, pages) = read_all(&mut dev, op, rx);
        assert!(pages > 1, "{op}: one page");
        assert_eq!(
            got.iter().map(|c| c.number).collect::<Vec<_>>(),
            (1..=40).collect::<Vec<u16>>(),
            "{op}"
        );
    }
}

#[test]
fn a_2809_page_takes_its_first_and_last_item_and_audio_only() {
    let mut dev = Dev::new(config_sized(4, 4));
    // Channels 2 and 3 only.
    let reply = sent_bytes(dev.send(&asked("0x2400", 20, &[2, 3])));
    let got = channels(&reply, false);
    assert_eq!(got.iter().map(|c| c.number).collect::<Vec<_>>(), [2, 3]);
    assert_eq!(head(&reply).3, STATUS_OK);
    // From channel 3 to the end.
    let reply = sent_bytes(dev.send(&asked("0x3400", 20, &[3, 0])));
    let got = channels(&reply, true);
    assert_eq!(got.iter().map(|c| c.number).collect::<Vec<_>>(), [3, 4]);
    // Another media than audio (selector 1): an empty page, nothing listed.
    for op in ["0x2400", "0x3400", "0x3600"] {
        let reply = sent_bytes(dev.send(&asked(op, 18, &[2])));
        assert_eq!(head(&reply).3, STATUS_OK, "{op}");
        assert_eq!(reply[HEADER_LEN..], [0; 8], "{op}");
    }
    // No first item, or a body that ends before it: refused, nothing read.
    for op in ["0x2400", "0x3400", "0x3600"] {
        let reply = sent_bytes(dev.send(&asked(op, 20, &[0])));
        assert_eq!(head(&reply).3, STATUS_ERROR, "{op}");
        let mut cut = card("requests", op);
        cut.truncate(21);
        cut[2..4].copy_from_slice(&21u16.to_be_bytes());
        let reply = sent_bytes(dev.send(&cut));
        assert_eq!(head(&reply).3, STATUS_ERROR, "{op} cut");
    }
}

#[test]
fn our_tx_names_under_2809_list_only_renamed_channels_as_the_card() {
    // The card lists TX 2, the one renamed, with its current name.
    let card_reply = card("replies", "0x2010");
    assert_eq!(card_reply[10..12], [2, 1]);
    assert_eq!((u16_at(&card_reply, 12), u16_at(&card_reply, 14)), (2, 2));
    assert_eq!(name_at(&card_reply, u16_at(&card_reply, 16)), "Custom 2");
    let mut dev = Dev::new(config_sized(2, 3));
    let reply = ours(&mut dev, "0x2010");
    assert_eq!(head(&reply).3, STATUS_OK);
    assert_eq!(reply[10..12], [3, 0], "no channel renamed");
    dev.state.tx[1] = "Mix 2".into();
    let reply = ours(&mut dev, "0x2010");
    assert_eq!(reply[10..12], [3, 1]);
    assert_eq!((u16_at(&reply, 12), u16_at(&reply, 14)), (2, 2));
    assert_eq!(name_at(&reply, u16_at(&reply, 16)), "Mix 2");
    // Under 2.7 every channel is listed, as before.
    let packet = card("requests", "0x2010");
    let mut req_27 = packet.clone();
    req_27[0..2].copy_from_slice(&CONTROLLER.to_be_bytes());
    let reply = sent_bytes(dev.send(&req_27));
    assert_eq!(reply[10..12], [3, 3]);
}

#[test]
fn our_tx_flow_labels_answer_as_both_2809_devices() {
    // Both devices answer 0x2204 with start code 0x2801: no label, a slot
    // per TX flow (the card 2, the adapter without TX none).
    let card_reply = card("replies", "0x2204");
    assert_eq!(
        (head(&card_reply).0, head(&card_reply).3),
        (START_2801, STATUS_OK)
    );
    assert_eq!(card_reply[10..], [2, 0, 0, 0, 0, 0]);
    assert_eq!(second("0x2204")[10..], [0, 0]);
    let mut dev = Dev::new(config_sized(2, 3));
    let reply = ours(&mut dev, "0x2204");
    let (start, seq, op1, status) = head(&reply);
    assert_eq!(
        (start, seq, op1, status),
        (
            START_2801,
            head(&card("requests", "0x2204")).1,
            0x2204,
            STATUS_OK
        )
    );
    let mut want = vec![16, 0];
    want.extend([0; 32]);
    assert_eq!(reply[10..], want);
    let mut rx_only = Dev::new(config_sized(2, 0));
    assert_eq!(ours(&mut rx_only, "0x2204")[10..], [0, 0]);
}

#[test]
fn the_reads_both_generations_share_answer_alike() {
    let mut dev = Dev::new(config());
    for op in ["0x1102", "0x1100", "0x3300", "0x2320"] {
        let req = card("requests", op);
        let reply = ours(&mut dev, op);
        let mut req_27 = req.clone();
        req_27[0..2].copy_from_slice(&CONTROLLER.to_be_bytes());
        let reply_27 = sent_bytes(dev.send(&req_27));
        assert_eq!(head(&reply).0, START_2809, "{op}");
        assert_eq!(reply[2..], reply_27[2..], "{op}");
        assert_eq!(head(&reply).3, head(&card("replies", op)).3, "{op}");
    }
    // The TX flow list too (dstack #148), our 2.8.9 list.
    let reply = ours(&mut dev, "0x2600");
    assert_eq!(head(&reply).3, STATUS_OK);
}

#[test]
fn our_2_7_answers_stay_as_they_were() {
    // Our 0x1000 for 2 TX and 2 RX, as Dante Controller read it on
    // 12.11.2026 under 0x2729.
    let mut dev = Dev::new(config_sized(2, 2));
    #[rustfmt::skip]
    let want = [
        0x00, 0x10, 0x00, 0x02, 0x00, 0x02, 0x00, 0x04, 0x00, 0x02, 0x00, 0x08,
        0x00, 0x20, 0x00, 0x20, 0x00, 0x04, 0x00, 0x01, 0x00, 0x01,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    for start in [CONTROLLER, NETAUDIO] {
        let reply = sent_bytes(dev.send(&request(start, 3, op::COUNTS, 0, &[])));
        assert_eq!(reply[10..], want);
        // 0x1003 keeps the 2.7 form: the name after a 38-byte header.
        let reply = sent_bytes(dev.send(&request(start, 4, op::DEVICE_NAMES, 0, &[])));
        assert_eq!(name_at(&reply, 48), "test-device");
    }
    // 2.8.9's channel and flow pages are no 2.7 opcodes.
    for op in ["0x2400", "0x3400", "0x3600"] {
        for start in [CONTROLLER, NETAUDIO] {
            let mut req = card("requests", op);
            req[0..2].copy_from_slice(&start.to_be_bytes());
            let op1 = u16_at(&req, 6);
            assert_eq!(dev.send(&req), ArcOutcome::Unanswered { op1 }, "{op}");
        }
    }
}

#[test]
fn our_txt_announces_arc_2_8_9_as_our_0x1003_states() {
    let cfg = config();
    let state = DeviceState::from_config(&cfg);
    let records = crate::announce::records(&state, &cfg, "test-device.local.", OUR_IP);
    let arc = &records[0].txt;
    assert!(arc.contains(&"arcp_vers=2.8.9".to_string()), "{arc:?}");
    assert!(arc.contains(&"router_vers=4.3.0".to_string()), "{arc:?}");
    assert!(arc.contains(&"arcp_min=0.2.4".to_string()), "{arc:?}");
    let mut dev = Dev::new(cfg);
    let reply = ours(&mut dev, "0x1003");
    assert_eq!(
        (u16_at(&reply, 44), u16_at(&reply, 48), u16_at(&reply, 50)),
        (0x0403, 0x2809, 0x0204),
        "router 4.3, ARC 2.8.9, ARC min 0.2.4"
    );
}

const DC_RENAME: &str = include_str!("../../../tests/fixtures/arc_dc_rename.json");

#[test]
fn dante_controller_s_rename_under_2809_renames_our_device_and_is_saved() {
    let f: serde_json::Value = serde_json::from_str(DC_RENAME).expect("the fixture is JSON");
    let frame = hex(f["requests"]["0x1001"].as_str().expect("the rename"));
    let want = hex(f["replies"]["0x1001"].as_str().expect("its answer"));
    assert_eq!(head(&frame).0, START_2809);
    let dir = tempfile::tempdir().unwrap();
    let mut device = Device::load(config_in(dir.path())).unwrap();
    match respond_idle(&mut device, &frame) {
        ArcOutcome::Changed(bytes, change) => {
            assert_eq!(bytes, want, "status OK, no body, as our device answered");
            assert!(change.device_renamed);
        }
        other => panic!("not renamed: {other:?}"),
    }
    assert_eq!(device.state.name, "test-renamed");
    let saved = Device::load(device.cfg.clone()).unwrap();
    assert_eq!(saved.state.name, "test-renamed", "kept across a restart");
}
