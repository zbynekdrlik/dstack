// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! netaudio's writes to a device that announces ARC 2.8.9 (dstack #220),
//! as netaudio 0.4.2's own encoders build them
//! (`tests/fixtures/arc_netaudio_289_writes.json`): 0x3410 subscribes,
//! 0x3401 renames an RX channel, 0x2438 names TX channels. netaudio picks
//! them from our `arcp_vers`, and our device left them unanswered. Each is
//! to change our device as the 2.7 write of the same kind does (0x3010,
//! 0x3001, 0x2013), with status OK under start code 0x2809.
//!
//! The replies are read with this file's own parser, never with the code
//! under test.

use super::*;

use crate::arc::flows::tests::hex;

const WRITES: &str = include_str!("../../../tests/fixtures/arc_netaudio_289_writes.json");

/// ARC 2.8.9's start code.
const START_2809: u16 = 0x2809;
const SUBSCRIBE_2809: u16 = 0x3410;
const RX_RENAME_2809: u16 = 0x3401;
const TX_NAMES_2809: u16 = 0x2438;

/// netaudio's frame `name` from the fixture, the whole datagram.
fn frame(name: &str) -> Vec<u8> {
    let f: serde_json::Value = serde_json::from_str(WRITES).expect("the fixture is JSON");
    hex(f["requests"][name]
        .as_str()
        .expect("the frame is in the fixture"))
}

/// `frame(name)` with `edit` applied, its length field set again.
fn edited(name: &str, edit: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut f = frame(name);
    edit(&mut f);
    let len = u16::try_from(f.len()).unwrap().to_be_bytes();
    f[2..4].copy_from_slice(&len);
    f
}

fn u16_at(p: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([p[at], p[at + 1]])
}

/// The string at packet offset `at`, up to its NUL.
fn name_at(p: &[u8], at: u16) -> String {
    let s = &p[usize::from(at)..];
    let end = s.iter().position(|&b| b == 0).expect("a NUL ends the name");
    String::from_utf8(s[..end].to_vec()).expect("a name is UTF-8")
}

/// The reply's header: start code, length, sequence, opcode, status.
fn head(p: &[u8]) -> (u16, u16, u16, u16, u16) {
    (
        u16_at(p, 0),
        u16_at(p, 2),
        u16_at(p, 4),
        u16_at(p, 6),
        u16_at(p, 8),
    )
}

/// The reply to `req` and the change it made (`None`: no change).
fn answered(outcome: ArcOutcome) -> (Vec<u8>, Option<Change>) {
    match outcome {
        ArcOutcome::Changed(bytes, change) => (bytes, Some(change)),
        ArcOutcome::Reply(bytes) => (bytes, None),
        other => panic!("not answered: {other:?}"),
    }
}

/// A reply of status OK and no body to `req`, under 0x2809.
fn ok_without_body(reply: &[u8], seq: u16, op1: u16) {
    assert_eq!(head(reply), (START_2809, 10, seq, op1, STATUS_OK));
}

/// A rename's change: RX channels (from 0) renamed.
fn rx_renamed(rx: &[usize]) -> Change {
    Change {
        renamed_rx: rx.to_vec(),
        ..Change::default()
    }
}

#[test]
fn the_fixture_s_frames_read_as_it_states() {
    let set = frame("0x3410 set");
    assert_eq!(head(&set), (START_2809, 48, 0x0104, SUBSCRIBE_2809, 0));
    // 6 zero bytes, the record size, [slots, count], a record at 20.
    assert_eq!(set[10..20], [0, 0, 0, 0, 0, 0, 8, 0, 2, 1]);
    assert_eq!([u16_at(&set, 20), u16_at(&set, 22)], [1, 3], "RX 1, audio");
    assert_eq!(name_at(&set, u16_at(&set, 24)), "Out-1");
    assert_eq!(name_at(&set, u16_at(&set, 26)), "dev-a");
    let clear = frame("0x3410 clear");
    assert_eq!([u16_at(&clear, 24), u16_at(&clear, 26)], [0, 0]);
    let two = frame("0x3410 two");
    assert_eq!(two[18..20], [32, 2]);
    assert_eq!(name_at(&two, u16_at(&two, 34)), "dev-b");
    let rx = frame("0x3401");
    assert_eq!(head(&rx).3, RX_RENAME_2809);
    assert_eq!(rx[16..20], [6, 0, 1, 1]);
    assert_eq!([u16_at(&rx, 20), u16_at(&rx, 22)], [2, 3]);
    assert_eq!(name_at(&rx, u16_at(&rx, 24)), "Lead-in");
    let tx = frame("0x2438");
    assert_eq!(head(&tx).3, TX_NAMES_2809);
    assert_eq!(tx[16..20], [6, 0, 2, 2]);
    assert_eq!(name_at(&tx, u16_at(&tx, 30)), "Mix-L");
}

