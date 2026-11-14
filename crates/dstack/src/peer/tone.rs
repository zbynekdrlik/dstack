// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The peer's tones (dstack #305): a sine per TX channel on the media
//! sample grid, and a continuity check of each received tone.
//!
//! [`tone_at`]: the TX tone of a whole-Hz frequency at media sample `n`,
//! its phase from `n` itself (`n × f mod rate`, exact in integers), so a
//! receiver hears one unbroken sine for as long as the media clock runs on.
//!
//! [`ToneCheck`]: the received tone needs no known frequency. Each block is
//! fitted to a pure sine's recurrence `x[n+1] = 2c·x[n] − x[n−1]` (`c` =
//! cos ω, least squares over the block). A block whose own fit leaves no
//! residual over its threshold (a clean sine) sets the frequency and level
//! the NEXT block is judged by. Judged per sample: the recurrence residual
//! (a sample out of place: a skip, a repeat, a jump) over the threshold, or
//! the sine's energy invariant `x[n]² + x[n−1]² − 2c·x[n]·x[n−1]` (= A²
//! sin²ω, whatever the phase) under a quarter of the tone's (silence, a
//! level drop) is bad. A run of bad samples is one gap (`tone_gaps`); it
//! ends after [`CLEAR`] good samples in a row.
//!
//! Why not [`crate::media::check::SineCheck`] (the HIL sine check): it
//! needs the frequency up front (the peer hears whatever tone the app
//! sends), and it leaves a silence blind with a discontinuity at each of
//! its edges and passes a fade; a CI gate wants one count a dropout and a
//! level drop counted. The two share only the recurrence.
//!
//! The threshold is a quarter of the smallest residual a one-sample skip
//! leaves, `A·(1 − c)` (`2A·sin²(ω/2)`): 1.1e-3 for a 1 kHz tone at -6 dBFS
//! at 48 kHz, far above 24-bit quantization (1.2e-7). A lower tone gives a
//! smaller margin (200 Hz at 96 kHz: 2.1e-5); keep tones at 200 Hz or over.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The TX tones' level, linear (-6 dBFS).
pub const LEVEL: f64 = 0.5;
/// Good samples in a row that end a gap.
pub const CLEAR: u32 = 16;
/// The least amplitude a received tone is locked on (-60 dBFS).
pub const MIN_AMPLITUDE: f64 = 1e-3;
/// The highest |cos ω| a tone may have: under it, the tone is neither DC
/// nor at Nyquist (about 7 Hz from either at 48 kHz).
const MAX_ABS_C: f64 = 0.99999;

/// The TX tone of `hz` (whole Hz, under `rate / 2`) at media sample `n`,
/// at [`LEVEL`].
pub fn tone_at(n: u64, hz: u32, rate: u32) -> f64 {
    let rate = u128::from(rate.max(1));
    let phase = (u128::from(n) * u128::from(hz)) % rate;
    LEVEL * (std::f64::consts::TAU * phase as f64 / rate as f64).sin()
}

/// A received tone's frequency, Hz, from its `c` at `rate`.
pub fn hz_of(c: f64, rate: u32) -> f64 {
    c.acos() * f64::from(rate) / std::f64::consts::TAU
}

/// What a clean block's fit gives: the frequency's `c`, the energy
/// invariant's mean and the residual threshold.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fit {
    pub c: f64,
    pub energy: f64,
    pub threshold: f64,
}

impl Fit {
    /// The tone's amplitude.
    pub fn amplitude(&self) -> f64 {
        (self.energy / (1.0 - self.c * self.c)).sqrt()
    }
}

/// `x` (with the two samples before it, `before = [x[−2], x[−1]]`) fitted
/// to a sine: `Some` when it is one, clean (every residual within the
/// threshold), loud enough ([`MIN_AMPLITUDE`]) and neither DC nor Nyquist.
pub fn fit(before: [f64; 2], x: &[f64]) -> Option<Fit> {
    let at = |i: usize| -> f64 {
        match i {
            0 => before[0],
            1 => before[1],
            _ => x[i - 2],
        }
    };
    let len = x.len() + 2;
    let (mut num, mut den) = (0.0, 0.0);
    for i in 1..len - 1 {
        num += at(i) * (at(i + 1) + at(i - 1));
        den += 2.0 * at(i) * at(i);
    }
    let c = num / den;
    if c.is_nan() || c.abs() >= MAX_ABS_C {
        return None;
    }
    let mut energy = 0.0;
    for i in 1..len {
        energy += invariant(at(i), at(i - 1), c);
    }
    energy /= (len - 1) as f64;
    let fit = Fit {
        c,
        energy,
        threshold: 0.0,
    };
    let amplitude = fit.amplitude();
    if amplitude.is_nan() || amplitude < MIN_AMPLITUDE {
        return None;
    }
    let threshold = amplitude * (1.0 - c.abs()) / 4.0;
    for i in 2..len {
        if residual(at(i), at(i - 1), at(i - 2), c).abs() > threshold {
            return None;
        }
    }
    Some(Fit { threshold, ..fit })
}

