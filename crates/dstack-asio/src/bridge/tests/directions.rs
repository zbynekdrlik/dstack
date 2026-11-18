// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #382 (dasio#120): each direction's own target, and each player's
//! ASRC started from the last run's locked rate.

use super::*;

/// With no override both directions keep `app_buffer_ns` (or the default),
/// so a bridge that sets nothing new runs as before.
#[test]
fn without_an_override_both_directions_keep_the_shared_target() {
    let at = opened(96_000.0, 128);
    let b = bridge(spec(1, 1));
    let default = b.app_buffer(&at);
    assert_eq!(default.target_ns(), 2_666_666, "128 + 128 frames");
    assert_eq!(b.to_asio_app_buffer(&at), default);
    assert_eq!(b.from_asio_app_buffer(&at), default);
    let mut s = spec(1, 1);
    s.app_buffer_ns = Some(1_666_667);
    let b = bridge(s);
    assert_eq!(b.to_asio_app_buffer(&at).target_ns(), 1_666_667);
    assert_eq!(b.from_asio_app_buffer(&at).target_ns(), 1_666_667);
}

/// Each direction's override wins for its own player only, over the shared
/// value and over the default; the players keep their own targets.
#[test]
fn each_direction_takes_its_own_target() {
    let at = opened(96_000.0, 128);
    let mut s = spec(1, 1);
    s.app_buffer_ns = Some(1_666_667);
    s.to_asio_app_buffer_ns = Some(1_333_333);
    let mut b = bridge(s);
    assert_eq!(b.to_asio_app_buffer(&at).target_ns(), 1_333_333);
    assert_eq!(b.from_asio_app_buffer(&at).target_ns(), 1_666_667);
    assert_eq!(b.app_buffer(&at).target_ns(), 1_666_667, "the shared one");
    let _dante = b.dante_side();
    let _asio = b.asio_side(&at).unwrap();
    assert!((b.to_asio_stats().target_ms() - 1.333333).abs() < 1e-9);
    assert!((b.from_asio_stats().target_ms() - 1.666667).abs() < 1e-9);
    // From-ASIO alone, over the default.
    let mut s = spec(1, 1);
    s.from_asio_app_buffer_ns = Some(2_000_000);
    let mut b = bridge(s);
    assert_eq!(b.to_asio_app_buffer(&at).target_ns(), 2_666_666);
    assert_eq!(b.from_asio_app_buffer(&at).target_ns(), 2_000_000);
    let _dante = b.dante_side();
    let _asio = b.asio_side(&at).unwrap();
    assert!((b.to_asio_stats().target_ms() - 2.666666).abs() < 1e-9);
    assert!((b.from_asio_stats().target_ms() - 2.0).abs() < 1e-9);
}

/// A direction's target out of range refuses the bridge with its reason:
/// since dstack #422 at `Bridge::new`, before any open (it refused the
/// open before; `floor.rs` has the floor's edges).
#[test]
fn a_direction_s_target_out_of_range_refuses_the_bridge() {
    for to in [true, false] {
        let mut s = spec(1, 1);
        if to {
            s.to_asio_app_buffer_ns = Some(400_000_000);
        } else {
            s.from_asio_app_buffer_ns = Some(400_000_000);
        }
        let key = if to {
            "to_asio_app_buffer_ns"
        } else {
            "from_asio_app_buffer_ns"
        };
        assert_eq!(
            Bridge::new(s).err().as_deref(),
            Some(format!("{key}: 400000000 ns (400 ms); 0.5 to 150 ms").as_str()),
            "{to}"
        );
    }
}

fn seed(to: f64, from: f64) -> Seed {
    Seed {
        dante_rate: RATE,
        driver_rate: 96_000,
        to_asio_ppm: Some(to),
        from_asio_ppm: Some(from),
    }
}

/// One driver switch and one pump block: each player publishes its
/// correction (the from-ASIO one taken from the switch first).
fn one_round(dante: &mut DanteSide, asio: &mut AsioSide) {
    let input = vec![0.0; 128];
    let mut output = vec![0.0; 128];
    asio.process(&mut Block::new(128, &input, &mut output));
    dante.process(&mut Block::new(128, &input, &mut output));
}

