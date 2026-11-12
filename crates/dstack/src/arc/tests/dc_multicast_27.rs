// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! Dante Controller's ARC 2.7 multicast create (0x2201), as captured
//! (`tests/fixtures/arc_dc_multicast_27.json`, dstack #222): with 0x1000
//! stating multicast TX (0x0020) and not segmented flows (0x1000), Dante
//! Controller creates a multicast flow with 2.7's 0x2201 (under start code
//! 0x2809), not with 2.8.9's 0x2601 (#193). Our device refused it, so no
//! flow was made and Dante Controller showed none.
//!
//! Our answer and the 2.7 list (0x2200) after it are read with this file's
//! own reader, never with the code under test.

use super::*;

use crate::arc::flows::tests::hex;
use crate::device::SavedMulticast;

const DC_MULTICAST: &str = include_str!("../../../tests/fixtures/arc_dc_multicast_27.json");

fn fixture() -> serde_json::Value {
    serde_json::from_str(DC_MULTICAST).expect("the fixture is JSON")
}

/// Dante Controller's request `name`, the whole datagram.
fn dc(name: &str) -> Vec<u8> {
    hex(fixture()["requests"][name]
        .as_str()
        .expect("the frame is in the fixture"))
}

/// The 2.7 TX flow list (0x2200) from its first flow, as Dante Controller
/// asks for it after a create: each listed flow's id, type and TX channels
/// (this file's own reader of inferno's page: `[slots, count]`, then each
/// flow descriptor's packet offset; a descriptor is the flow id, its type,
/// the rate `u32`, 0, the encoding, 1, the slot count, the socket
/// descriptor's offset, each slot's TX channel).
fn listed(device: &mut Device, flows: &NoFlows) -> Vec<(u16, u16, Vec<u16>)> {
    let page = request(0x2809, 0x77, op::TX_FLOWS, 0, &[0, 1, 0, 1, 0, 0]);
    let (bytes, change) = answered(answer(device, flows, &page));
    assert_eq!(change, None, "a list changes nothing");
    assert_eq!(u16_at(&bytes, 8), STATUS_OK);
    let count = usize::from(bytes[HEADER_LEN + 1]);
    (0..count)
        .map(|k| {
            let at = usize::from(u16_at(&bytes, HEADER_LEN + 2 + 2 * k));
            let slots = usize::from(u16_at(&bytes, at + 14));
            let channels = (0..slots)
                .map(|s| u16_at(&bytes, at + 18 + 2 * s))
                .collect();
            (u16_at(&bytes, at), u16_at(&bytes, at + 2), channels)
        })
        .collect()
}

#[test]
fn dante_controller_s_2_7_multicast_create_makes_the_flow_it_names() {
    let dir = tempfile::tempdir().unwrap();
    let mut device = Device::load(config_in(dir.path())).unwrap();
    let flows = NoFlows::new(&device);
    let create = dc("0x2201");
    let (bytes, change) = answered(answer(&mut device, &flows, &create));
    // Answered OK under its own start code, sequence and opcode.
    assert_eq!(&bytes[..2], &create[..2]);
    assert_eq!((u16_at(&bytes, 4), u16_at(&bytes, 6)), (0x00cd, 0x2201));
    assert_eq!(u16_at(&bytes, 8), STATUS_OK, "made, not refused");
    assert_eq!(change, Some(Change::TX_FLOWS_ONLY));
    // The flow Dante Controller named: flow 32 (slot 31 from 0), TX 1 and
    // TX 2, multicast, to its group; kept in the state and its file, as a
    // 2.8.9 create keeps one (dstack #207).
    let saved = [SavedMulticast {
        slot: 31,
        channels: vec![1, 2],
    }];
    assert_eq!(device.state.multicast, saved);
    let back = DeviceState::load_or_default(&device.cfg).unwrap();
    assert_eq!(back.multicast, saved, "in the state file");
    {
        let tx = flows.tx.lock().unwrap();
        let f = tx.flow(31).expect("slot 31 holds the flow");
        assert!(f.multicast);
        assert_eq!(f.channels, [Some(1), Some(2)]);
        assert_eq!(
            f.dest,
            crate::media::tx::multicast_group(OUR_IP, device.cfg.process_id, 31)
        );
        assert_eq!(tx.flows().count(), 1, "that flow only");
    }
    // Dante Controller reads the 2.7 list next: the flow is there, type 2.
    assert_eq!(listed(&mut device, &flows), [(32, 0x0002, vec![1, 2])]);
}

