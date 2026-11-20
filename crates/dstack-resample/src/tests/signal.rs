// SPDX-License-Identifier: MIT OR Apache-2.0

//! The signal helpers the converter's tests measure with (a tone, a
//! fixed-input run, THD+N, a level): shared by the unit tests (`mod signal`
//! in `tests.rs`) and the long runs in their own binary (`tests/table.rs`,
//! `#[path]`), so a figure measured in either comes from the same code.

use super::Converter;
use std::f64::consts::PI;

/// −1 dBFS.
pub const MINUS_1_DBFS: f64 = 0.891_250_938_133_745_6;

/// `n` samples of a `hz` sine at `rate`, peak `amp`.
pub fn tone(hz: f64, rate: f64, n: usize, amp: f64) -> Vec<f64> {
    (0..n)
        .map(|i| amp * (2.0 * PI * hz * i as f64 / rate).sin())
        .collect()
}

/// Runs one channel of `input` through `c` (fixed input of `block`
/// frames); returns all the output.
pub fn run_fixed_input(c: &mut Converter, input: &[f64], block: usize) -> Vec<f64> {
    let room = c.output_frames_max();
    let mut out = vec![0.0; room];
    let mut all = Vec::new();
    for chunk in input.chunks_exact(block) {
        let (taken, made) = c.process(chunk, block, &mut out, room).unwrap();
        assert_eq!(taken, block);
        all.extend_from_slice(&out[..made]);
    }
    all
}

/// The residual of `x` after the least-squares fit of a sine at `hz`
/// (`rate`) with an offset: its THD+N in dB (signal over residual).
pub fn thd_n_db(x: &[f64], hz: f64, rate: f64) -> f64 {
    let w = 2.0 * PI * hz / rate;
    // Normal equations for a*sin + b*cos + d.
    let (mut ss, mut cc, mut sc, mut s1, mut c1, mut n) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    let (mut xs, mut xc, mut x1) = (0.0, 0.0, 0.0);
    for (i, &v) in x.iter().enumerate() {
        let (s, c) = (w * i as f64).sin_cos();
        ss += s * s;
        cc += c * c;
        sc += s * c;
        s1 += s;
        c1 += c;
        n += 1.0;
        xs += v * s;
        xc += v * c;
        x1 += v;
    }
    let m = [[ss, sc, s1], [sc, cc, c1], [s1, c1, n]];
    let r = [xs, xc, x1];
    let det = |m: [[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let d = det(m);
    let solve = |k: usize| {
        let mut mk = m;
        for (row, &rv) in mk.iter_mut().zip(&r) {
            row[k] = rv;
        }
        det(mk) / d
    };
    let (a, b, off) = (solve(0), solve(1), solve(2));
    let (mut sig, mut res) = (0.0, 0.0);
    for (i, &v) in x.iter().enumerate() {
        let (s, c) = (w * i as f64).sin_cos();
        let fit = a * s + b * c + off;
        sig += fit * fit;
        res += (v - fit) * (v - fit);
    }
    10.0 * (sig / res).log10()
}

/// The level of `hz` in `x` (`rate`), dBFS, by a Blackman-Harris windowed
/// correlation.
pub fn level_dbfs(x: &[f64], hz: f64, rate: f64) -> f64 {
    let n = x.len() as f64;
    let (mut re, mut im, mut wsum) = (0.0, 0.0, 0.0);
    for (i, &v) in x.iter().enumerate() {
        let t = 2.0 * PI * i as f64 / (n - 1.0);
        let win =
            0.35875 - 0.48829 * t.cos() + 0.14128 * (2.0 * t).cos() - 0.01168 * (3.0 * t).cos();
        let ph = 2.0 * PI * hz * i as f64 / rate;
        re += v * win * ph.cos();
        im += v * win * ph.sin();
        wsum += win;
    }
    20.0 * (2.0 * (re * re + im * im).sqrt() / wsum).log10()
}
