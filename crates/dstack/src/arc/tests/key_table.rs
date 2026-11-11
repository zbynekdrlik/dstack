// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The settings key table (ARC 0x1102, dstack #216) against a real card's
//! replies (`tests/fixtures/arc_settings_card.json`): Dante Controller
//! reads from it which of a device's settings it may write. Our device
//! answered zeros, so Dante Controller greyed out the rename and never sent
//! the latency write.

use super::*;

use crate::arc::control::{key, parse_settings};

const CARD: &str = include_str!("../../../tests/fixtures/arc_settings_card.json");

/// A key the device lets a controller write.
const READ_WRITE: u16 = 0x0003;
/// A key the device only states.
const READ_ONLY: u16 = 0x0001;

/// The card's reply to `op1` from the fixture, the whole datagram.
fn card_reply(op1: &str) -> Vec<u8> {
    let card: serde_json::Value = serde_json::from_str(CARD).expect("the fixture is JSON");
    let text = card["replies"][op1]
        .as_str()
        .expect("the reply is in the fixture");
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
        .collect()
}

/// A 0x1102 body, read on its own (not by the code under test): the count
/// (u16), then that many pairs `key, access` (u16 each), and nothing after.
fn key_table(body: &[u8]) -> Vec<(u16, u16)> {
    let n = usize::from(read_u16(body, 0).expect("the count"));
    assert_eq!(
        body.len(),
        2 + 4 * n,
        "the count and the pairs fill the body"
    );
    (0..n)
        .map(|k| {
            let at = 2 + 4 * k;
            (
                read_u16(body, at).expect("a key"),
                read_u16(body, at + 2).expect("its access"),
            )
        })
        .collect()
}

/// A settings read (0x1100, Dante Controller's start code) of `keys`.
fn settings_read(keys: &[u16]) -> Vec<u8> {
    let mut body = u16::try_from(keys.len())
        .expect("few keys")
        .to_be_bytes()
        .to_vec();
    for k in keys {
        body.extend_from_slice(&k.to_be_bytes());
    }
    request(CONTROLLER, 7, op::ZEROS_1100, 0, &body)
}

#[test]
fn the_card_s_key_table_reads_as_31_key_access_pairs() {
    let reply = card_reply("0x1102");
    let (h, body) = Header::parse(&reply).expect("the card's reply parses");
    assert_eq!(
        (h.start, h.op1, h.op2),
        (CONTROLLER, op::ZEROS_1102, STATUS_OK)
    );
    let table = key_table(body);
    assert_eq!(table.len(), 31);
    assert_eq!(table[0], (key::SAMPLE_RATE, READ_ONLY));
    assert_eq!(table[30], (0x0209, READ_ONLY));
    assert!(
        table
            .iter()
            .all(|&(_, a)| a == READ_WRITE || a == READ_ONLY)
    );
    let access = |k: u16| table.iter().find(|&&(t, _)| t == k).map(|&(_, a)| a);
    // The latency, as configured and in effect, can be written; its
    // minimum and maximum are stated.
    assert_eq!(access(key::LATENCY_ACTIVE), Some(READ_WRITE));
    assert_eq!(access(key::LATENCY_CONFIGURED), Some(READ_WRITE));
    assert_eq!(access(key::LATENCY_MIN), Some(READ_ONLY));
    assert_eq!(access(key::LATENCY_MAX), Some(READ_ONLY));
}

