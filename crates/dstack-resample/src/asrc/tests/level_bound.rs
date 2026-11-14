// SPDX-License-Identifier: MIT OR Apache-2.0

//! The level loop's bound around the locked rate (dstack #303).

use super::*;

#[test]
fn the_bound_clamps_only_once_settled() {
    assert_eq!(bounded_level_ppm(30.0, Some(20.0), true), 20.0);
    assert_eq!(bounded_level_ppm(-30.0, Some(20.0), true), -20.0);
    assert_eq!(bounded_level_ppm(19.5, Some(20.0), true), 19.5);
    assert_eq!(bounded_level_ppm(30.0, Some(20.0), false), 30.0);
    assert_eq!(bounded_level_ppm(30.0, None, true), 30.0);
}

#[test]
fn settling_needs_the_lock_and_an_error_within_the_calm_zone_and_stays() {
    assert!(settled(false, true, 0.5, 1.0));
    assert!(settled(false, true, 1.0, 1.0));
    assert!(settled(false, true, -1.0, 1.0));
    assert!(!settled(false, true, 1.01, 1.0));
    assert!(!settled(false, true, -1.01, 1.0));
    assert!(!settled(false, false, 0.5, 1.0));
    assert!(settled(true, false, 9.0, 1.0));
}

#[test]
fn a_bound_below_zero_is_none_at_all() {
    let c = Controller::new(0.010, 0.002, 0.050, Basis::Arrival).with_level_bound_ppm(-5.0);
    assert_eq!(c.level_bound_ppm, Some(0.0));
    let c = Controller::new(0.010, 0.002, 0.050, Basis::Arrival);
    assert_eq!((c.level_bound_ppm, c.settled), (None, false));
}

/// A controller held 80 ppm off for 5 minutes (locked, settled), then
/// left 15 ms over its target (a re-prime out of a writer's phase): the
/// correction stays within 20 ppm of the fitted rate while it drains,
/// where the unbounded laws brake at up to 300 ppm off.
#[test]
fn after_settling_an_offset_drains_within_the_bound() {
    let worst = |bound: Option<f64>| {
        let mut s = Sim::new(80.0, 0.010);
        if let Some(b) = bound {
            s.c = s.c.with_level_bound_ppm(b);
        }
        assert_eq!(s.run(300.0), 0);
        assert!(s.c.locked() && (s.c.rate_ppm() - 80.0).abs() < 2.0);
        s.depth_frames += 0.015 * s.rate;
        let mut worst: f64 = 0.0;
        for _ in 0..600 {
            assert_eq!(s.run(0.1), 0, "an offset inside the excess is no fault");
            worst = worst.max((s.c.ppm() - s.c.rate_ppm()).abs());
        }
        (worst, s.depth_ms())
    };
    let (bounded, depth) = worst(Some(20.0));
    assert!(bounded <= 20.0 + 0.5, "{bounded} ppm off the rate");
    assert!(depth < 25.0 - 0.5, "it drains: {depth} ms");
    let (unbounded, _) = worst(None);
    assert!(unbounded > 100.0, "{unbounded} ppm off the rate");
}

/// Before it settles, the bound holds nothing: the braking follows a
/// start-up offset (the drift before the fit knows it) as without one.
#[test]
fn before_settling_the_laws_are_unbounded() {
    let mut s = Sim::new(0.0, 0.035);
    s.c = s.c.with_level_bound_ppm(20.0);
    s.run(30.0);
    assert!(!s.c.settled);
    let mut unbounded = Sim::new(0.0, 0.035);
    unbounded.run(30.0);
    assert_eq!(s.c.ppm(), unbounded.c.ppm());
    assert!(s.c.ppm().abs() > 20.0, "{}", s.c.ppm());
}