#[test]
fn netaudio_s_2809_subscribe_subscribes_our_rx_channel() {
    let mut dev = Dev::new(config());
    let (reply, change) = answered(dev.send(&frame("0x3410 set")));
    ok_without_body(&reply, 0x0104, SUBSCRIBE_2809);
    assert_eq!(change, Some(subs_changed(&[0])));
    assert_eq!(dev.state.subscription(0), Some(&sub("Out-1", "dev-a")));
    assert_eq!(dev.state.subscription(1), None);
    // The same again changes nothing and is still OK.
    let (reply, change) = answered(dev.send(&frame("0x3410 set")));
    ok_without_body(&reply, 0x0104, SUBSCRIBE_2809);
    assert_eq!(change, None);
}

#[test]
fn netaudio_s_2809_unsubscribe_clears_our_rx_channel() {
    let mut dev = Dev::new(config());
    dev.state
        .set_subscription(&dev.cfg, 0, Some(sub("TX 9", "peer-device")));
    dev.state
        .set_subscription(&dev.cfg, 1, Some(sub("TX 8", "peer-device")));
    let (reply, change) = answered(dev.send(&frame("0x3410 clear")));
    ok_without_body(&reply, 0x0105, SUBSCRIBE_2809);
    assert_eq!(change, Some(subs_changed(&[0])));
    assert_eq!(dev.state.subscription(0), None);
    assert_eq!(dev.state.subscription(1), Some(&sub("TX 8", "peer-device")));
}

#[test]
fn a_2809_subscription_page_of_32_slots_subscribes_each_record() {
    let mut dev = Dev::new(config());
    let (reply, change) = answered(dev.send(&frame("0x3410 two")));
    ok_without_body(&reply, 0x0106, SUBSCRIBE_2809);
    assert_eq!(change, Some(subs_changed(&[0, 1])));
    assert_eq!(dev.state.subscription(0), Some(&sub("Out-1", "dev-a")));
    assert_eq!(dev.state.subscription(1), Some(&sub("Out-2", "dev-b")));
}

/// A record of another media than audio (we have no video channel), a
/// channel we do not have and channel 0 are passed over, as 2.7's 0x3010
/// passes over the channels it cannot set: OK, nothing changed.
#[test]
fn a_2809_subscription_record_we_cannot_take_is_passed_over() {
    let video = edited("0x3410 set", |f| f[23] = 2);
    let missing = edited("0x3410 set", |f| f[21] = 40);
    let zero = edited("0x3410 set", |f| f[21] = 0);
    for (what, req) in [("video", video), ("missing", missing), ("zero", zero)] {
        let mut dev = Dev::new(config());
        let (reply, change) = answered(dev.send(&req));
        ok_without_body(&reply, 0x0104, SUBSCRIBE_2809);
        assert_eq!(change, None, "{what}");
        assert!(dev.state.subscriptions.is_empty(), "{what}");
    }
}

#[test]
fn netaudio_s_2809_rx_rename_renames_our_rx_channel() {
    let mut dev = Dev::new(config());
    let (reply, change) = answered(dev.send(&frame("0x3401")));
    ok_without_body(&reply, 0x0101, RX_RENAME_2809);
    assert_eq!(change, Some(rx_renamed(&[1])));
    assert_eq!(dev.state.rx[1], "Lead-in");
    assert_eq!(dev.state.rx[0], dev.cfg.rx[0]);
}

