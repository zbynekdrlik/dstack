// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #382 (the owner's ROZHODNUTÉ of 17.11.2026): the converter length
//! as a bridge setting, `Spec::converter_taps`.

use super::*;

fn taps(t: Option<u32>) -> Spec {
    Spec {
        converter_taps: t,
        ..spec(1, 1)
    }
}

/// Each direction's converter delay, ns, of a bridge of `t` before the
/// driver's first open (the idle from-ASIO player) and after an open at
/// 96 kHz with a 128-frame buffer: (from before, to after, from after).
fn delays(t: Option<u32>) -> (u64, u64, u64) {
    let mut b = bridge(taps(t));
    let before = b.from_asio_stats().converter_delay_ns();
    let _dante = b.dante_side();
    let _asio = b.asio_side(&opened(96_000.0, 128)).unwrap();
    (
        before,
        b.to_asio_stats().converter_delay_ns(),
        b.from_asio_stats().converter_delay_ns(),
    )
}

/// Unset, both directions keep the 256-tap converter (1.333 ms at 96 <-> 96
/// kHz), as before the setting; 256 set is the same.
#[test]
fn unset_both_directions_keep_the_256_tap_converter() {
    let at = (1_333_333, 1_333_333, 1_333_333);
    assert_eq!(delays(None), at);
    assert_eq!(delays(Some(256)), at);
}

/// The set length reaches both directions' converters, the idle player's
/// too: 48 taps 0.250 ms a direction, 32 taps 0.167 ms.
#[test]
fn the_set_length_reaches_both_directions() {
    assert_eq!(delays(Some(48)), (250_000, 250_000, 250_000));
    assert_eq!(delays(Some(32)), (166_667, 166_667, 166_667));
}

/// 32 to 256 in steps of 8: the edges and a length off the step.
#[test]
fn a_length_outside_32_to_256_or_off_the_step_is_refused() {
    for t in [32, 40, 248, 256] {
        assert!(Bridge::new(taps(Some(t))).is_ok(), "{t}");
        assert_eq!(converter_taps_problem(Some(t)), None, "{t}");
    }
    assert_eq!(converter_taps_problem(None), None);
    for t in [0, 24, 31, 33, 255, 257, 264] {
        let refused = Bridge::new(taps(Some(t))).err();
        assert_eq!(
            refused.as_deref(),
            Some(format!("converter_taps: {t}; 32 to 256 in steps of 8").as_str()),
            "{t}"
        );
    }
}

/// The whole bridge opens and runs with 48 taps (dsg's 96 <-> 96 kHz, its
/// driver 7.5 ppm fast, a 128-frame buffer at the default target): no
/// underrun or re-centre after the priming, each tone at its frequency and
/// level, each converter at 0.250 ms.
#[test]
fn the_whole_bridge_runs_with_48_taps() {
    let mut s = Sim::of(taps(Some(48)), 7.5, 128);
    s.run(40.0, 30.0);
    let to = s.bridge.to_asio_stats();
    let from = s.bridge.from_asio_stats();
    for (what, st) in [("to", &to), ("from", &from)] {
        assert_eq!(st.underruns(), 0, "{what}");
        assert_eq!(st.recentres(), 0, "{what}");
        assert!(!st.priming(), "{what}: playing");
        assert_eq!(st.converter_delay_ns(), 250_000, "{what}");
    }
    let card = f64::from(RATE) * (1.0 + 7.5e-6);
    let f_to = frequency(&s.to_asio_out, card);
    let f_from = frequency(&s.from_asio_out, f64::from(RATE));
    assert!((f_to - 1000.0).abs() < 0.05, "to ASIO: {f_to} Hz");
    assert!((f_from - 2000.0).abs() < 0.1, "from ASIO: {f_from} Hz");
    for (what, out) in [("to", &s.to_asio_out), ("from", &s.from_asio_out)] {
        let rms = (out.iter().map(|x| x * x).sum::<f64>() / out.len() as f64).sqrt();
        let db = 20.0 * (rms / (0.5 / 2f64.sqrt())).log10();
        assert!(db.abs() < 0.05, "{what}: {db:+.4} dB");
    }
}
