// SPDX-License-Identifier: GPL-3.0-or-later

//! The 2:1 rate step between the network and the app (dstack #20,
//! 3.11.2026): the device runs at the network's rate (96 kHz on the site,
//! the rate every transmitter there sends), and an app at half of it
//! (the intercom's hub, 48 kHz) gets its blocks at its own rate. A Dante
//! flow needs both ends at one rate, so without this a 48 kHz device
//! cannot receive from a 96 kHz transmitter at all.
//!
//! Both rates run on the device's one media clock, so the step is
//! synchronous and exact: no drift to follow, no asynchronous resampler.
//! RX is low-passed and every second sample kept; TX is zero-stuffed and
//! low-passed, with gain 2. The filter is one half-band FIR ([`TAPS`],
//! a Kaiser-windowed sinc at a quarter of the network rate): flat to about
//! 20 kHz at 96 kHz, and the band that would fold into the app's audio
//! held down by at least [`STOP_DB`]. It delays each way by
//! (`TAPS` - 1) / 2 network samples (0.16 ms at 96 kHz).
//!
//! `process` allocates nothing: every buffer is sized at the start for the
//! largest block a device can have ([`MAX_FRAMES`]).

use dstack_audio_io::{Block, Process};

/// The filter's length, network samples (odd, so it has a centre tap).
pub const TAPS: usize = 63;
/// The largest block of the device, network frames
/// (`dstack::stream::BLOCKS`' last).
pub const MAX_FRAMES: usize = 256;
/// The least attenuation of the band that folds into the app's audio, dB.
pub const STOP_DB: f64 = 60.0;
/// The Kaiser window's beta.
const BETA: f64 = 8.0;

/// The zeroth-order modified Bessel function of the first kind (the
/// Kaiser window), by its series.
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0, 1.0);
    for k in 1..40 {
        let f = x / (2.0 * f64::from(k));
        term *= f * f;
        sum += term;
    }
    sum
}

/// The half-band low-pass: a sinc cut at a quarter of the rate, windowed,
/// its taps summing to 1.
pub fn half_band() -> [f64; TAPS] {
    let centre = (TAPS - 1) as f64 / 2.0;
    let mut h = [0.0; TAPS];
    for (n, tap) in h.iter_mut().enumerate() {
        let t = n as f64 - centre;
        let sinc = if t == 0.0 {
            0.5
        } else {
            (std::f64::consts::PI * t / 2.0).sin() / (std::f64::consts::PI * t)
        };
        let r = t / centre;
        let window = bessel_i0(BETA * (1.0 - r * r).max(0.0).sqrt()) / bessel_i0(BETA);
        *tap = sinc * window;
    }
    let sum: f64 = h.iter().sum();
    h.iter_mut().for_each(|tap| *tap /= sum);
    h
}

/// One channel's filter line: the last `TAPS - 1` network samples, then
/// room for a block.
#[derive(Debug, Clone)]
struct Line {
    x: Vec<f64>,
}

impl Line {
    fn new() -> Self {
        Line {
            x: vec![0.0; TAPS - 1 + MAX_FRAMES],
        }
    }

    fn clear(&mut self) {
        self.x.fill(0.0);
    }

    /// Appends `n` network samples from `fill` after the history, runs
    /// `out` over the line, and keeps the last `TAPS - 1` as history.
    fn run(&mut self, n: usize, fill: impl FnOnce(&mut [f64]), out: impl FnOnce(&[f64])) {
        fill(&mut self.x[TAPS - 1..TAPS - 1 + n]);
        out(&self.x[..TAPS - 1 + n]);
        self.x.copy_within(n..n + TAPS - 1, 0);
    }
}

/// The filter's output for the line `x` ending at index `end` (the newest
/// sample, `end >= TAPS - 1`).
fn fir(h: &[f64; TAPS], x: &[f64], end: usize) -> f64 {
    h.iter()
        .zip(x[end + 1 - TAPS..=end].iter().rev())
        .map(|(a, b)| a * b)
        .sum()
}

/// Network samples `input` (an even count) to half as many app samples in
/// `out`.
fn down(h: &[f64; TAPS], line: &mut Line, input: &[f64], out: &mut [f64]) {
    let n = input.len();
    line.run(
        n,
        |room| room.copy_from_slice(input),
        |x| {
            for (k, y) in out.iter_mut().enumerate().take(n / 2) {
                *y = fir(h, x, TAPS - 1 + 2 * k + 1);
            }
        },
    );
}

/// App samples `input` to twice as many network samples in `out`.
fn up(h: &[f64; TAPS], line: &mut Line, input: &[f64], out: &mut [f64]) {
    let n = 2 * input.len();
    line.run(
        n,
        |room| {
            for (pair, &s) in room.as_chunks_mut::<2>().0.iter_mut().zip(input) {
                pair[0] = 2.0 * s;
                pair[1] = 0.0;
            }
        },
        |x| {
            for (k, y) in out.iter_mut().enumerate().take(n) {
                *y = fir(h, x, TAPS - 1 + k);
            }
        },
    );
}

/// The app's processor behind the 2:1 step: it gets blocks of half the
/// device's frames, at the app's rate, with half the media time.
pub struct Halve<P> {
    inner: P,
    h: [f64; TAPS],
    rx: Vec<Line>,
    tx: Vec<Line>,
    app_in: Vec<f64>,
    app_out: Vec<f64>,
}

impl<P: Process> Halve<P> {
    /// `inner` on `inputs` RX and `outputs` TX channels.
    pub fn new(inner: P, inputs: usize, outputs: usize) -> Self {
        Halve {
            inner,
            h: half_band(),
            rx: vec![Line::new(); inputs],
            tx: vec![Line::new(); outputs],
            app_in: vec![0.0; inputs * MAX_FRAMES / 2],
            app_out: vec![0.0; outputs * MAX_FRAMES / 2],
        }
    }
}

impl<P: Process> Process for Halve<P> {
    fn process(&mut self, block: &mut Block<'_>) {
        let n = block.frames();
        if n % 2 != 0 || n > MAX_FRAMES {
            // No device block is odd or larger (dstack::stream::BLOCKS).
            block.zero_outputs();
            return;
        }
        let m = n / 2;
        let (ins, outs) = (self.rx.len(), self.tx.len());
        for (ch, line) in self.rx.iter_mut().enumerate() {
            let out = &mut self.app_in[ch * m..(ch + 1) * m];
            down(&self.h, line, block.input(ch), out);
        }
        let start = block.start();
        let mut app = Block::new(m, &self.app_in[..ins * m], &mut self.app_out[..outs * m]);
        if let Some(s) = start {
            app = app.at(s / 2);
        }
        self.inner.process(&mut app);
        for (ch, line) in self.tx.iter_mut().enumerate() {
            up(
                &self.h,
                line,
                &self.app_out[ch * m..(ch + 1) * m],
                block.output(ch),
            );
        }
    }

    fn discontinuity(&mut self) {
        self.rx
            .iter_mut()
            .chain(self.tx.iter_mut())
            .for_each(Line::clear);
        self.inner.discontinuity();
    }
}

#[cfg(test)]
mod tests;
