// SPDX-License-Identifier: MIT OR Apache-2.0

//! The fixed-ratio table's long runs (dstack #462), in their own binary: a
//! converter whose ratio never moves runs the sinc core's filter from
//! kernels built once (`dstack_resample::poly`), and these runs check it
//! against the core on every rate pair and group width, and its quality at
//! 96 <-> 44.1 kHz. Their own binary because they are heavy under a
//! coverage build, and a unit test beside them (the timing test, now
//! `tests/resample_timing.rs`)
//! starved; the unit tests keep short versions for the mutation gate
//! (`src/tests/polyphase.rs`).
//!
//! Measured (1 kHz at -1 dBFS, a block of 256, the settled part): THD+N
//! 228.1 dB at 96 -> 44.1 and 221.9 dB at 44.1 -> 96 (the core: 215.5-218.8
//! and 194.2-197.5, its float position sum), the same levels as the core's
//! at 20 Hz to 20 kHz (96 -> 44.1: 20 kHz 0.553 dB down, the 256-tap
//! filter's edge; 44.1 -> 96 within 0.0017 dB), a 30 kHz tone folded to
//! 14.1 kHz at -214.7 dBFS and a 20 kHz tone's 24.1 kHz image at -177.0
//! dBFS.

use dstack_resample::{Converter, Fixed, TAPS};

#[allow(dead_code)]
#[path = "../src/tests/signal.rs"]
mod signal;
use signal::{MINUS_1_DBFS, level_dbfs, run_fixed_input, thd_n_db, tone};

/// What [`stream`] heard: each channel's output, and each call's frames
/// taken and given.
type Heard = (Vec<Vec<f64>>, Vec<(usize, usize)>);

/// Runs `c` over `input` (per channel) call by call until a call would
/// pass its end.
fn stream(c: &mut Converter, input: &[Vec<f64>]) -> Heard {
    let channels = input.len();
    let room = c.output_frames_max();
    let mut out = vec![0.0; channels * room];
    let mut heard = vec![Vec::new(); channels];
    let mut calls = Vec::new();
    let mut at = 0;
    loop {
        let n = c.input_frames_next();
        if at + n > input[0].len() {
            return (heard, calls);
        }
        let flat: Vec<f64> = input
            .iter()
            .flat_map(|ch| ch[at..at + n].iter().copied())
            .collect();
        let (took, gave) = c.process(&flat, n, &mut out, room).unwrap();
        assert_eq!(took, n);
        calls.push((took, gave));
        at += took;
        for (ch, h) in heard.iter_mut().enumerate() {
            h.extend_from_slice(&out[ch * room..ch * room + gave]);
        }
    }
}

/// A different mix of tones on each channel, near full scale.
fn mixes(channels: usize, rate: u32, frames: usize) -> Vec<Vec<f64>> {
    (0..channels)
        .map(|ch| {
            let a = tone(997.0 + 211.0 * ch as f64, f64::from(rate), frames, 0.45);
            let b = tone(13_001.0 - 97.0 * ch as f64, f64::from(rate), frames, 0.4);
            a.iter().zip(&b).map(|(x, y)| x + y).collect()
        })
        .collect()
}

/// The rate pairs the table is checked on: 44.1 kHz on 96 kHz each way
/// (dasio), 44.1 <-> 48 kHz, an odd 2:1 (a block the half-band refuses)
/// and 22.05 kHz up to 96 kHz (147 output frames for 320 input).
const PAIRS: [(u32, u32); 7] = [
    (96_000, 44_100),
    (44_100, 96_000),
    (48_000, 44_100),
    (44_100, 48_000),
    (96_000, 48_000),
    (22_050, 96_000),
    (96_000, 22_050),
];