/// The names a 0x2438 reply carries, read as netaudio's parser reads them:
/// 6 zero bytes, `06 00`, `[n, n]`, `n` records of 6 bytes from packet byte
/// 20 (channel, 3, the name's offset), the names after the records.
fn tx_names_reply(p: &[u8]) -> Vec<(u16, String)> {
    assert_eq!(usize::from(u16_at(p, 2)), p.len(), "the length field");
    assert_eq!(p[10..18], [0, 0, 0, 0, 0, 0, 6, 0]);
    let n = usize::from(p[18]);
    assert_eq!(p[19], p[18], "as many records as stated");
    let table_end = 20 + 6 * n;
    (0..n)
        .map(|k| {
            let at = 20 + 6 * k;
            assert_eq!(u16_at(p, at + 2), 3, "an audio channel");
            let name = u16_at(p, at + 4);
            assert!(usize::from(name) >= table_end, "the name after the records");
            (u16_at(p, at), name_at(p, name))
        })
        .collect()
}

#[test]
fn netaudio_s_2809_tx_names_rename_our_tx_channels_and_echo_them() {
    let mut dev = Dev::new(config());
    let (reply, change) = answered(dev.send(&frame("0x2438")));
    let (start, _, seq, op1, status) = head(&reply);
    assert_eq!(
        (start, seq, op1, status),
        (START_2809, 0x0103, TX_NAMES_2809, STATUS_OK)
    );
    assert_eq!(change, Some(Change::default()), "a TX rename names no RX");
    assert_eq!(
        tx_names_reply(&reply),
        [(1, "Out-1".to_string()), (3, "Mix-L".to_string())]
    );
    assert_eq!(dev.state.tx[0], "Out-1");
    assert_eq!(dev.state.tx[1], dev.cfg.tx[1]);
    assert_eq!(dev.state.tx[2], "Mix-L");
    // Again: nothing changes, the same answer.
    let (again, change) = answered(dev.send(&frame("0x2438")));
    assert_eq!(change, None);
    assert_eq!(again, reply);
}

/// netaudio 0.4.2 renames a TX channel with 2.7's 0x2013 under 0x2809; it
/// is answered as 2.7's, under the request's start code.
#[test]
fn netaudio_s_tx_rename_under_2809_renames_as_2_7_s() {
    let mut dev = Dev::new(config());
    let (reply, change) = answered(dev.send(&frame("0x2013")));
    assert_eq!(
        head(&reply),
        (START_2809, 12, 0x0102, op::TX_RENAME, STATUS_OK)
    );
    assert_eq!(reply[10..], [0, 0]);
    assert_eq!(change, Some(Change::default()));
    assert_eq!(dev.state.tx[1], "Out-2");
}

/// A 2.8.9 write is saved before its OK goes out, as the 2.7 writes.
#[test]
fn the_2809_writes_are_saved() {
    let dir = tempfile::tempdir().unwrap();
    let mut device = Device::load(config_in(dir.path())).unwrap();
    for name in ["0x3410 set", "0x3401", "0x2438"] {
        let (reply, change) = answered(respond_idle(&mut device, &frame(name)));
        assert_eq!(head(&reply).4, STATUS_OK, "{name}");
        assert!(change.is_some(), "{name}");
    }
    let saved = Device::load(device.cfg.clone()).unwrap();
    assert_eq!(saved.state.subscription(0), Some(&sub("Out-1", "dev-a")));
    assert_eq!(saved.state.rx[1], "Lead-in");
    assert_eq!(
        (&*saved.state.tx[0], &*saved.state.tx[2]),
        ("Out-1", "Mix-L")
    );
}