#[test]
fn dante_controller_s_2_7_multicast_delete_ends_the_flow_it_names() {
    let dir = tempfile::tempdir().unwrap();
    let mut device = Device::load(config_in(dir.path())).unwrap();
    let flows = NoFlows::new(&device);
    let (_, made) = answered(answer(&mut device, &flows, &dc("0x2201")));
    assert_eq!(made, Some(Change::TX_FLOWS_ONLY));
    let delete = dc("0x2202");
    let (bytes, change) = answered(answer(&mut device, &flows, &delete));
    assert_eq!(&bytes[..2], &delete[..2]);
    assert_eq!((u16_at(&bytes, 4), u16_at(&bytes, 6)), (0x00da, 0x2202));
    assert_eq!(u16_at(&bytes, 8), STATUS_OK, "deleted, not refused");
    assert_eq!(bytes.len(), HEADER_LEN, "no body");
    assert_eq!(change, Some(Change::TX_FLOWS_ONLY));
    // Gone from the TX table, the state and its file, and from the list.
    assert_eq!(flows.tx.lock().unwrap().flow(31), None);
    assert!(device.state.multicast.is_empty());
    let back = DeviceState::load_or_default(&device.cfg).unwrap();
    assert!(back.multicast.is_empty(), "{back:?}");
    assert_eq!(listed(&mut device, &flows), []);
}

/// A device of its own in `dir`, with no flow.
fn fresh(dir: &Path) -> (Device, NoFlows) {
    let device = Device::load(config_in(dir)).unwrap();
    let flows = NoFlows::new(&device);
    (device, flows)
}

/// Our answer's status to `packet` and whether it changed the state.
fn status(device: &mut Device, flows: &NoFlows, packet: &[u8]) -> (u16, bool) {
    let (bytes, change) = answered(answer(device, flows, packet));
    (u16_at(&bytes, 8), change.is_some())
}

/// Dante Controller's create with the big-endian `word` at packet offset
/// `at`.
fn create_with(at: usize, word: &[u8]) -> Vec<u8> {
    let mut p = dc("0x2201");
    p[at..at + word.len()].copy_from_slice(word);
    p
}

#[test]
fn a_2_7_create_dante_controller_would_not_send_is_refused_and_makes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut device, flows) = fresh(dir.path());
    let other_rate = (device.cfg.sample_rate / 2).to_be_bytes();
    let other_bits = (if device.cfg.encoding == 16 { 24u16 } else { 16 }).to_be_bytes();
    // Each breaks one check of the captured frame (packet offsets: the
    // record count at 11, its offset at 12, the flow id at 16, its type at
    // 18, the rate at 20, the encoding at 26, the channel count at 30, the
    // channels from 32).
    for (what, packet) in [
        ("two records", create_with(11, &[2])),
        (
            "a record offset inside the header",
            create_with(12, &[0, 9]),
        ),
        (
            "a record offset past the datagram",
            create_with(12, &[0, 0xff]),
        ),
        ("channels past the datagram", create_with(30, &[0, 0x20])),
        ("a unicast flow", create_with(18, &[0, 0x11])),
        ("another rate", create_with(20, &other_rate)),
        ("another encoding", create_with(26, &other_bits)),
        ("flow 0", create_with(16, &[0, 0])),
        ("a channel we do not have", create_with(34, &[0, 99])),
    ] {
        assert_eq!(
            status(&mut device, &flows, &packet),
            (STATUS_ERROR, false),
            "{what}"
        );
        assert!(device.state.multicast.is_empty(), "{what}");
        assert_eq!(flows.tx.lock().unwrap().flows().count(), 0, "{what}");
    }
    // Our own rate and encoding, stated: made.
    let rate = device.cfg.sample_rate.to_be_bytes();
    let bits = u16::from(device.cfg.encoding).to_be_bytes();
    let mut ours = create_with(20, &rate);
    ours[26..28].copy_from_slice(&bits);
    assert_eq!(status(&mut device, &flows, &ours), (STATUS_OK, true));
    // A flow in its slot already: the second create is refused.
    assert_eq!(
        status(&mut device, &flows, &dc("0x2201")),
        (STATUS_ERROR, false)
    );
    assert_eq!(device.state.multicast.len(), 1);
}

