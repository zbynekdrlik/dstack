// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #422 (dasio#190): a direction's own target may go under dlink's
//! 1 ms app-buffer floor, down to the bridge's measured floor
//! (`MIN_BRIDGE_TARGET_NS`), while `app_buffer_ns` keeps dlink's.

use std::sync::atomic::{AtomicI64, Ordering};

use dlink::bridge::{MAX_APP_BUFFER_NS, MIN_APP_BUFFER_NS};

use super::*;

// The bridge's floor is under every dlink app's.
const _: () = assert!(MIN_BRIDGE_TARGET_NS < MIN_APP_BUFFER_NS);

/// The floor is 0.5 ms (the measured table on dstack #422), and the range
/// text names the key, the value and the bridge's floor.
#[test]
fn the_bridge_floor_is_half_a_millisecond() {
    assert_eq!(MIN_BRIDGE_TARGET_NS, 500_000);
    assert_eq!(MIN_APP_BUFFER_NS, 1_000_000, "dlink's apps keep theirs");
    for key in ["to_asio_app_buffer_ns", "from_asio_app_buffer_ns"] {
        assert_eq!(direction_target_problem(key, MIN_BRIDGE_TARGET_NS), None);
        assert_eq!(direction_target_problem(key, MAX_APP_BUFFER_NS), None);
        assert_eq!(
            direction_target_problem(key, MIN_BRIDGE_TARGET_NS - 1).as_deref(),
            Some(format!("{key}: 499999 ns (0.499999 ms); 0.5 to 150 ms").as_str())
        );
        assert_eq!(
            direction_target_problem(key, MAX_APP_BUFFER_NS + 1).as_deref(),
            Some(format!("{key}: 150000001 ns (150.000001 ms); 0.5 to 150 ms").as_str())
        );
    }
}

/// `Bridge::new` refuses a direction's target out of the bridge's range
/// with its text, and takes one at its floor; the shared `app_buffer_ns`
/// keeps dlink's 1 ms floor (refused at the open, as before).
#[test]
fn a_direction_s_target_is_checked_at_the_start_against_the_bridge_floor() {
    for to in [true, false] {
        let key = if to {
            "to_asio_app_buffer_ns"
        } else {
            "from_asio_app_buffer_ns"
        };
        let with = |ns: u64| {
            let mut s = spec(1, 1);
            if to {
                s.to_asio_app_buffer_ns = Some(ns);
            } else {
                s.from_asio_app_buffer_ns = Some(ns);
            }
            Bridge::new(s)
        };
        assert!(with(MIN_BRIDGE_TARGET_NS).is_ok(), "{key}");
        assert!(with(MAX_APP_BUFFER_NS).is_ok(), "{key}");
        for bad in [MIN_BRIDGE_TARGET_NS - 1, MAX_APP_BUFFER_NS + 1, 0] {
            assert_eq!(
                with(bad).err(),
                direction_target_problem(key, bad),
                "{key} {bad}"
            );
            assert!(with(bad).is_err(), "{key} {bad}");
        }
    }
    let at = opened(96_000.0, 128);
    let mut s = spec(1, 1);
    s.app_buffer_ns = Some(MIN_BRIDGE_TARGET_NS);
    let mut b = bridge(s);
    let shared = b.app_buffer(&at);
    assert_eq!(shared.floor_ns(), MIN_APP_BUFFER_NS);
    let problem = shared.problem();
    assert_eq!(
        problem.as_deref(),
        Some("app_buffer_ns: 500000 ns (0.5 ms); 1 to 150 ms")
    );
    assert_eq!(b.asio_side(&at).err(), problem);
    // One direction set in range does not hide the other's shared value
    // out of range.
    s.to_asio_app_buffer_ns = Some(MIN_BRIDGE_TARGET_NS);
    let mut b = bridge(s);
    assert_eq!(b.to_asio_app_buffer(&at).problem(), None);
    assert_eq!(b.asio_side(&at).err(), problem);
}

