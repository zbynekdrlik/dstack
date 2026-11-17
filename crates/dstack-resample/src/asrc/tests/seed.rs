// SPDX-License-Identifier: MIT OR Apache-2.0

//! The rate seed (dstack #382): the last run's locked rate, used until the
//! fit locks.

use super::*;

/// The largest |depth - 10 ms| over the first `seconds`, ms, read every
/// 100 ms of a [`Sim`] whose controller was seeded with `seed` (none: 0).
fn first_drift_ms(drift: f64, seed: Option<f64>, seconds: f64) -> (Sim, f64) {
    let mut s = Sim::new(drift, 0.010);
    if let Some(ppm) = seed {
        s.c.seed_ppm(ppm);
    }
    let mut worst: f64 = 0.0;
    let mut t = 0.0;
    while t < seconds {
        assert_eq!(s.run(0.1), 0, "no re-centre");
        worst = worst.max((s.depth_ms() - 10.0).abs());
        t += 0.1;
    }
    (s, worst)
}

/// dasio#120: a clock 7.5 ppm off drifts the depth about 0.45 ms before the
/// rate fit locks at 60 s. Seeded with that rate, the depth holds from the
/// first window, and the lock lands on the same rate.
#[test]
fn a_seeded_start_holds_the_depth_through_the_first_minute() {
    for drift in [7.5, -7.5, 50.0] {
        let (_, unseeded) = first_drift_ms(drift, None, 59.0);
        let (mut s, seeded) = first_drift_ms(drift, Some(drift), 59.0);
        println!(
            "drift {drift} ppm: first minute {unseeded:.3} ms unseeded, {seeded:.4} ms seeded"
        );
        assert!(
            unseeded > 0.3,
            "{drift}: the unseeded start drifts: {unseeded}"
        );
        assert!(seeded < 0.02, "{drift}: the seeded start holds: {seeded}");
        assert_eq!(s.run(60.0), 0);
        assert!(s.c.locked());
        assert!(
            (s.c.rate_ppm() - drift).abs() < 0.5,
            "{drift}: {}",
            s.c.rate_ppm()
        );
        assert!(
            (s.depth_ms() - 10.0).abs() < 0.05,
            "{drift}: {}",
            s.depth_ms()
        );
    }
}

/// The correction starts at a seed given before the first observation; a
/// seed given later moves only the rate term (no step: the slew applies).
#[test]
fn a_seed_starts_the_correction_only_before_the_first_observation() {
    let mut c = Controller::new(0.010, 0.0, 0.050, Basis::Arrival);
    assert_eq!(c.seed(), 0.0);
    c.seed_ppm(7.5);
    assert_eq!((c.ppm(), c.seed()), (7.5, 7.5));
    // An observation opens a window: a seed now keeps the correction.
    let mut c = Controller::new(0.010, 0.0, 0.050, Basis::Arrival);
    assert_eq!(c.observe(0.0, 0.010, 0.0), Step::Hold(0.0));
    c.seed_ppm(7.5);
    assert_eq!((c.ppm(), c.seed()), (0.0, 7.5));
    // A whole window closed (none open): still no step.
    let mut c = Controller::new(0.010, 0.0, 0.050, Basis::Arrival);
    window(&mut c, 0.0, 0.010);
    c.seed_ppm(7.5);
    assert_eq!(c.ppm(), 0.0);
    // The next window (1.25 s after the last) slews toward it, 5 ppm a
    // second at most.
    window(&mut c, 1.25, 0.010);
    assert!((c.ppm() - 6.25).abs() < 1e-9, "{}", c.ppm());
}

/// A seed past the controller's bound is held at ±[`MAX_PPM`].
#[test]
fn a_seed_is_held_within_the_bound() {
    let mut c = Controller::new(0.010, 0.0, 0.050, Basis::Arrival);
    c.seed_ppm(1000.0);
    assert_eq!((c.ppm(), c.seed()), (MAX_PPM, MAX_PPM));
    c.seed_ppm(-1000.0);
    assert_eq!(c.seed(), -MAX_PPM);
    // No number: ignored, the controller as it was.
    let mut c = Controller::new(0.010, 0.0, 0.050, Basis::Arrival);
    c.seed_ppm(7.5);
    for nan in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        c.seed_ppm(nan);
        assert_eq!((c.ppm(), c.seed()), (7.5, 7.5), "{nan}");
    }
}

/// The fit wins once locked: a wrong seed (40 ppm on a clock with no
/// drift) costs depth at the start, then the lock takes the real rate and
/// the depth is back within the calm zone.
#[test]
fn the_locked_fit_replaces_a_wrong_seed() {
    let mut s = Sim::new(0.0, 0.010);
    s.c.seed_ppm(40.0);
    assert_eq!(s.run(600.0), 0);
    assert!(s.c.locked());
    assert!(s.c.rate_ppm().abs() < 0.5, "{}", s.c.rate_ppm());
    assert!(s.c.ppm().abs() < 1.0, "{}", s.c.ppm());
    assert!((s.depth_ms() - 10.0).abs() < 0.5, "{}", s.depth_ms());
}