#[test]
fn a_2_7_create_is_answered_under_any_start_code_but_not_with_op2() {
    let dir = tempfile::tempdir().unwrap();
    let (mut device, flows) = fresh(dir.path());
    // netaudio's legacy encoder sends it under 0x2729.
    let netaudio = create_with(0, &[0x27, 0x29]);
    let (bytes, change) = answered(answer(&mut device, &flows, &netaudio));
    assert_eq!((u16_at(&bytes, 0), u16_at(&bytes, 8)), (0x2729, STATUS_OK));
    assert_eq!(change, Some(Change::TX_FLOWS_ONLY));
    // op2 other than 0: no such request, no reply.
    let mut flagged = create_with(16, &[0, 3]);
    flagged[9] = 1;
    assert_eq!(
        answer(&mut device, &flows, &flagged),
        ArcOutcome::Unanswered { op1: 0x2201 }
    );
    let mut delete = dc("0x2202");
    delete[9] = 1;
    assert_eq!(
        answer(&mut device, &flows, &delete),
        ArcOutcome::Unanswered { op1: 0x2202 }
    );
    assert_eq!(device.state.multicast.len(), 1, "flow 32 only");
}

/// Dante Controller's delete (start code, sequence) with the body `body`.
fn delete_of(body: &[u8]) -> Vec<u8> {
    request(0x2809, 0x00da, 0x2202, 0, body)
}

#[test]
fn a_2_7_delete_ends_every_flow_it_names_or_none() {
    let dir = tempfile::tempdir().unwrap();
    let (mut device, flows) = fresh(dir.path());
    let flow_3 = create_with(16, &[0, 3]);
    for packet in [dc("0x2201"), flow_3] {
        assert_eq!(status(&mut device, &flows, &packet), (STATUS_OK, true));
    }
    // Refused, ending none: no flow named, ids cut short, flow 0, one
    // flow we do not have beside one we do.
    for (what, body) in [
        ("a count of 0", vec![0, 0, 0, 0, 0, 32]),
        ("two announced, one there", vec![0, 2, 0, 0, 0, 32]),
        ("no count", vec![0]),
        ("flow 0", vec![0, 1, 0, 0, 0, 0]),
        ("flow 5 beside 32", vec![0, 2, 0, 0, 0, 32, 0, 5]),
    ] {
        assert_eq!(
            status(&mut device, &flows, &delete_of(&body)),
            (STATUS_ERROR, false),
            "{what}"
        );
        assert_eq!(device.state.multicast.len(), 2, "{what}");
        assert_eq!(flows.tx.lock().unwrap().flows().count(), 2, "{what}");
    }
    // Both named: both end.
    let both = delete_of(&[0, 2, 0, 0, 0, 32, 0, 3]);
    assert_eq!(status(&mut device, &flows, &both), (STATUS_OK, true));
    assert!(device.state.multicast.is_empty());
    assert_eq!(flows.tx.lock().unwrap().flows().count(), 0);
}