/// A direction's own target takes the bridge's floor: its player plays it
/// (never clamped up to 1 ms); a direction that follows the configured
/// shared value keeps dlink's, one on the default takes the bridge's too
/// (dstack #482, `default_floor.rs`).
#[test]
fn a_direction_s_own_target_plays_under_dlink_s_floor() {
    let at = opened(96_000.0, 128);
    let mut s = spec(1, 1);
    s.to_asio_app_buffer_ns = Some(MIN_BRIDGE_TARGET_NS);
    let mut b = bridge(s);
    let to = b.to_asio_app_buffer(&at);
    assert_eq!(to.floor_ns(), MIN_BRIDGE_TARGET_NS);
    assert_eq!(to.clamped_ns(), MIN_BRIDGE_TARGET_NS);
    let from = b.from_asio_app_buffer(&at);
    assert_eq!(
        from.floor_ns(),
        MIN_BRIDGE_TARGET_NS,
        "the default's (#482)"
    );
    assert_eq!(from, b.app_buffer(&at), "the default");
    let _dante = b.dante_side();
    let _asio = b.asio_side(&at).unwrap();
    assert!((b.to_asio_stats().target_ms() - 0.5).abs() < 1e-12);
    assert!((b.from_asio_stats().target_ms() - 2.666666).abs() < 1e-12);
    let mut s = spec(1, 1);
    s.from_asio_app_buffer_ns = Some(MIN_BRIDGE_TARGET_NS);
    s.app_buffer_ns = Some(1_500_000);
    let mut b = bridge(s);
    assert_eq!(b.from_asio_app_buffer(&at).floor_ns(), MIN_BRIDGE_TARGET_NS);
    assert_eq!(b.to_asio_app_buffer(&at).floor_ns(), MIN_APP_BUFFER_NS);
    let _dante = b.dante_side();
    let _asio = b.asio_side(&at).unwrap();
    assert!((b.from_asio_stats().target_ms() - 0.5).abs() < 1e-12);
    assert!((b.to_asio_stats().target_ms() - 1.5).abs() < 1e-12);
}

/// The driver's buffer of [`at_the_floor`], frames: a 64-frame driver at
/// 96 kHz (0.667 ms), the shape the floor holds both ways (a 128-frame one
/// needs 0.75 ms from ASIO, whose producer writes the whole buffer at once:
/// dstack #422's table).
const FLOOR_BUFFER: usize = 64;

/// Both directions at [`MIN_BRIDGE_TARGET_NS`] over 30 s of simulated
/// time: 96 kHz, a [`FLOOR_BUFFER`]-frame driver `drift` ppm off Dante's
/// clock, the pump's 32-frame blocks (the field's 0.333 ms), every block
/// naming its start and every FIFO on the simulated time, each player
/// seeded at its rate (as dasio keeps it, [`Seed`]). At ±50 ppm the two
/// clocks' phase crosses a whole driver buffer every 13.3 s, the moment a
/// target too short drops out (a 128-frame driver did at 23 s). The
/// to-ASIO side here is pump-written (32-frame pushes from the pump's
/// side; a pull needs a device), an easier producer than the field's pull
/// of a whole driver buffer a switch: the pulled shape's floor is the
/// ticket's simulation table, not this test.
fn at_the_floor(drift: f64) {
    let now = Arc::new(AtomicI64::new(0));
    let clock = Arc::clone(&now);
    let mut b = Bridge::with_clock(
        Spec {
            block_frames: 32,
            to_asio_app_buffer_ns: Some(MIN_BRIDGE_TARGET_NS),
            from_asio_app_buffer_ns: Some(MIN_BRIDGE_TARGET_NS),
            ..spec(1, 1)
        },
        Arc::new(move || clock.load(Ordering::Relaxed)),
    )
    .unwrap();
    b.seed_from(Seed {
        dante_rate: RATE,
        driver_rate: RATE,
        to_asio_ppm: Some(drift),
        from_asio_ppm: Some(-drift),
    });
    let mut dante = b.dante_side().unwrap();
    let mut asio = b
        .asio_side(&opened(f64::from(RATE), FLOOR_BUFFER as u32))
        .unwrap();
    let net = f64::from(RATE);
    let card = net * (1.0 + drift * 1e-6);
    let (mut dante_frames, mut asio_frames) = (0u64, 0u64);
    let input = [0.25; FLOOR_BUFFER];
    let mut output = [0.0; FLOOR_BUFFER];
    loop {
        let t_dante = dante_frames as f64 / net;
        let t_asio = asio_frames as f64 / card;
        let t = t_dante.min(t_asio);
        if t >= 30.0 {
            break;
        }
        now.store((t * 1e9) as i64, Ordering::Relaxed);
        if t_dante <= t_asio {
            dante.process(&mut Block::new(32, &input[..32], &mut output[..32]).at(dante_frames));
            dante_frames += 32;
        } else {
            asio.process(&mut Block::new(FLOOR_BUFFER, &input, &mut output).at(asio_frames));
            asio_frames += FLOOR_BUFFER as u64;
        }
    }
    for (what, st) in [("to", b.to_asio_stats()), ("from", b.from_asio_stats())] {
        assert!(
            (st.target_ms() - 0.5).abs() < 1e-12,
            "{what}: the floor plays"
        );
        assert!(!st.priming(), "{what}: playing");
        assert_eq!(
            (st.underruns(), st.recentres()),
            (0, 0),
            "{what} at {drift} ppm"
        );
    }
    assert_eq!(b.to_asio_fifo().unwrap().overflows(), 0);
}

#[test]
fn a_bridge_at_its_floor_plays_clean_50_ppm_fast() {
    at_the_floor(50.0);
}

#[test]
fn a_bridge_at_its_floor_plays_clean_50_ppm_slow() {
    at_the_floor(-50.0);
}
