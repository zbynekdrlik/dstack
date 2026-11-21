// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #476: the RX hand-off lead H (the owner's ROZHODNUTÉ of
//! 21.11.2026: 30 % of P, rounded down to whole samples). A pump that
//! leads runs each block H before its play time: due = start + B + P - H,
//! D = B + P + W - H, the play time (`play_at_ns`) unchanged.

use super::*;

/// The test device's P (96 samples, 1 ms) leads by 28 (28.8 rounded down).
const LEADING: Timing = Timing {
    leads: true,
    ..TIMING
};
const H: u64 = 28;

#[test]
fn h_is_30_percent_of_p_rounded_down_to_whole_samples() {
    assert_eq!(HANDOFF_LEAD_SHARE, (3, 10));
    for (p, h) in [
        (0, 0),
        (1, 0),
        (3, 0),
        (4, 1),
        (6, 1),
        (7, 2),
        (10, 3),
        (13, 3),
        (14, 4),
        (96, 28),
        // presenter-listen's 5 ms at 96 kHz: 1.5 ms.
        (480, 144),
        // obs on the editing PC, 13.041666 ms at 96 kHz: 3.906 ms.
        (1_252, 375),
        // A 40 ms P at 96 kHz: 12 ms.
        (3_840, 1_152),
    ] {
        assert_eq!(handoff_lead(p), h, "P {p}");
    }
    assert_eq!(handoff_lead(u64::MAX), u64::MAX / 10, "saturates");
}

#[test]
fn a_leading_timing_reads_and_stamps_h_earlier_and_plays_at_the_same_time() {
    assert_eq!((TIMING.lead(), TIMING.read_point()), (0, P));
    assert_eq!((LEADING.lead(), LEADING.read_point()), (H, P - H));
    assert_eq!(LEADING.offset(), D - H);
    // The config's switch reaches the timing.
    let mut cfg = DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-device.toml");
    assert!(!cfg.rx_handoff_lead, "off when left out");
    assert_eq!(Timing::of(&cfg), TIMING);
    cfg.rx_handoff_lead = true;
    assert_eq!(Timing::of(&cfg), LEADING);
    // The block is due H before its play time, which does not move.
    let mut b = Blocks::bootstrap(T + 5, LEADING);
    let start = b.start();
    let due = start + B + P - H;
    assert_eq!(b.due(), due);
    assert!((T + 5..T + 5 + B).contains(&due), "due {due}");
    assert_eq!(
        play_at_ns(start, LEADING, RATE, 0),
        play_at_ns(start, TIMING, RATE, 0)
    );
    assert_eq!(
        play_at_ns(start, LEADING, RATE, 0) - i64::try_from(ns_of(due, RATE)).unwrap(),
        i64::try_from(ns_of(H, RATE)).unwrap()
    );
    assert_eq!(b.next(due - 1), Due::Wait { until: due });
    assert_eq!(b.next(due), Due::Run { start, late: false });
    // The earliest block: due at B + P - H.
    assert_eq!(Blocks::bootstrap(0, LEADING).due(), B + P - H);
    // Late and the margin are judged from the earlier due time.
    let mut b = Blocks::bootstrap(T + 5, LEADING);
    let due = b.due();
    assert!(matches!(
        b.next(due + WORK_MARGIN + 1),
        Due::Rebootstrap {
            cause: Cause::Late,
            ..
        }
    ));
}

/// A re-anchor's room is judged on the leading due time too: due at
/// now + W + B moves, a block more stays.
#[test]
fn a_leading_re_anchor_at_the_edge_of_the_room_still_moves() {
    let t = Timing {
        leads: true,
        ..Timing::new(32, 96, 64)
    };
    let b0 = Blocks::bootstrap(T, t);
    // Due = start + 32 + 96 - 28 = start + 100 (+ the shift); the room ends
    // at now + 64 + 32. Without the lead (start + 128) the 64 would stay.
    let now = b0.start() + 100 + 64 - 96;
    let mut b = b0;
    assert_eq!(b.re_anchor(Some(64), now), (Shift::Moved, 0));
    let mut b = b0;
    assert_eq!(b.re_anchor(Some(96), now), (Shift::Kept, 0));
}

/// The pump with the lead wakes H before each block's play time and
/// writes each output D - H after its input: the TX stamp stays W after
/// the wake.
#[test]
fn a_leading_pump_wakes_h_early_and_stamps_its_outputs_w_after_the_wake() {
    let mut r = rig_at(
        Map {
            rx: vec![0],
            tx: vec![1],
        },
        &[0],
        &[1],
        LEADING,
    );
    fill(&r, 0, T - 1_000, 21_000, counter);
    let first = Blocks::bootstrap(T, LEADING).start();
    assert_eq!(r.pump.media(T, 0), first + B + P - H, "the first wake");
    run_to(&mut r, T, T + 9_600, 0);
    let blocks = r.stats.view().blocks;
    let n = (blocks * B) as usize;
    let (out, holes) = engine_at(&r, 1, first + D - H, n);
    assert_eq!(holes, 0);
    for (i, &v) in out.iter().enumerate() {
        let t = first + D - H + i as u64;
        assert_eq!(v, counter(t - (D - H)), "TX 2 at {t} is RX 1 at T - D + H");
    }
    let v = r.stats.view();
    assert_eq!(
        (v.offset, v.playout, v.rebootstraps),
        (D - H, P, 0),
        "{v:?}"
    );
    assert_eq!(r.stats.timing(), LEADING);
}

/// Dante Controller moves P: the lead follows with the same share (2 ms:
/// P 192, H 57), D with both.
#[test]
fn a_retime_keeps_the_share_of_the_new_p() {
    let mut r = rig_at(
        Map {
            rx: vec![0],
            tx: vec![0],
        },
        &[0],
        &[0],
        LEADING,
    );
    fill(&r, 0, T - 1_000, 30_000, counter);
    let mut now = T;
    for _ in 0..3 {
        now = r.pump.media(now, 7);
    }
    r.rx.set_device_latency(2_000_000);
    r.pump.media(now, 7);
    let v = r.stats.view();
    assert_eq!(
        (v.playout, v.offset),
        (192, B + 192 + WORK_MARGIN - 57),
        "{v:?}"
    );
    assert_eq!(v.rebootstraps, 1, "{v:?}");
    assert_eq!(r.stats.timing().lead(), 57);
}