/// The card's settings read holds the keys of its table, in the table's
/// order, with these values at 96 kHz and 5 ms: the default 1 ms, the
/// minimum 1 ms, the maximum 10.156 ms.
#[test]
fn the_card_s_settings_read_holds_its_table_s_keys() {
    let table = key_table(Header::parse(&card_reply("0x1102")).expect("parses").1);
    let reply = card_reply("0x1100");
    let (h, body) = Header::parse(&reply).expect("the card's reply parses");
    assert_eq!((h.op1, h.op2), (op::ZEROS_1100, STATUS_OK));
    // 36 asked, 31 held; each held key's pair in the table's order.
    assert_eq!((body[0], body[1]), (36, 31));
    let held: Vec<u16> = (0..31)
        .map(|k| read_u16(body, 2 + 4 * k).expect("a key"))
        .collect();
    let keys: Vec<u16> = table.iter().map(|&(k, _)| k).collect();
    assert_eq!(held, keys);
    let s = parse_settings(body).expect("the card's settings");
    assert_eq!(s.all.len(), 31);
    assert_eq!(s.sample_rate, Some(96_000));
    assert_eq!(s.pullup, Some(0));
    assert_eq!(s.latency_default_ns, Some(1_000_000));
    assert_eq!(s.latency_configured_ns, Some(5_000_000));
    assert_eq!(s.latency_active_ns, Some(5_000_000));
    assert_eq!(s.latency_min_ns, Some(1_000_000));
    assert_eq!(s.latency_max_ns, Some(10_156_250));
}

/// Our 0x1102 reply is a table in the card's layout: the keys our settings
/// read answers, the latency (as configured and in effect) writable, the
/// rest read only.
#[test]
fn our_key_table_lists_the_keys_our_settings_read_answers() {
    let mut cfg = config();
    cfg.sample_rate = 96_000;
    cfg.latency_ns = 10_000_000;
    let mut dev = Dev::new(cfg);
    let reply = decode(dev.send(&request(CONTROLLER, 3, op::ZEROS_1102, 0, &[])));
    assert_eq!(reply.header.op2, STATUS_OK);
    let table = key_table(&reply.body);
    // Every key of ours is one the card lists too.
    let card = key_table(Header::parse(&card_reply("0x1102")).expect("parses").1);
    let mut ours: Vec<u16> = table.iter().map(|&(k, _)| k).collect();
    assert!(ours.iter().all(|k| card.iter().any(|&(c, _)| c == *k)));
    // Asked for every key of the card's table, our settings read holds
    // exactly the keys of our table.
    let asked: Vec<u16> = card.iter().map(|&(k, _)| k).collect();
    let answer = decode(dev.send(&settings_read(&asked)));
    let held: Vec<u16> = parse_settings(&answer.body)
        .expect("our settings")
        .all
        .into_keys()
        .collect();
    ours.sort_unstable();
    assert_eq!(ours, held);
    assert_eq!(
        held,
        [
            key::SAMPLE_RATE,
            key::PULLUP,
            key::LATENCY_DEFAULT,
            key::LATENCY_CONFIGURED,
            key::LATENCY_ACTIVE,
            key::LATENCY_MAX,
            key::LATENCY_MIN,
        ]
    );
    let access = |k: u16| table.iter().find(|&&(t, _)| t == k).map(|&(_, a)| a);
    assert_eq!(access(key::LATENCY_ACTIVE), Some(READ_WRITE));
    assert_eq!(access(key::LATENCY_CONFIGURED), Some(READ_WRITE));
    for k in [
        key::SAMPLE_RATE,
        key::PULLUP,
        key::LATENCY_DEFAULT,
        key::LATENCY_MIN,
        key::LATENCY_MAX,
    ] {
        assert_eq!(access(k), Some(READ_ONLY), "key {k:#06x}");
    }
}

/// A device whose latency range holds its own latency alone takes no other
/// latency write: its table marks the latency read only.
#[test]
fn a_latency_no_write_can_move_is_read_only_in_our_key_table() {
    let mut cfg = config();
    cfg.tx_latency_ns = None;
    // A TX floor above any latency the rings hold, and the latency under
    // it: the range is that latency alone.
    cfg.tx_margin_ns = 2_000_000_000;
    cfg.latency_ns = 1_000_000_000;
    assert_eq!(cfg.latency_range(), (1_000_000_000, 1_000_000_000));
    let mut dev = Dev::new(cfg);
    let reply = decode(dev.send(&request(CONTROLLER, 4, op::ZEROS_1102, 0, &[])));
    let table = key_table(&reply.body);
    assert_eq!(table.len(), 7);
    assert!(table.iter().all(|&(_, a)| a == READ_ONLY), "{table:x?}");
}
