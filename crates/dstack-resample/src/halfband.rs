// SPDX-License-Identifier: MIT OR Apache-2.0

//! The short 2:1 and 1:2 step (dstack #1, 4.11.2026): a linear-phase
//! half-band FIR, for a converter whose delay is in a latency budget
//! ([`crate::Converter::low_delay`]). The sinc core delays 2.67 ms at
//! 48 → 96 kHz; this filter delays [`DELAY_HIGH`] samples at the higher
//! rate (0.64 ms at 96 kHz). Flat to 20 kHz at 96 kHz (ripple 0.0000006
//! dB), and the band that folds back or images held 149 dB down from
//! 28 kHz, under a 24-bit LSB (−144.5 dBFS) for a full-scale tone; the
//! sinc holds it over 200 dB down. Measured by the tests: THD+N of a 1 kHz
//! tone at −1 dBFS 252.4 dB at 96 → 48 kHz and 161.4 dB at 48 → 96, a
//! 30 kHz tone folded at 96 → 48 −157.6 dBFS, a 20 kHz tone's 28 kHz image
//! at 48 → 96 −150.2 dBFS ([`STOP_DB`], [`RIPPLE_DB`]).
//!
//! The filter is a Kaiser-windowed sinc cut at a quarter of the higher
//! rate, [`TAPS`] long. Every second tap but the centre is zero, so a
//! decimated output costs one multiply per non-zero tap, and an
//! interpolated pair of outputs the same: the even output phase runs the
//! non-zero taps, the odd one is the centre's sample alone.
//!
//! Data is channel-major, as the crate's. Nothing allocates after
//! [`HalfBand::new`].

/// The filter's length, samples at the higher rate: 4k + 3, so the centre
/// tap sits between zeros and every non-centre tap at an odd distance is
/// non-zero.
pub const TAPS: usize = 123;
/// The filter's delay, samples at the higher rate.
pub const DELAY_HIGH: usize = (TAPS - 1) / 2;
/// The Kaiser window's beta: 0.1102 (A − 8.7) for A = 150 dB.
pub const BETA: f64 = 15.57;
/// The least attenuation from 28 kHz up at 96 kHz (the band that folds
/// back on 2:1 down or images on 1:2 up), dB. The tests hold it.
pub const STOP_DB: f64 = 145.0;
/// The widest passband ripple up to 20 kHz at 96 kHz, dB. The tests hold
/// it.
pub const RIPPLE_DB: f64 = 0.000_01;

/// The non-zero taps beside the centre: those at an odd distance.
const SIDE: usize = (TAPS + 1) / 4;
/// The centre tap's value (a half-band's).
const CENTRE: f64 = 0.5;
const _: () = assert!(TAPS % 4 == 3);

/// The zeroth-order modified Bessel function of the first kind, by its
/// series (enough terms for BETA's range to converge in `f64`).
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0, 1.0);
    for k in 1..60 {
        let f = x / (2.0 * f64::from(k));
        term *= f * f;
        sum += term;
    }
    sum
}

/// The whole filter, [`TAPS`] long, its taps summing to 1 (unit gain at
/// DC).
pub fn taps() -> [f64; TAPS] {
    let centre = DELAY_HIGH as f64;
    let mut h = [0.0; TAPS];
    for (n, tap) in h.iter_mut().enumerate() {
        let t = n as f64 - centre;
        let sinc = if n == DELAY_HIGH {
            CENTRE
        } else {
            (std::f64::consts::PI * t / 2.0).sin() / (std::f64::consts::PI * t)
        };
        let r = t / centre;
        let window = bessel_i0(BETA * (1.0 - r * r).sqrt()) / bessel_i0(BETA);
        *tap = sinc * window;
    }
    // The zero taps are exact zeros of the sine; set them so, and scale the
    // rest so the sum is 1 with the centre kept at a half.
    for (n, tap) in h.iter_mut().enumerate() {
        if n != DELAY_HIGH && (n.abs_diff(DELAY_HIGH)) % 2 == 0 {
            *tap = 0.0;
        }
    }
    let side: f64 = h.iter().sum::<f64>() - CENTRE;
    for (n, tap) in h.iter_mut().enumerate() {
        if n != DELAY_HIGH {
            *tap *= (1.0 - CENTRE) / side;
        }
    }
    h
}

/// The non-zero side taps, nearest the centre first: `side[i]` is the tap
/// at distance 2i + 1 on either side.
fn side_taps() -> [f64; SIDE] {
    let h = taps();
    std::array::from_fn(|i| h[DELAY_HIGH + 2 * i + 1])
}

/// Which way a [`HalfBand`] steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The higher rate to the lower: low-pass, every second sample kept.
    Down,
    /// The lower rate to the higher: zero-stuffed and low-passed, gain 2.
    Up,
}