/// The recurrence's residual at `x` after `p1`, `p2`.
fn residual(x: f64, p1: f64, p2: f64, c: f64) -> f64 {
    x - 2.0 * c * p1 + p2
}

/// The sine's energy invariant of `x` after `p1`.
fn invariant(x: f64, p1: f64, c: f64) -> f64 {
    x * x + p1 * p1 - 2.0 * c * x * p1
}

/// One RX channel's received tone, as the status reads it (written by the
/// pump thread).
#[derive(Debug, Default)]
pub struct RxTone {
    gaps: AtomicU64,
    samples: AtomicU64,
    locked: AtomicBool,
    hz: AtomicU64,
    amplitude: AtomicU64,
}

/// [`RxTone`] read at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RxToneView {
    /// Gaps since the first lock.
    pub gaps: u64,
    /// Samples received (every block of the channel's input).
    pub samples: u64,
    /// A tone was locked on at least once.
    pub locked: bool,
    /// The last clean block's frequency, Hz, and amplitude, linear.
    pub hz: f64,
    pub amplitude: f64,
}

impl RxTone {
    pub fn view(&self) -> RxToneView {
        RxToneView {
            gaps: self.gaps.load(Ordering::Relaxed),
            samples: self.samples.load(Ordering::Relaxed),
            locked: self.locked.load(Ordering::Relaxed),
            hz: f64::from_bits(self.hz.load(Ordering::Relaxed)),
            amplitude: f64::from_bits(self.amplitude.load(Ordering::Relaxed)),
        }
    }
}

/// The check of one RX channel's tone (the module doc), on the pump
/// thread: no allocation.
#[derive(Debug, Default, Clone)]
pub struct ToneCheck {
    /// `[x[n−2], x[n−1]]` and how many of them are real samples (0 to 2).
    before: [f64; 2],
    seen: u8,
    /// The last clean block's fit.
    fit: Option<Fit>,
    in_gap: bool,
    good_run: u32,
    gaps: u64,
}

impl ToneCheck {
    /// The gaps so far.
    pub fn gaps(&self) -> u64 {
        self.gaps
    }

    /// The last clean block's fit.
    pub fn fit(&self) -> Option<Fit> {
        self.fit
    }

    /// One block of the channel's samples: judged against the last clean
    /// block's fit, then fitted.
    pub fn block(&mut self, x: &[f64]) {
        if let (Some(f), 2) = (self.fit, self.seen) {
            let [mut p2, mut p1] = self.before;
            for &s in x {
                // Written so a NaN sample is bad: no comparison holds.
                let good = residual(s, p1, p2, f.c).abs() <= f.threshold
                    && invariant(s, p1, f.c) >= f.energy / 4.0;
                let bad = !good;
                self.judge(bad);
                (p2, p1) = (p1, s);
            }
        }
        if self.seen == 2
            && let Some(f) = fit(self.before, x)
        {
            self.fit = Some(f);
        }
        for &s in x {
            self.before = [self.before[1], s];
        }
        self.seen = if x.len() >= 2 || usize::from(self.seen) + x.len() >= 2 {
            2
        } else {
            self.seen + 1
        };
    }

    fn judge(&mut self, bad: bool) {
        if bad {
            self.good_run = 0;
            if !self.in_gap {
                self.in_gap = true;
                self.gaps += 1;
            }
        } else {
            self.good_run += 1;
            if self.good_run >= CLEAR {
                self.in_gap = false;
            }
        }
    }

    /// Publishes the counts to `out` with the channel's `samples` so far
    /// and the `rate` the frequency is read at.
    pub fn publish(&self, out: &RxTone, samples: u64, rate: u32) {
        out.gaps.store(self.gaps, Ordering::Relaxed);
        out.samples.store(samples, Ordering::Relaxed);
        if let Some(f) = self.fit {
            out.locked.store(true, Ordering::Relaxed);
            out.hz.store(hz_of(f.c, rate).to_bits(), Ordering::Relaxed);
            out.amplitude
                .store(f.amplitude().to_bits(), Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests;
