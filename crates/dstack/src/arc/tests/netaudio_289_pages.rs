// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! netaudio's channel list reads of a device that announces ARC 2.8.9
//! (dstack #237): 0x2400 (TX) and 0x3400 (RX), as netaudio 0.4.2's own
//! encoder builds them (`tests/fixtures/arc_netaudio_289_pages.json`).
//! netaudio asks the first page with media selector 1 and every following
//! page with the media type of the last record it read (3, audio); our
//! device answered selector 3 with an empty page, and netaudio refused the
//! whole list ("channel page made no progress").
//!
//! netaudio's page rule is written out here from its source (a protocol
//! reference, netaudio-core `channel_inventory.rs`), with this file's own
//! reader of the records, never with the code under test.

use std::collections::{BTreeMap, BTreeSet};

use super::*;

use crate::arc::flows::tests::hex;

const PAGES: &str = include_str!("../../../tests/fixtures/arc_netaudio_289_pages.json");

/// ARC 2.8.9's start code.
const START_2809: u16 = 0x2809;
/// The media selector of netaudio's first page.
const FIRST_SELECTOR: u16 = 1;
/// The media type of an audio channel, netaudio's selector after page one.
const AUDIO: u16 = 3;
/// Packet offsets of a page request's media selector and first channel.
const SELECTOR_AT: usize = HEADER_LEN + 8;
const FIRST_AT: usize = HEADER_LEN + 10;

fn fixture() -> serde_json::Value {
    serde_json::from_str(PAGES).expect("the fixture is JSON")
}

/// netaudio's request `name` from the fixture, the whole datagram.
fn frame(name: &str) -> Vec<u8> {
    hex(fixture()["requests"][name]
        .as_str()
        .expect("the frame is in the fixture"))
}

/// netaudio's following-page request for `op` ("0x2400", "0x3400"), set
/// to `selector` and `first`.
fn next_page(op: &str, selector: u16, first: u16) -> Vec<u8> {
    let mut req = frame(&format!("{op} next"));
    req[SELECTOR_AT..SELECTOR_AT + 2].copy_from_slice(&selector.to_be_bytes());
    req[FIRST_AT..FIRST_AT + 2].copy_from_slice(&first.to_be_bytes());
    req
}

/// A channel's (media type, media-local id), netaudio's identity.
type Identity = (u16, u16);

/// A page as netaudio reads it: the status and, for each of the `count`
/// records, its channel number and (media type, media-local id). A
/// channel number or identity twice in one page is netaudio's refusal.
fn records(p: &[u8], op1: u16) -> (u16, Vec<(u16, Identity)>) {
    assert!(p.len() <= MAX_DATAGRAM);
    assert_eq!(usize::from(u16_at(p, 2)), p.len(), "the length field");
    assert_eq!((u16_at(p, 0), u16_at(p, 6)), (START_2809, op1));
    let (slots, count) = (usize::from(p[16]), usize::from(p[17]));
    assert!(count <= slots, "{count} records in {slots} slots");
    let recs: Vec<(u16, Identity)> = (0..count)
        .map(|k| {
            let at = usize::from(u16_at(p, 18 + 2 * k));
            let number = u16_at(p, at + 2);
            assert!(number > 0, "a channel number");
            (number, (u16_at(p, at + 6), u16_at(p, at + 8)))
        })
        .collect();
    let numbers: BTreeSet<u16> = recs.iter().map(|r| r.0).collect();
    let ids: BTreeSet<Identity> = recs.iter().map(|r| r.1).collect();
    assert_eq!((numbers.len(), ids.len()), (count, count), "a repeat");
    (u16_at(p, 8), recs)
}

/// netaudio's read of the list `op`: page one from the fixture, then while
/// the status is 0x8112 the next page from the lowest media-local id not
/// seen yet, with the last record's media type as the selector. A page
/// after the first that adds no record is netaudio's refusal.
fn netaudio_reads(dev: &mut Dev, op: &str, op1: u16) -> Result<(Vec<u16>, usize), String> {
    let mut seen = BTreeSet::new();
    // Each channel number's identity, as netaudio keeps it across pages.
    let mut numbers = BTreeMap::new();
    let mut req = frame(&format!("{op} first"));
    for pages in 1..=256 {
        let reply = sent_bytes(dev.send(&req));
        let (status, recs) = records(&reply, op1);
        for &(number, id) in &recs {
            if *numbers.entry(number).or_insert(id) != id {
                return Err(format!("{op}: channel {number} with two identities"));
            }
        }
        let added = recs.iter().filter(|(_, id)| !seen.contains(id)).count();
        if pages > 1 && added == 0 {
            return Err(format!("{op}: page {pages} made no progress"));
        }
        seen.extend(recs.iter().map(|&(_, id)| id));
        if status == STATUS_OK {
            return Ok((seen.iter().map(|&(_, id)| id).collect(), pages));
        }
        assert_eq!(status, STATUS_MORE, "{op}: page {pages}");
        let media = recs
            .last()
            .ok_or(format!("{op}: a partial page {pages} with no record"))?
            .1
            .0;
        let next = (1..=u16::MAX)
            .find(|id| !seen.contains(&(media, *id)))
            .expect("an unseen id");
        req = next_page(op, media, next);
    }
    Err(format!("{op}: more than 256 pages"))
}

