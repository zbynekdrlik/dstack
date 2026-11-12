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
            let channels = (0..slots).map(|s| u16_at(&bytes, at + 18 + 2 * s)).collect();
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