/// The table's output is the core's, sample for sample (within 1e-9 of a
/// near full-scale signal, under -180 dBFS: the core's own float error
/// reached 3.6e-10 at 22.05 -> 96 kHz), on a fixed input and a fixed
/// output, 1, 2 and 31 channels (every group width), a block of 256 and an
/// odd one, with the core's delay. Each call takes and gives the core's
/// frames, but where the core's float rounding moves one frame on a whole
/// boundary to the next call (#343): the fixed side alike every call, the
/// other side's running sum within one frame.
#[test]
fn the_table_gives_the_sinc_core_s_output() {
    for (from, to) in PAIRS {
        for fixed in [Fixed::Input, Fixed::Output] {
            for (channels, block) in [(1, 256), (2, 37), (31, 256)] {
                let what = format!("{from}->{to} {fixed:?} {channels}x{block}");
                let mut table = Converter::new(from, to, channels, block, fixed).unwrap();
                let mut core = Converter::drifting(from, to, channels, block, fixed).unwrap();
                assert!(table.is_polyphase() && !core.is_polyphase(), "{what}");
                assert_eq!(table.delay_frames(), core.delay_frames(), "{what}");
                let frames = 6 * block.max(256) * from as usize / to.min(from) as usize;
                let input = mixes(channels, from, frames);
                let ((a, ca), (b, cb)) = (stream(&mut table, &input), stream(&mut core, &input));
                assert!(ca.len().abs_diff(cb.len()) <= 1 && ca.len() > 4, "{what}");
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
                let mut worst = 0.0f64;
                for (x, y) in a.iter().zip(&b) {
                    for (p, q) in x.iter().zip(y) {
                        worst = worst.max((p - q).abs());
                    }
                }
                assert!(worst < 1e-9, "{what}: {worst:e}");
                assert!(a[0].iter().any(|v| v.abs() > 0.3), "{what}: no signal");
            }
        }
    }
}

/// A table at 96 <-> 44.1 kHz keeps the core's bars: a 1 kHz tone above
/// 120 dB THD+N at its level, the audio band at the core's levels (96 ->
/// 44.1: flat to 15 kHz, 20 kHz within 0.6 dB; up: flat to 20 kHz), what
/// folds back or images below -120 dBFS.
#[test]
fn the_table_keeps_the_core_s_quality_at_44_1_khz() {
    for (from, to) in [(96_000u32, 44_100u32), (44_100, 96_000)] {
        let (f, t) = (f64::from(from), f64::from(to));
        let through = |make: &dyn Fn() -> Converter, hz: f64| {
            let input = tone(hz, f, 256 * 300, MINUS_1_DBFS);
            let out = run_fixed_input(&mut make(), &input, 256);
            out[TAPS * 2..].to_vec()
        };
        let table = || Converter::new(from, to, 1, 256, Fixed::Input).unwrap();
        let core = || Converter::drifting(from, to, 1, 256, Fixed::Input).unwrap();
        let out = through(&table, 1_000.0);
        let db = thd_n_db(&out, 1_000.0, t);
        assert!(db >= 120.0, "{from} -> {to}: THD+N {db:.1} dB");
        for hz in [20.0, 1_000.0, 10_000.0, 15_000.0, 20_000.0] {
            let lvl = level_dbfs(&through(&table, hz), hz, t);
            let core_lvl = level_dbfs(&through(&core, hz), hz, t);
            assert!(
                (lvl - core_lvl).abs() < 1e-6,
                "{from} -> {to}: {hz} Hz at {lvl:.7} against the core's {core_lvl:.7}"
            );
            let bar = if from > to && hz > 15_000.0 {
                0.6
            } else {
                0.01
            };
            assert!(
                (lvl + 1.0).abs() < bar,
                "{from} -> {to}: {hz} Hz at {lvl:.4}"
            );
        }
        // Down: 30 kHz would fold to 14.1 kHz; up: 20 kHz would image at
        // 24.1 kHz.
        let (hz, at) = if from > to {
            (30_000.0, t - 30_000.0)
        } else {
            (20_000.0, f - 20_000.0)
        };
        let lvl = level_dbfs(&through(&table, hz), at, t);
        assert!(lvl < -120.0, "{from} -> {to}: {at} Hz at {lvl:.1} dBFS");
    }
}
