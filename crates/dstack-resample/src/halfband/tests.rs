use super::*;
use std::f64::consts::PI;

/// The filter's gain at `hz` for a rate of `rate`, dB (its DTFT).
fn gain_db(h: &[f64], hz: f64, rate: f64) -> f64 {
    let w = 2.0 * PI * hz / rate;
    let (mut re, mut im) = (0.0, 0.0);
    for (n, &t) in h.iter().enumerate() {
        re += t * (w * n as f64).cos();
        im -= t * (w * n as f64).sin();
    }
    10.0 * (re * re + im * im).log10()
}

/// The response sampled every 10 Hz from `lo` to `hi` Hz at 96 kHz.
fn response(lo: u32, hi: u32) -> Vec<f64> {
    let h = taps();
    (lo / 10..=hi / 10)
        .map(|f| gain_db(&h, f64::from(f * 10), 96_000.0))
        .collect()
}

#[test]
fn the_passband_is_flat_to_20_khz_and_the_stopband_below_145_db() {
    let pass = response(0, 20_000);
    let (lo, hi) = pass
        .iter()
        .fold((f64::MAX, f64::MIN), |(a, b), &g| (a.min(g), b.max(g)));
    assert!(hi - lo <= RIPPLE_DB, "ripple {:.7} dB", hi - lo);
    assert!(hi.abs() <= RIPPLE_DB && lo.abs() <= RIPPLE_DB, "{lo} {hi}");
    let stop = response(28_000, 48_000);
    let worst = stop.iter().copied().fold(f64::MIN, f64::max);
    assert!(worst <= -STOP_DB, "stopband {worst:.1} dB");
}

#[test]
fn the_filter_is_a_half_band_summing_to_one() {
    let h = taps();
    assert_eq!(h[DELAY_HIGH], 0.5);
    assert!((h.iter().sum::<f64>() - 1.0).abs() < 1e-15);
    for d in (2..=DELAY_HIGH).step_by(2) {
        assert_eq!((h[DELAY_HIGH - d], h[DELAY_HIGH + d]), (0.0, 0.0), "{d}");
    }
    for d in (1..=DELAY_HIGH).step_by(2) {
        assert_eq!(h[DELAY_HIGH - d], h[DELAY_HIGH + d], "symmetric at {d}");
        assert!(h[DELAY_HIGH + d] != 0.0, "{d}");
    }
    assert_eq!(side_taps()[0], h[DELAY_HIGH + 1]);
    assert_eq!(side_taps()[SIDE - 1], h[TAPS - 1]);
}

/// An impulse through each step comes out [`HalfBand::delay_frames`] later,
/// and the filter's taps are what the step applies.
#[test]
fn an_impulse_comes_out_at_the_delay_with_the_filter_s_taps() {
    let h = taps();
    // Down: each output k is the filter at input 2k + 1; an impulse at
    // input 10 lands on output 5 + the delay with the centre tap.
    let mut d = HalfBand::new(Step::Down, 1, 64);
    let mut input = vec![0.0; 64];
    input[10] = 1.0;
    let mut out = vec![0.0; 32];
    let mut all = Vec::new();
    for _ in 0..4 {
        d.process(&input, 64, &mut out, 32);
        all.extend_from_slice(&out);
        input.fill(0.0);
    }
    assert_eq!(d.delay_frames(), 30);
    assert_eq!(all[5 + 30], 0.5);
    for (k, &y) in all.iter().enumerate() {
        let want = (2 * k + 1)
            .checked_sub(10)
            .filter(|&j| j < TAPS)
            .map_or(0.0, |j| h[j]);
        assert!((y - want).abs() < 1e-15, "{k}: {y} {want}");
    }
    // Up: an impulse at input 5 gives the filter twice over, from output
    // 10, its peak at 10 + the delay.
    let mut u = HalfBand::new(Step::Up, 1, 32);
    let mut input = vec![0.0; 32];
    input[5] = 1.0;
    let mut out = vec![0.0; 64];
    let mut all = Vec::new();
    for _ in 0..4 {
        u.process(&input, 32, &mut out, 64);
        all.extend_from_slice(&out);
        input.fill(0.0);
    }
    assert_eq!(u.delay_frames(), 61);
    assert_eq!(all[10 + 61], 1.0);
    for (n, &y) in all.iter().enumerate() {
        let want = n
            .checked_sub(10)
            .filter(|&j| j < TAPS)
            .map_or(0.0, |j| 2.0 * h[j]);
        assert!((y - want).abs() < 1e-15, "{n}: {y} {want}");
    }
}

#[test]
fn a_reset_forgets_the_history() {
    for (step, frames_in, frames_out) in [(Step::Up, 32, 64), (Step::Down, 64, 32)] {
        let mut s = HalfBand::new(step, 2, frames_in);
        let loud = vec![1.0; 2 * frames_in];
        let mut out = vec![0.0; 2 * frames_out];
        s.process(&loud, frames_in, &mut out, frames_out);
        s.reset();
        s.process(&vec![0.0; 2 * frames_in], frames_in, &mut out, frames_out);
        assert!(out.iter().all(|&v| v == 0.0), "{step:?}");
    }
}

#[test]
fn the_output_frames_follow_the_step() {
    let d = HalfBand::new(Step::Down, 2, 128);
    let u = HalfBand::new(Step::Up, 2, 64);
    assert_eq!((d.output_frames(128), u.output_frames(64)), (64, 128));
}