/// A seed at the open's rates starts each player at its rate; at other
/// rates (or with none) the players start at 0.
#[test]
fn an_open_at_the_seed_s_rates_starts_each_player_at_its_rate() {
    for (rate, expect) in [(96_000.0, (7.5, -7.25)), (48_000.0, (0.0, 0.0))] {
        let mut b = bridge(spec(1, 1));
        b.seed_from(seed(7.5, -7.25));
        assert_eq!(b.seed(), Some(seed(7.5, -7.25)), "the given one");
        let mut dante = b.dante_side().unwrap();
        let mut asio = b.asio_side(&opened(rate, 128)).unwrap();
        one_round(&mut dante, &mut asio);
        assert_eq!(
            (b.to_asio_stats().ppm(), b.from_asio_stats().ppm()),
            expect,
            "{rate}"
        );
        // Before any lock the run stands at what it started from; a run at
        // other rates knows none and keeps the seed it was given.
        assert_eq!(b.seed(), Some(seed(7.5, -7.25)), "{rate}");
    }
    let mut b = bridge(spec(1, 1));
    assert_eq!(b.seed(), None, "nothing known");
    let mut dante = b.dante_side().unwrap();
    let mut asio = b.asio_side(&opened(96_000.0, 128)).unwrap();
    one_round(&mut dante, &mut asio);
    assert_eq!(
        (b.to_asio_stats().ppm(), b.from_asio_stats().ppm()),
        (0.0, 0.0)
    );
    assert_eq!(b.seed(), None, "still nothing known");
}

/// A reopen at the seed's rates starts from it again; an open at other
/// rates in between neither uses nor loses it.
#[test]
fn a_reopen_starts_from_the_seed_again_after_an_open_at_other_rates() {
    let mut b = bridge(spec(1, 1));
    b.seed_from(seed(7.5, -7.25));
    let mut dante = b.dante_side().unwrap();
    for (rate, expect) in [
        (96_000.0, (7.5, -7.25)),
        (48_000.0, (0.0, 0.0)),
        (96_000.0, (7.5, -7.25)),
    ] {
        let mut asio = b.asio_side(&opened(rate, 128)).unwrap();
        one_round(&mut dante, &mut asio);
        assert_eq!(
            (b.to_asio_stats().ppm(), b.from_asio_stats().ppm()),
            expect,
            "{rate}"
        );
        assert_eq!(b.seed(), Some(seed(7.5, -7.25)), "{rate}");
        b.collect();
    }
}

/// A one-way bridge seeds and keeps only its own direction.
#[test]
fn a_one_way_bridge_keeps_only_its_direction_s_seed() {
    let mut b = bridge(spec(0, 1));
    b.seed_from(seed(7.5, -7.25));
    let _asio = b.asio_side(&opened_with(96_000.0, 128, 0, 1)).unwrap();
    let s = b.seed().unwrap();
    assert_eq!((s.to_asio_ppm, s.from_asio_ppm), (None, Some(-7.25)));
    let mut b = bridge(spec(1, 0));
    b.seed_from(seed(7.5, -7.25));
    let _asio = b.asio_side(&opened_with(96_000.0, 128, 1, 0)).unwrap();
    let s = b.seed().unwrap();
    assert_eq!((s.to_asio_ppm, s.from_asio_ppm), (Some(7.5), None));
}

/// What a run stands at: its own lock once the counts changed since the
/// open, else its seed.
#[test]
fn a_run_stands_at_its_own_lock_else_its_seed() {
    assert_eq!(standing(None, None, Some(7.5)), Some(7.5));
    assert_eq!(standing(None, None, None), None);
    assert_eq!(standing(Some(7.6), None, Some(7.5)), Some(7.6));
    assert_eq!(standing(Some(7.6), Some(7.0), Some(7.5)), Some(7.6));
    // The last run's lock, unchanged since the open: not this run's.
    assert_eq!(standing(Some(7.0), Some(7.0), Some(7.5)), Some(7.5));
    assert_eq!(standing(Some(7.0), Some(7.0), None), None);
}
