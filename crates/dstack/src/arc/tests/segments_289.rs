// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! ARC 2.8.9's records walked by their segment lengths (dstack #349), the
//! way netaudio and Dante Controller read them, with a strict walker of
//! this file written from the card's capture
//! (`tests/fixtures/arc_289_card.json`), never from our writer. A record is
//! a chain of segments, each `[its words, the words from it to the
//! record's end]` (the last one's second byte may be 0); a reader steps
//! to the next segment by the first byte. Our replies are walked with odd
//! channel counts and odd name lengths, the case #345's heartbeat record
//! got wrong while every test used aligned counts.

use super::*;

use crate::arc::flows::tests::hex;

const CARD: &str = include_str!("../../../tests/fixtures/arc_289_card.json");

fn fixture() -> serde_json::Value {
    serde_json::from_str(CARD).expect("the fixture is JSON")
}

/// The card's capture: `kind` (`requests`, `replies`, `second_device`),
/// `op`.
fn card(kind: &str, op: &str) -> Vec<u8> {
    hex(fixture()[kind][op]
        .as_str()
        .expect("the frame is in the fixture"))
}

fn word(p: &[u8], at: usize) -> Result<usize, String> {
    p.get(at..at + 2)
        .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
        .ok_or_else(|| format!("no u16 at {at} of {}", p.len()))
}

/// The reply's header: the length field is the datagram's size.
fn framed(p: &[u8]) -> Result<(), String> {
    let stated = word(p, 2)?;
    if stated != p.len() {
        return Err(format!(
            "the length field says {stated}, the reply is {}",
            p.len()
        ));
    }
    Ok(())
}

/// Walks the record at `at` by its segments; returns its length in bytes.
/// The record starts at a multiple of 4 and ends inside the reply; each
/// segment states at least one word; every segment but the last states
/// the words from it to the record's end, which the step leaves; the last
/// states its own words again or 0.
fn walk_record(p: &[u8], at: usize) -> Result<usize, String> {
    if !at.is_multiple_of(4) {
        return Err(format!("a record at {at}, not a multiple of 4"));
    }
    let (first, total) = segment(p, at)?;
    let total = if total == 0 { first } else { total };
    if total < first {
        return Err(format!(
            "record at {at}: a segment of {first} words in {total}"
        ));
    }
    let end = at + 2 * total;
    if end > p.len() {
        return Err(format!(
            "record at {at}: {total} words pass the reply's end {}",
            p.len()
        ));
    }
    let (mut pos, mut left) = (at, total);
    loop {
        let (words, rest) = segment(p, pos)?;
        if words > left {
            return Err(format!("segment at {pos}: {words} words, {left} left"));
        }
        let last = words == left;
        if rest != left && !(last && rest == 0) {
            return Err(format!(
                "segment at {pos}: states {rest} words to the end, {left} are"
            ));
        }
        pos += 2 * words;
        left -= words;
        if last {
            return Ok(2 * total);
        }
    }
}

fn segment(p: &[u8], at: usize) -> Result<(usize, usize), String> {
    let s = p
        .get(at..at + 2)
        .ok_or_else(|| format!("no segment at {at}"))?;
    if s[0] == 0 {
        return Err(format!("segment at {at}: 0 words"));
    }
    Ok((usize::from(s[0]), usize::from(s[1])))
}

/// A page's records (0x2400, 0x3400, 0x3600): 6 zero bytes after the
/// header, `[slots, count]`, the offsets; each record walked.
fn walk_page(p: &[u8]) -> Result<Vec<usize>, String> {
    framed(p)?;
    let (slots, count) = (usize::from(p[16]), usize::from(p[17]));
    if count > slots || 18 + 2 * slots > p.len() {
        return Err(format!(
            "a page of {count} in {slots} slots, {} bytes",
            p.len()
        ));
    }
    (0..count)
        .map(|k| {
            let at = word(p, 18 + 2 * k)?;
            walk_record(p, at).map(|_| at)
        })
        .collect()
}

/// An RX flow's mapping node (0x3600): from the record's second segment,
/// the node's offset; the first RX channel, the node's byte count (even),
/// no next node, the bytes, all inside the reply.
fn walk_mapping(p: &[u8], record: usize) -> Result<(), String> {
    let node = word(p, record + 54)?;
    let (count, next) = (word(p, node + 2)?, word(p, node + 4)?);
    if count < 2 || !count.is_multiple_of(2) || next != 0 || node + 6 + count > p.len() {
        return Err(format!(
            "mapping at {node}: {count} bytes, next {next}, reply of {}",
            p.len()
        ));
    }
    Ok(())
}

/// 0x1000's media record: its offset at packet byte 44.
fn walk_counts(p: &[u8]) -> Result<usize, String> {
    framed(p)?;
    walk_record(p, word(p, 44)?)
}

