// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! Dante Controller's own ARC 2.8.9 writes to our device, as captured
//! (`tests/fixtures/arc_dc_289_writes.json`, dstack #220): with 0x1000's
//! bit 0x1000 stated it subscribes with 0x3410, renames an RX channel
//! with 0x3401 and a TX channel with 0x2438, the same writes netaudio
//! sends a 2.8.9 device. Dante Controller puts a TX channel back to its
//! default name with a name offset of 0, which our device refused.
//!
//! The replies are read with this file's own parser, never with the code
//! under test.

use super::*;

use crate::arc::flows::tests::hex;

const DC_WRITES: &str = include_str!("../../../tests/fixtures/arc_dc_289_writes.json");

fn fixture() -> serde_json::Value {
    serde_json::from_str(DC_WRITES).expect("the fixture is JSON")
}

/// Dante Controller's request (`"requests"`) or our recorded answer
/// (`"replies"`) `name`, the whole datagram.
fn dc(kind: &str, name: &str) -> Vec<u8> {
    hex(fixture()[kind][name]
        .as_str()
        .expect("the frame is in the fixture"))
}

fn u16_at(p: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([p[at], p[at + 1]])
}

/// The reply to `req` and the change it made (`None`: no change).
fn answered(outcome: ArcOutcome) -> (Vec<u8>, Option<Change>) {
    match outcome {
        ArcOutcome::Changed(bytes, change) => (bytes, Some(change)),
        ArcOutcome::Reply(bytes) => (bytes, None),
        other => panic!("not answered: {other:?}"),
    }
}

/// The string at packet offset `at`, up to its NUL.
fn name_at(p: &[u8], at: u16) -> String {
    let s = &p[usize::from(at)..];
    let end = s.iter().position(|&b| b == 0).expect("a NUL ends the name");
    String::from_utf8(s[..end].to_vec()).expect("a name is UTF-8")
}

/// Dante Controller's session, replayed on one device: each answer is the
/// one Dante Controller took from our device, and each change is made.
#[test]
fn dante_controller_s_2809_writes_change_our_device_as_it_showed() {
    let mut dev = Dev::new(config());
    let mut replay = |name: &str| {
        let (reply, change) = answered(dev.send(&dc("requests", name)));
        assert_eq!(reply, dc("replies", name), "{name}");
        change
    };
    assert_eq!(replay("0x3410 set"), Some(subs_changed(&[0])));
    assert_eq!(replay("0x3401").map(|c| c.renamed_rx), Some(vec![1]));
    assert_eq!(replay("0x2438"), Some(Change::default()));
    assert_eq!(dev.state.subscription(0), Some(&sub("TX 1", "test-dev-01")));
    assert_eq!((&*dev.state.rx[1], &*dev.state.tx[1]), ("RX 2r", "TX 2r"));
    let (reply, change) = answered(dev.send(&dc("requests", "0x3401 back")));
    assert_eq!(reply, dc("replies", "0x3401 back"));
    assert_eq!(change.map(|c| c.renamed_rx), Some(vec![1]));
    assert_eq!(dev.state.rx[1], "RX 2");
    let (reply, change) = answered(dev.send(&dc("requests", "0x3410 clear")));
    assert_eq!(reply, dc("replies", "0x3410 clear"));
    assert_eq!(change, Some(subs_changed(&[0])));
    assert_eq!(dev.state.subscription(0), None);
}

/// Dante Controller's "back to the default" for a TX channel: a 0x2438
/// record whose name offset is 0. The channel takes its default name, and
/// the answer's record carries it.
#[test]
fn a_2809_name_offset_of_0_is_the_default_name() {
    let mut dev = Dev::new(config());
    dev.state.tx[1] = "TX 2r".into();
    let req = dc("requests", "0x2438 default");
    assert_eq!(u16_at(&req, 24), 0, "no name");
    let (reply, change) = answered(dev.send(&req));
    assert_eq!(
        [u16_at(&reply, 0), u16_at(&reply, 4), u16_at(&reply, 6)],
        [0x2809, 0x00eb, 0x2438]
    );
    assert_eq!(u16_at(&reply, 8), STATUS_OK, "answered, not refused");
    assert_eq!(change, Some(Change::default()));
    assert_eq!(dev.state.tx[1], dev.cfg.tx[1]);
    // One record: TX 2, audio, the name it now carries.
    assert_eq!(reply[16..20], [6, 0, 1, 1]);
    assert_eq!([u16_at(&reply, 20), u16_at(&reply, 22)], [2, 3]);
    assert_eq!(name_at(&reply, u16_at(&reply, 24)), dev.cfg.tx[1]);
    // The same for an RX channel (0x3401).
    dev.state.rx[1] = "RX 2r".into();
    let mut rx = dc("requests", "0x3401");
    rx[24..26].copy_from_slice(&[0, 0]);
    let (reply, change) = answered(dev.send(&rx));
    assert_eq!(u16_at(&reply, 8), STATUS_OK);
    assert_eq!(change.map(|c| c.renamed_rx), Some(vec![1]));
    assert_eq!(dev.state.rx[1], dev.cfg.rx[1]);
}