/// The samples a down step keeps from the call before, at its input rate.
const HISTORY_DOWN: usize = TAPS - 1;
/// The samples an up step keeps from the call before, at its input rate:
/// the even phase reads back `2 SIDE − 1` inputs from the newest.
const HISTORY_UP: usize = 2 * SIDE - 1;

/// A half-band step for `channels` channels of up to `max_in` input frames
/// a call.
#[derive(Debug, Clone)]
pub struct HalfBand {
    step: Step,
    side: [f64; SIDE],
    channels: usize,
    history: usize,
    max_in: usize,
    /// Per channel: the history, then room for `max_in` frames.
    lines: Vec<f64>,
}

impl HalfBand {
    /// A step for `channels` channels, up to `max_in` input frames a call
    /// (an even count for [`Step::Down`]).
    pub fn new(step: Step, channels: usize, max_in: usize) -> HalfBand {
        let history = match step {
            Step::Down => HISTORY_DOWN,
            Step::Up => HISTORY_UP,
        };
        HalfBand {
            step,
            side: side_taps(),
            channels,
            history,
            max_in,
            lines: vec![0.0; channels * (history + max_in)],
        }
    }

    /// Which way this steps.
    pub fn step(&self) -> Step {
        self.step
    }

    /// The output frames for `frames_in` input frames.
    pub fn output_frames(&self, frames_in: usize) -> usize {
        match self.step {
            Step::Down => frames_in / 2,
            Step::Up => frames_in * 2,
        }
    }

    /// The delay, output frames: [`DELAY_HIGH`] at the higher rate, so
    /// half of it (whole, as the decimated phase keeps it) at the lower.
    pub fn delay_frames(&self) -> usize {
        match self.step {
            Step::Down => DELAY_HIGH / 2,
            Step::Up => DELAY_HIGH,
        }
    }

    /// Forgets the history.
    pub fn reset(&mut self) {
        self.lines.fill(0.0);
    }

    /// Steps `frames_in` frames of `input` (channel `c` at
    /// `c * frames_in`) into `output` (channel `c` at `c * out_frames`).
    /// `frames_in` is at most `max_in` (even for down) and `out_frames` at
    /// least [`HalfBand::output_frames`] of it; the caller checks
    /// ([`crate::Converter::process`] does).
    ///
    /// # Panics
    ///
    /// When `frames_in` passes `max_in`, or `input` or `output` is shorter
    /// than `channels` times its frames.
    pub fn process(
        &mut self,
        input: &[f64],
        frames_in: usize,
        output: &mut [f64],
        out_frames: usize,
    ) {
        let span = self.history + self.max_in;
        let h = self.history;
        for c in 0..self.channels {
            let line = &mut self.lines[c * span..(c + 1) * span];
            line[h..h + frames_in].copy_from_slice(&input[c * frames_in..(c + 1) * frames_in]);
            let out = &mut output[c * out_frames..c * out_frames + out_frames];
            match self.step {
                Step::Down => down(&self.side, line, frames_in, out),
                Step::Up => up(&self.side, line, frames_in, out),
            }
            line.copy_within(frames_in..frames_in + h, 0);
        }
    }
}

/// Down: output k is the filter at input `2k + 1` (the line's index
/// `HISTORY_DOWN + 2k + 1`); its centre is the even input
/// `2k + 1 − DELAY_HIGH` (DELAY_HIGH is odd), so
/// the delay is a whole [`DELAY_HIGH`] / 2 output frames.
fn down(side: &[f64; SIDE], line: &[f64], frames_in: usize, out: &mut [f64]) {
    for (k, y) in out.iter_mut().enumerate().take(frames_in / 2) {
        let end = HISTORY_DOWN + 2 * k + 1;
        let mid = end - DELAY_HIGH;
        let mut acc = CENTRE * line[mid];
        for (i, &t) in side.iter().enumerate() {
            let d = 2 * i + 1;
            acc += t * (line[mid - d] + line[mid + d]);
        }
        *y = acc;
    }
}

/// Up: output 2m + 1 is the centre's input sample, `m − SIDE + 1` back
/// from input m (the odd phase); output 2m the non-zero side taps over
/// the inputs around it (the even phase), both with the step's gain 2.
fn up(side: &[f64; SIDE], line: &[f64], frames_in: usize, out: &mut [f64]) {
    for m in 0..frames_in {
        // Input m sits at the line's index HISTORY_UP + m. The even output
        // 2m has its centre half-way between inputs m − SIDE and
        // m − SIDE + 1 (DELAY_HIGH = 2 SIDE − 1 output samples back).
        let now = HISTORY_UP + m;
        let mut acc = 0.0;
        for (i, &t) in side.iter().enumerate() {
            acc += t * (line[now - SIDE - i] + line[now - SIDE + 1 + i]);
        }
        out[2 * m] = 2.0 * acc;
        out[2 * m + 1] = 2.0 * CENTRE * line[now - SIDE + 1];
    }
}

#[cfg(test)]
mod tests;