/// A 2.8.9 write we cannot read, or a rename we cannot make, is refused
/// and changes nothing (each case breaks one thing in netaudio's frame).
#[test]
fn a_2809_write_we_cannot_carry_out_is_refused_and_changes_nothing() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        // The record size word is not the subscription's.
        ("subscribe size", edited("0x3410 set", |f| f[16] = 6)),
        // The record runs past the datagram.
        ("subscribe cut", edited("0x3410 clear", |f| f.truncate(27))),
        // The header alone.
        ("subscribe empty", edited("0x3410 set", |f| f.truncate(10))),
        ("rename size", edited("0x3401", |f| f[16] = 8)),
        ("rename cut", edited("0x3401", |f| f.truncate(25))),
        ("rename none", edited("0x3401", |f| f[19] = 0)),
        // A video channel: we have none.
        ("rename video", edited("0x3401", |f| f[23] = 2)),
        // A channel we do not have.
        ("rename missing", edited("0x3401", |f| f[21] = 40)),
        // A name Dante refuses ('=').
        ("rename invalid", edited("0x3401", |f| f[28] = b'=')),
        // The name's offset past the datagram.
        ("rename offset", edited("0x3401", |f| f[25] = 0x40)),
        ("names size", edited("0x2438", |f| f[16] = 8)),
        // Two records stated, one there.
        ("names cut", edited("0x2438", |f| f.truncate(29))),
        ("names none", edited("0x2438", |f| f[19] = 0)),
        // The second record's channel is a video one.
        ("names video", edited("0x2438", |f| f[29] = 2)),
        // TX 3 to take the name TX 1 takes: the names would clash.
        (
            "names clash",
            edited("0x2438", |f| {
                let at = f.len() - 6;
                f[at..at + 5].copy_from_slice(b"Out-1");
            }),
        ),
    ];
    for (what, req) in cases {
        let mut dev = Dev::new(config());
        let before = dev.state.clone();
        let (h, _) = Header::parse(&req).expect("the edited frame parses");
        let (reply, change) = answered(dev.send(&req));
        assert_eq!(
            head(&reply),
            (START_2809, 10, h.seq, h.op1, STATUS_ERROR),
            "{what}"
        );
        assert_eq!(change, None, "{what}");
        assert_eq!(dev.state, before, "{what}");
    }
}

/// The 2.7 writes are answered as before under 2.7's start codes, and the
/// 2.8.9 opcodes stay unanswered under them (2.8.9's own).
#[test]
fn the_2809_write_opcodes_are_2809_s_only() {
    for name in ["0x3410 set", "0x3401", "0x2438"] {
        let req = edited(name, |f| f[0..2].copy_from_slice(&NETAUDIO.to_be_bytes()));
        let (h, _) = Header::parse(&req).unwrap();
        let mut dev = Dev::new(config());
        assert_eq!(
            dev.send(&req),
            ArcOutcome::Unanswered { op1: h.op1 },
            "{name}"
        );
    }
}

/// A 0x2438 of `records` (TX channel, name), in netaudio's layout.
fn tx_names_frame(records: &[(u16, &str)]) -> Vec<u8> {
    let n = u8::try_from(records.len()).unwrap();
    let mut body = vec![0, 0, 0, 0, 0, 0, 6, 0, n, n];
    let mut at = 20 + 6 * records.len();
    let mut names = Vec::new();
    for &(channel, name) in records {
        body.extend_from_slice(&channel.to_be_bytes());
        body.extend_from_slice(&3u16.to_be_bytes());
        body.extend_from_slice(&u16::try_from(at).unwrap().to_be_bytes());
        names.extend_from_slice(name.as_bytes());
        names.push(0);
        at += name.len() + 1;
    }
    body.extend_from_slice(&names);
    request(START_2809, 9, TX_NAMES_2809, 0, &body)
}

