// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! Dante Controller's own latency write (ARC 0x1101, dstack #216), as
//! captured (`tests/fixtures/arc_dc_latency_write.json`): `05, count`, then
//! `key, offset` pairs, the values at those frame offsets. Our device left
//! it unanswered (not netaudio's form), so Dante Controller's latency
//! change never reached it.

use super::*;

use crate::arc::flows::tests::hex;

const DC_WRITE: &str = include_str!("../../../tests/fixtures/arc_dc_latency_write.json");

/// Dante Controller's captured write: 10 ms to 5 ms.
fn dc_write() -> Vec<u8> {
    let f: serde_json::Value = serde_json::from_str(DC_WRITE).expect("the fixture is JSON");
    hex(f["requests"]["0x1101"]
        .as_str()
        .expect("the write is in the fixture"))
}

#[test]
fn dante_controller_s_latency_write_sets_the_latency_and_is_saved() {
    let frame = dc_write();
    let (h, _) = Header::parse(&frame).expect("the captured frame parses");
    assert_eq!((h.start, h.op1, h.op2), (CONTROLLER, op::SETTINGS_SET, 0));
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config_in(dir.path());
    cfg.latency_ns = 10_000_000;
    let mut device = Device::load(cfg.clone()).unwrap();
    match respond_idle(&mut device, &frame) {
        ArcOutcome::Changed(bytes, change) => {
            let (r, body) = Header::parse(&bytes).unwrap();
            assert_eq!(
                (r.start, r.seq, r.op1, r.op2, body.len()),
                (CONTROLLER, 0x00c2, op::SETTINGS_SET, STATUS_OK, 0)
            );
            assert_eq!(change.latency, Some(5_000_000));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(device.cfg.latency_ns, 5_000_000);
    assert_eq!(
        (device.state.latency_ns, device.state.config_latency_ns),
        (Some(5_000_000), Some(10_000_000))
    );
    // Saved: a restart with the same config keeps it.
    assert_eq!(Device::load(cfg).unwrap().cfg.latency_ns, 5_000_000);
}

/// The captured frame with `edit` applied to its bytes.
fn dc_write_with(edit: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut frame = dc_write();
    edit(&mut frame);
    let len = u16::try_from(frame.len()).unwrap().to_be_bytes();
    frame[2..4].copy_from_slice(&len);
    frame
}

/// Every keyed write but the latency's stays unanswered, as before: a
/// refusal once made Dante Controller ask again thousands of times a
/// second.
#[test]
fn a_keyed_write_of_anything_but_the_latency_stays_unanswered() {
    let set_u16 =
        |f: &mut Vec<u8>, at: usize, v: u16| f[at..at + 2].copy_from_slice(&v.to_be_bytes());
    let bad: Vec<(&str, Vec<u8>)> = vec![
        // Another first byte.
        ("tag", dc_write_with(|f| f[10] = 0x04)),
        // A key besides the latency's (the sample rate).
        ("key", dc_write_with(|f| set_u16(f, 12, 0x8020))),
        // Two different values.
        ("values", dc_write_with(|f| f[39] = 0x41)),
        // A value past the frame's end (0x28 is the frame's length).
        ("past", dc_write_with(|f| set_u16(f, 18, 0x0028))),
        // Both values inside the pairs (offset 0x10, so the same value).
        (
            "pairs",
            dc_write_with(|f| {
                set_u16(f, 14, 0x0010);
                set_u16(f, 18, 0x0010);
            }),
        ),
        // Values off the 4-byte grid: one byte more before them, the
        // offsets 0x21 and 0x25 (equal, in the frame, past the pairs).
        (
            "misaligned",
            dc_write_with(|f| {
                f.insert(32, 0);
                set_u16(f, 14, 0x0021);
                set_u16(f, 18, 0x0025);
            }),
        ),
        // The frame cut in the values.
        ("short", dc_write_with(|f| f.truncate(38))),
        // A count past the pairs the body holds.
        ("count", dc_write_with(|f| f[11] = 0x09)),
        // No pairs at all.
        ("empty", dc_write_with(|f| f[11] = 0x00)),
    ];
    for (what, frame) in bad {
        let dir = tempfile::tempdir().unwrap();
        let mut device = Device::load(config_in(dir.path())).unwrap();
        let before = device.clone();
        assert_eq!(
            respond_idle(&mut device, &frame),
            ArcOutcome::Unanswered {
                op1: op::SETTINGS_SET
            },
            "{what}"
        );
        assert_eq!(device, before, "{what}");
    }
}

/// One latency key alone is a latency write too, and its value may follow
/// the pairs at once (frame offset 0x10, where the pairs end).
#[test]
fn a_keyed_write_of_one_latency_key_sets_the_latency() {
    let body = [0x05, 0x01, 0x83, 0x01, 0x00, 0x10, 0x00, 0x1e, 0x84, 0x80];
    let frame = request(CONTROLLER, 9, op::SETTINGS_SET, 0, &body);
    let dir = tempfile::tempdir().unwrap();
    let mut device = Device::load(config_in(dir.path())).unwrap();
    assert!(decode(respond_idle(&mut device, &frame)).changed);
    assert_eq!(device.cfg.latency_ns, 2_000_000);
}
