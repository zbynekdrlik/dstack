use super::*;

const S: i64 = 1_000_000_000;

/// A wall that runs `ppb` fast of QPC from `start`, plus `stepped`.
fn wall(q: i64, start: i64, ppb: i64, stepped: i64) -> i64 {
    start + q + (i128::from(q) * i128::from(ppb) / 1_000_000_000) as i64 + stepped
}

/// Polls once a second from `from` to `to` (s), the wall at `ppb` with
/// `stepped` added; returns the updates.
fn poll(st: &mut Steer, from: i64, to: i64, ppb: i64, stepped: i64) -> Vec<Update> {
    (from..to)
        .map(|t| st.update(t * S, wall(t * S, 7 * S, ppb, stepped)))
        .collect()
}

#[test]
fn the_clock_follows_a_steered_wall_within_a_microsecond() {
    let mut st = Steer::new();
    let ups = poll(&mut st, 0, 10, 50_000, 0);
    assert_eq!(ups[0], Update::First);
    let m = st.model().unwrap();
    for t in [10 * S, 10 * S + S / 2] {
        let err = m.at(t) - wall(t, 7 * S, 50_000, 0);
        assert!(err.abs() < 1_000, "{err} ns at {t}");
    }
    assert!(
        ups[3..].iter().all(|u| matches!(u, Update::Slew { .. })),
        "{ups:?}"
    );
    assert_eq!(st.offset(), 0);
}

#[test]
fn a_date_step_does_not_move_the_clock() {
    for step in [1_600_000_000, -1_600_000_000, 200_000_000, 2_000_000] {
        let mut st = Steer::new();
        poll(&mut st, 0, 10, 20_000, 0);
        let before = st.model().unwrap();
        // The wall steps between the polls at 9 s and 10 s.
        let ups = poll(&mut st, 10, 11, 20_000, step);
        let Update::Step { jump_ns } = ups[0] else {
            panic!("{step}: {ups:?}");
        };
        assert!((jump_ns - step).abs() < 1_000, "{step}: {jump_ns}");
        let after = st.model().unwrap();
        // One clock across the step: the old model and the new agree at
        // the poll, and the clock never jumped.
        assert!(
            (after.at(10 * S) - before.at(10 * S)).abs() < 1_000,
            "{step}"
        );
        // Later polls are slews again, tracking the stepped wall less the
        // offset.
        let ups = poll(&mut st, 11, 20, 20_000, step);
        assert!(
            ups.iter().all(|u| matches!(u, Update::Slew { .. })),
            "{step}: {ups:?}"
        );
        let m = st.model().unwrap();
        let err = m.at(20 * S) - (wall(20 * S, 7 * S, 20_000, step) - st.offset());
        assert!(err.abs() < 1_000, "{step}: {err}");
        assert!(
            (st.offset() - step).abs() < 1_000,
            "{step}: {}",
            st.offset()
        );
    }
}

#[test]
fn a_small_jump_is_slewed_out_not_kept() {
    let mut st = Steer::new();
    poll(&mut st, 0, 5, 0, 0);
    let ups = poll(&mut st, 5, 6, 0, 300_000);
    let Update::Slew { error_ns, ppb } = ups[0] else {
        panic!("{ups:?}");
    };
    assert!((error_ns - 300_000).abs() < 1_000, "{error_ns}");
    // The jump reads as rate too (one poll cannot tell them apart), so the
    // first slew overshoots and the next takes the overshoot back.
    assert!((300_000..=MAX_PPB).contains(&ppb), "{ppb}");
    poll(&mut st, 6, 8, 0, 300_000);
    assert_eq!(st.offset(), 0, "no step");
    let err = st.model().unwrap().at(8 * S) - wall(8 * S, 7 * S, 0, 300_000);
    assert!(err.abs() < 1_000, "{err}");
}

#[test]
fn a_slew_is_never_steeper_than_the_bound() {
    let mut st = Steer::new();
    poll(&mut st, 0, 3, 0, 0);
    // Just under the step threshold, after a second.
    let ups = poll(&mut st, 3, 4, 0, STEP_MIN_NS + DRIFT_PPB - 1);
    let Update::Slew { ppb, .. } = ups[0] else {
        panic!("{ups:?}");
    };
    assert_eq!(ppb, MAX_PPB);
    let ups = poll(&mut st, 4, 5, 0, -STEP_MIN_NS);
    assert!(matches!(ups[0], Update::Step { .. }), "{ups:?}");
}

#[test]
fn a_long_gap_allows_more_drift_before_a_step() {
    let mut st = Steer::new();
    poll(&mut st, 0, 3, 0, 0);
    // 30 s without a poll, the wall 50 ppm fast meanwhile: 1.5 ms of drift,
    // under 1 ms + 100 ppm x 30 s.
    let q = 32 * S;
    let w = 7 * S + q + 1_500_000;
    assert!(matches!(st.update(q, w), Update::Slew { .. }));
    // The same 1.5 ms after one second is a step.
    let mut st = Steer::new();
    poll(&mut st, 0, 3, 0, 0);
    assert!(matches!(
        st.update(3 * S, 10 * S + 1_500_000),
        Update::Step { .. }
    ));
}

#[test]
fn a_model_survives_its_words_and_scales_both_ways() {
    let m = Model {
        base_q: -5,
        base_m: 1_800_000_000_000_000_000,
        ppb: -42_000,
    };
    assert_eq!(Model::from_words(m.words()), m);
    let m = Model {
        base_q: 10 * S,
        base_m: 100 * S,
        ppb: 1_000,
    };
    assert_eq!(m.at(11 * S), 101 * S + 1_000);
    assert_eq!(m.at(9 * S), 99 * S - 1_000);
    assert_eq!(m.at(10 * S), 100 * S);
}
