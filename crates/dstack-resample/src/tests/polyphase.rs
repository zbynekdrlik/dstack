// SPDX-License-Identifier: MIT OR Apache-2.0

//! The fixed-ratio table (dstack #462): a converter whose ratio never
//! moves runs the sinc core's filter from kernels built once
//! ([`crate::poly`]). Its output is the core's (a drifting converter at
//! its nominal ratio is the core itself), with the same delay and frame
//! counts. Short versions for the mutation gate and the coverage run: the
//! long runs over every rate pair and group width, and the quality figures,
//! are `tests/table.rs`'s (its own binary).

use super::*;

/// Runs `c` over `channels` channels of a different tone mix each (near
/// full scale) for `calls` calls; returns each channel's output and each
/// call's frames taken and given.
fn run(c: &mut Converter, channels: usize, calls: usize) -> (Vec<Vec<f64>>, Vec<(usize, usize)>) {
    let rate = f64::from(c.from);
    let room = c.output_frames_max();
    let mut out = vec![0.0; channels * room];
    let mut heard = vec![Vec::new(); channels];
    let mut counts = Vec::new();
    let mut at = 0;
    for _ in 0..calls {
        let n = c.input_frames_next();
        let flat: Vec<f64> = (0..channels)
            .flat_map(|ch| {
                let (a, b) = (997.0 + 211.0 * ch as f64, 13_001.0 - 97.0 * ch as f64);
                (at..at + n).map(move |i| {
                    let t = 2.0 * std::f64::consts::PI * i as f64 / rate;
                    0.45 * (a * t).sin() + 0.4 * (b * t).sin()
                })
            })
            .collect();
        let (took, gave) = c.process(&flat, n, &mut out, room).unwrap();
        at += took;
        counts.push((took, gave));
        for (ch, h) in heard.iter_mut().enumerate() {
            h.extend_from_slice(&out[ch * room..ch * room + gave]);
        }
    }
    (heard, counts)
}

/// The table's output is the core's, sample for sample (within 1e-9, the
/// core's own float error), at 96 <-> 44.1 kHz on both fixed sides with 31
/// channels (one group of each width), past the filter's start; the fixed
/// side's frames alike every call, the other side's running sum within
/// one frame.
#[test]
fn the_table_gives_the_sinc_core_s_output_in_short() {
    for (from, to) in [(96_000u32, 44_100u32), (44_100, 96_000)] {
        for fixed in [Fixed::Input, Fixed::Output] {
            let what = format!("{from}->{to} {fixed:?}");
            let mut table = Converter::new(from, to, 31, 128, fixed).unwrap();
            let mut core = Converter::drifting(from, to, 31, 128, fixed).unwrap();
            assert!(table.is_polyphase() && !core.is_polyphase(), "{what}");
            let ((a, ca), (b, cb)) = (run(&mut table, 31, 8), run(&mut core, 31, 8));
            let (mut sum_a, mut sum_b) = (0i64, 0i64);
            for (k, (&(ta, ga), &(tb, gb))) in ca.iter().zip(&cb).enumerate() {
                let (fa, fb, oa, ob) = match fixed {
                    Fixed::Input => (ta, tb, ga, gb),
                    Fixed::Output => (ga, gb, ta, tb),
                };
                assert_eq!(fa, fb, "{what}: call {k}");
                sum_a += oa as i64;
                sum_b += ob as i64;
                assert!((sum_a - sum_b).abs() <= 1, "{what}: call {k}");
            }
            for (ch, (x, y)) in a.iter().zip(&b).enumerate() {
                let n = x.len().min(y.len());
                assert!(n > 400, "{what}: {n} frames");
                for (i, (p, q)) in x[..n].iter().zip(&y[..n]).enumerate() {
                    assert!((p - q).abs() < 1e-9, "{what}: channel {ch} frame {i}");
                }
                assert!(
                    x[n / 2..n].iter().any(|v| v.abs() > 0.3),
                    "{what}: no signal"
                );
            }
        }
    }
}

/// A 1 kHz tone through the table at 96 <-> 44.1 kHz stays above 120 dB
/// THD+N at its level (`tests/table.rs` holds the band, the folded band
/// and the image).
#[test]
fn the_table_keeps_a_tone_clean_at_44_1_khz() {
    for (from, to) in [(96_000u32, 44_100u32), (44_100, 96_000)] {
        let mut c = Converter::new(from, to, 1, 256, Fixed::Input).unwrap();
        let input = tone(1_000.0, f64::from(from), 256 * 40, MINUS_1_DBFS);
        let out = run_fixed_input(&mut c, &input, 256);
        let settled = &out[TAPS * 2..];
        let db = thd_n_db(settled, 1_000.0, f64::from(to));
        assert!(db >= 120.0, "{from} -> {to}: THD+N {db:.1} dB");
        let lvl = level_dbfs(settled, 1_000.0, f64::from(to));
        assert!((lvl + 1.0).abs() < 0.001, "{from} -> {to}: {lvl:.5} dBFS");
    }
}