/// 38 records are taken, each answered; a 39th is refused before any
/// change: an answer of 39 records of 31-byte names would not fit one
/// datagram.
#[test]
fn a_2438_takes_at_most_38_records() {
    let names: Vec<String> = (0..39).map(|k| format!("Name-{k:02}")).collect();
    let records: Vec<(u16, &str)> = names
        .iter()
        .enumerate()
        .map(|(k, n)| ((k % 16) as u16 + 1, n.as_str()))
        .collect();
    let mut dev = Dev::new(config());
    let (reply, change) = answered(dev.send(&tx_names_frame(&records[..38])));
    assert_eq!(head(&reply).4, STATUS_OK);
    assert!(change.is_some());
    let echoed = tx_names_reply(&reply);
    assert_eq!(echoed.len(), 38);
    // Each record carries the name its channel now has: the last one asked.
    assert_eq!(echoed[0], (1, "Name-32".to_string()));
    assert_eq!(dev.state.tx[5], "Name-37");
    let before = dev.state.clone();
    let (reply, change) = answered(dev.send(&tx_names_frame(&records)));
    assert_eq!(head(&reply).4, STATUS_ERROR);
    assert_eq!(change, None);
    assert_eq!(dev.state, before);
}

/// A body that ends right after its records reads: the strings may be
/// elsewhere (here none, an unsubscribe).
#[test]
fn a_2809_write_that_ends_with_its_records_reads() {
    let mut dev = Dev::new(config());
    dev.state
        .set_subscription(&dev.cfg, 0, Some(sub("TX 9", "peer-device")));
    let req = edited("0x3410 clear", |f| f.truncate(28));
    let (reply, change) = answered(dev.send(&req));
    ok_without_body(&reply, 0x0105, SUBSCRIBE_2809);
    assert_eq!(change, Some(subs_changed(&[0])));
}

/// netaudio 0.4.2's reset_channel_name (2.7's 0x3001 and 0x2013 under
/// 0x27ff) points the name's offset at the datagram's end, with no byte
/// after it: an empty name, Dante's reset to the channel's default.
#[test]
fn netaudio_s_reset_with_the_name_at_the_datagram_s_end_resets() {
    let mut dev = Dev::new(config());
    dev.state.rx[0] = "In-1".into();
    dev.state.tx[1] = "Out-2".into();
    let rx = frame("0x3001 reset");
    assert_eq!(
        usize::from(u16_at(&rx, 14)),
        rx.len(),
        "the offset is the end"
    );
    let (reply, change) = answered(dev.send(&rx));
    assert_eq!(
        head(&reply),
        (NETAUDIO, 10, 0x0107, op::RX_RENAME, STATUS_OK)
    );
    assert_eq!(change, Some(rx_renamed(&[0])));
    assert_eq!(dev.state.rx[0], dev.cfg.rx[0]);
    let tx = frame("0x2013 reset");
    assert_eq!(
        usize::from(u16_at(&tx, 16)),
        tx.len(),
        "the offset is the end"
    );
    let (reply, change) = answered(dev.send(&tx));
    assert_eq!(
        head(&reply),
        (NETAUDIO, 12, 0x0108, op::TX_RENAME, STATUS_OK)
    );
    assert_eq!(change, Some(Change::default()));
    assert_eq!(dev.state.tx[1], dev.cfg.tx[1]);
    // An offset one past the end is still no name: refused.
    let past = edited("0x3001 reset", |f| f[15] = 0x15);
    let (reply, change) = answered(dev.send(&past));
    assert_eq!(head(&reply).4, STATUS_ERROR);
    assert_eq!(change, None);
}

/// Our RX channels state netaudio's can-subscribe-self bit (0x0008) in
/// both generations' records: 2.7's 0x3000 (flags, the record's second
/// word) as 2.8.9's 0x3400 (`v289.rs`'s test). Without it netaudio refuses
/// a subscription to our own TX before it sends one.
#[test]
fn our_2_7_rx_records_state_self_subscription_too() {
    let mut dev = Dev::new(config_sized(3, 2));
    let page = dev.ask(3, op::RX_CHANNELS, &page_request(1));
    assert_eq!((page.body[0], page.body[1]), (3, 3));
    for k in 0..3 {
        let at = 2 + 20 * k;
        assert_eq!(u16_at(&page.body, at), k as u16 + 1, "the channel");
        assert_eq!(u16_at(&page.body, at + 2), 0x000e, "the flags");
    }
}