#[test]
fn the_fixture_holds_netaudio_s_requests_and_our_empty_reply() {
    for (name, op1, selector, first) in [
        ("0x2400 first", 0x2400, FIRST_SELECTOR, 1),
        ("0x2400 next", 0x2400, AUDIO, 33),
        ("0x3400 first", 0x3400, FIRST_SELECTOR, 1),
        ("0x3400 next", 0x3400, AUDIO, 9),
    ] {
        let f = frame(name);
        assert_eq!(usize::from(u16_at(&f, 2)), f.len(), "{name}");
        assert_eq!((u16_at(&f, 0), u16_at(&f, 6)), (START_2809, op1), "{name}");
        assert_eq!(u16_at(&f, HEADER_LEN + 6), 1, "{name}");
        assert_eq!(
            (u16_at(&f, SELECTOR_AT), u16_at(&f, FIRST_AT)),
            (selector, first),
            "{name}"
        );
        assert_eq!(u16_at(&f, HEADER_LEN + 12), 0, "{name}: to the end");
    }
    // v0.6.101's answer to a following page: OK, 8 zero bytes, no record.
    let old = hex(fixture()["replies_v0_6_101"]["0x2400 next"]
        .as_str()
        .unwrap());
    assert_eq!(records(&old, 0x2400), (STATUS_OK, vec![]));
}

#[test]
fn netaudio_s_following_page_lists_the_channels_from_its_first() {
    let mut dev = Dev::new(config_sized(64, 64));
    let reply = sent_bytes(dev.send(&frame("0x2400 next")));
    // Channels 33 on, as many as one page holds (T33..T64 take two).
    let (status, recs) = records(&reply, 0x2400);
    assert_eq!(status, STATUS_MORE);
    assert!(recs.len() > 1, "{recs:?}");
    let want: Vec<_> = (33..)
        .take(recs.len())
        .map(|id| (id, (AUDIO, id)))
        .collect();
    assert_eq!(recs, want);
    assert_eq!(u16_at(&reply, 4), 0x0202, "netaudio's sequence");
    let reply = sent_bytes(dev.send(&frame("0x3400 next")));
    let (_, recs) = records(&reply, 0x3400);
    assert_eq!(recs.first(), Some(&(9, (AUDIO, 9))));
}

#[test]
fn netaudio_reads_every_page_of_a_16_and_a_64_channel_device() {
    let named = |prefix: &str, n: usize, long: bool| -> Vec<String> {
        (1..=n)
            .map(|k| match long {
                true => format!("{prefix}{k:0>30}"),
                false => format!("{prefix} {k}"),
            })
            .collect()
    };
    for n in [16, 64] {
        for long in [false, true] {
            let cfg = DeviceConfig {
                rx: named("R", n, long),
                tx: named("T", n, long),
                ..config()
            };
            let mut dev = Dev::new(cfg.clone());
            // Every RX channel subscribed: the heaviest RX records.
            for rx in 0..n {
                let tx = format!("{:0>31}", rx + 1);
                assert!(
                    dev.state
                        .set_subscription(&cfg, rx, Some(sub(&tx, "peer-device")))
                );
            }
            for (op, op1) in [("0x2400", 0x2400), ("0x3400", 0x3400)] {
                let (ids, pages) = netaudio_reads(&mut dev, op, op1)
                    .unwrap_or_else(|e| panic!("{n} channels, long {long}: {e}"));
                let all: Vec<u16> = (1..=n as u16).collect();
                assert_eq!(ids, all, "{op}, {n} channels, long {long}");
                // Every one of these lists takes several pages (16 TX
                // channels end a page at the soft limit already).
                assert!(pages > 1, "{op}: {n} channels, long {long}, one page");
            }
        }
    }
}

#[test]
fn selector_3_is_answered_as_selector_1_and_another_media_gets_none() {
    let mut dev = Dev::new(config_sized(64, 64));
    for op in ["0x2400", "0x3400"] {
        let one = sent_bytes(dev.send(&next_page(op, FIRST_SELECTOR, 5)));
        let three = sent_bytes(dev.send(&next_page(op, AUDIO, 5)));
        assert_eq!(three, one, "{op}: byte for byte");
        for other in [0, 2, 4, 0x8003] {
            let reply = sent_bytes(dev.send(&next_page(op, other, 5)));
            assert_eq!(head_status(&reply), STATUS_OK, "{op} {other:#x}");
            assert_eq!(reply[HEADER_LEN..], [0; 8], "{op} {other:#x}");
        }
    }
    // The RX flows (0x3600) read the same selector.
    let mut flows = |selector| {
        let mut req = next_page("0x2400", selector, 1);
        req[6..8].copy_from_slice(&0x3600u16.to_be_bytes());
        sent_bytes(dev.send(&req))
    };
    let (one, three) = (flows(FIRST_SELECTOR), flows(AUDIO));
    assert_eq!(three, one);
    assert_ne!(one[HEADER_LEN..], [0; 8], "an empty page for audio");
    assert_eq!(flows(2)[HEADER_LEN..], [0; 8]);
}

fn head_status(p: &[u8]) -> u16 {
    u16_at(p, 8)
}