/// 0x1003's names and versions records: their offsets at packet bytes 10
/// and 14.
fn walk_names(p: &[u8]) -> Result<(usize, usize), String> {
    framed(p)?;
    Ok((walk_record(p, word(p, 10)?)?, walk_record(p, word(p, 14)?)?))
}

#[test]
fn the_walker_reads_every_record_of_the_card_and_the_adapter() {
    assert_eq!(walk_counts(&card("replies", "0x1000")), Ok(40));
    assert_eq!(walk_counts(&card("second_device", "0x1000")), Ok(40));
    assert_eq!(walk_names(&card("replies", "0x1003")), Ok((10, 20)));
    for op in ["0x2400", "0x3400", "0x3600"] {
        let p = card("replies", op);
        let records = walk_page(&p).unwrap_or_else(|e| panic!("{op}: {e}"));
        assert!(!records.is_empty(), "{op}");
        if op == "0x3600" {
            assert_eq!(walk_record(&p, records[0]), Ok(68));
            walk_mapping(&p, records[0]).unwrap();
        }
    }
    // Broken one rule at a time, the walker refuses.
    let p = card("replies", "0x3600");
    let at = walk_page(&p).unwrap()[0];
    let with = |pos: usize, b: u8| {
        let mut q = p.clone();
        q[pos] = b;
        walk_record(&q, at)
    };
    assert!(with(at + 41, 0x0d).unwrap_err().contains("states 13"));
    assert!(
        with(at + 40, 0x0f)
            .unwrap_err()
            .contains("15 words, 14 left")
    );
    assert!(with(at + 1, 0x10).unwrap_err().contains("in 16"));
    assert!(
        with(at + 1, 0xff)
            .unwrap_err()
            .contains("pass the reply's end")
    );
    assert!(with(at, 0).unwrap_err().contains("0 words"));
    assert!(
        walk_record(&p, at + 2)
            .unwrap_err()
            .contains("multiple of 4")
    );
    let mut short = p.clone();
    short.pop();
    assert!(framed(&short).unwrap_err().contains("length field"));
}

/// Odd channel counts and odd name lengths (a name of 1, 2, 3 bytes), the
/// 2.8.9 reads Dante Controller and netaudio send.
#[test]
fn our_2809_records_walk_for_odd_counts_and_names() {
    for (rx, tx) in [
        (1, 1),
        (1, 3),
        (3, 1),
        (3, 5),
        (5, 3),
        (0, 1),
        (1, 0),
        (17, 33),
    ] {
        let cfg = DeviceConfig {
            name: "o".into(),
            rx: (1..=rx)
                .map(|n| "r".repeat(n % 3 + 1) + &n.to_string())
                .collect(),
            tx: (1..=tx)
                .map(|n| "t".repeat(n % 4 + 1) + &n.to_string())
                .collect(),
            ..config()
        };
        let mut dev = Dev::new(cfg);
        let ask = |dev: &mut Dev, op: &str| sent_bytes(dev.send(&card("requests", op)));
        walk_counts(&ask(&mut dev, "0x1000")).unwrap_or_else(|e| panic!("{rx}/{tx} 0x1000: {e}"));
        walk_names(&ask(&mut dev, "0x1003")).unwrap_or_else(|e| panic!("{rx}/{tx} 0x1003: {e}"));
        for (op, n) in [("0x2400", tx.min(TX_PAGE)), ("0x3400", rx.min(RX_PAGE))] {
            let records =
                walk_page(&ask(&mut dev, op)).unwrap_or_else(|e| panic!("{rx}/{tx} {op}: {e}"));
            // A page may end early at its soft limit (status 0x8112).
            assert!(
                records.len() <= n && (n == 0) == records.is_empty(),
                "{rx}/{tx} {op}: {} records",
                records.len()
            );
        }
    }
}

/// RX flows whose mappings span an odd number of RX channels (1 and 3)
/// walk, record and mapping.
#[test]
fn our_rx_flow_records_walk_for_odd_spans() {
    let net = Net(vec![
        ("peer-device".into(), "TX 3".into(), adv(PEER_IP, 3)),
        ("peer-device".into(), "TX 4".into(), adv(PEER_IP, 4)),
    ]);
    let one = [(4, sub("TX 3", "peer-device"))];
    let three = [
        (0, sub("TX 3", "peer-device")),
        (1, sub("TX 4", "peer-device")),
        (2, sub("TX 3", "peer-device")),
    ];
    for subs in [&one[..], &three[..]] {
        let mut dev = Dev::new(config());
        receiving(&mut dev.rx, subs, &net);
        let p = sent_bytes(dev.send(&card("requests", "0x3600")));
        let records = walk_page(&p).unwrap();
        assert_eq!(records.len(), 1, "{subs:?}");
        assert_eq!(walk_record(&p, records[0]), Ok(68));
        walk_mapping(&p, records[0]).unwrap();
    }
}