/// The frames each side of a call: the core's sizes, and the core's
/// counts (a fixed input's 147 frames for 320, a fixed output's 320 for
/// 147, the first call's start).
#[test]
fn the_table_counts_frames_as_the_core() {
    let rx = Converter::new(96_000, 44_100, 2, 256, Fixed::Input).unwrap();
    let tx = Converter::new(44_100, 96_000, 2, 256, Fixed::Output).unwrap();
    assert_eq!((rx.delay_frames(), tx.delay_frames()), (58, 278));
    assert_eq!((rx.input_frames_max(), rx.output_frames_max()), (256, 127));
    assert_eq!((tx.input_frames_max(), tx.output_frames_max()), (248, 256));
    assert_eq!((rx.input_frames_next(), tx.input_frames_next()), (256, 119));
    let (mut rx, mut tx) = (rx, tx);
    let (mut gave, mut took) = (0, 0);
    let mut out = vec![0.0; 2 * 256];
    let input = vec![0.0; 2 * 256];
    for _ in 0..320 {
        gave += rx.process(&input, 256, &mut out, 127).unwrap().1;
        let n = tx.input_frames_next();
        took += tx.process(&input, n, &mut out, 256).unwrap().0;
    }
    // 320 blocks of 256: floor((81920 - 2) * 147 / 320) outputs and
    // ceil(81920 * 147 / 320) + 1 inputs.
    assert_eq!((gave, took), (37631, 37633));
}

/// A fixed ratio runs the table up to its phase limit; a drifting
/// converter, a copy, a half-band step and a ratio of more phases run as
/// before; a table's ratio cannot be adjusted.
#[test]
fn a_fixed_ratio_runs_the_table_and_any_other_converter_as_before() {
    let made = |from, to| Converter::new(from, to, 1, 16, Fixed::Input).unwrap();
    assert!(made(96_000, 44_100).is_polyphase());
    assert!(made(3, 1_024).is_polyphase(), "1024 phases");
    assert!(!made(3, 1_025).is_polyphase(), "1025 phases");
    assert!(!made(11_025, 192_000).is_polyphase(), "2560 phases");
    assert!(!made(48_000, 48_000).is_polyphase());
    let low = Converter::low_delay(44_100, 96_000, 1, 128, Fixed::Output).unwrap();
    assert!(low.is_polyphase());
    let half = Converter::low_delay(96_000, 48_000, 1, 128, Fixed::Input).unwrap();
    assert!(half.is_half_band() && !half.is_polyphase());
    let mut drift = Converter::drifting(96_000, 44_100, 1, 128, Fixed::Input).unwrap();
    assert!(!drift.is_polyphase());
    assert!(drift.set_ppm(10.0).is_ok());
    let mut table = made(96_000, 44_100);
    for ppm in [0.0, 10.0] {
        assert!(matches!(table.set_ppm(ppm), Err(ConvertError::Invalid(_))));
    }
}

/// A call short of input, of input samples, of room or of output samples
/// is the core's error, as the sinc core's.
#[test]
fn the_table_refuses_a_short_call() {
    let mut c = Converter::new(96_000, 44_100, 2, 128, Fixed::Input).unwrap();
    let room = c.output_frames_max();
    let (input, mut out) = (vec![0.0; 256], vec![0.0; 2 * room]);
    assert!(c.process(&input, 128, &mut out, room).is_ok());
    for (frames, samples, out_frames, out_samples) in [
        (127, 256, room, 2 * room),
        (128, 255, room, 2 * room),
        (128, 256, 58, 2 * room),
        (128, 256, room, 2 * room - 1),
    ] {
        assert!(matches!(
            c.process(
                &input[..samples],
                frames,
                &mut out[..out_samples],
                out_frames
            ),
            Err(ConvertError::Core(_))
        ));
    }
    // More input than the call takes: the call's frames, from each
    // channel's place.
    let mut d = Converter::new(44_100, 96_000, 1, 32, Fixed::Output).unwrap();
    let n = d.input_frames_next();
    let wide = vec![0.25; n + 5];
    assert_eq!(d.process(&wide, n + 5, &mut out, 32).unwrap(), (n, 32));
}
