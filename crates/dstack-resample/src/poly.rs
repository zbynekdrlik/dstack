// SPDX-License-Identifier: MIT OR Apache-2.0

//! The fixed-ratio polyphase engine (dstack #462): the sinc core's filter
//! for a ratio that never moves, with every output's kernel built once.
//!
//! A ratio on one clock places every output at a fixed fraction of an
//! input frame, and the fractions repeat with the ratio's period: output
//! `j` sits at input `(j + 1) * from / to - (L - 1)` after a reset (`L`
//! the sinc's length), so `to / gcd` phases exist (147 for 96 -> 44.1 kHz,
//! 320 for 44.1 -> 96). A table holds one kernel a phase: the one
//! rubato's `Async` builds at that phase from its 256x oversampled
//! prototype by its 4-point cubic blend (same window, same cutoff, same
//! blend), so the filter, the delay and each output's place are the sinc
//! core's: their outputs differ by the core's own float error (up to
//! 3.6e-10 of a near full-scale signal). The positions are exact integers,
//! not a sum of floats (THD+N of a 1 kHz tone 222-228 dB, the core's
//! 194-219 dB); the frame counts are the core's own formulas (a fixed
//! input gives `floor((T - 2) * to / from)` outputs for `T` inputs, a fixed
//! output takes `ceil(J * from / to) + 1` inputs for `J` outputs).
//!
//! What makes it fast is the layout, not the kernel (rubato already
//! shares one kernel a frame across channels): the history is kept in
//! channel groups (16, then 8, 4, 2 and 1 for the rest), each frame-major
//! and contiguous, so one tap weights a whole group at once and one
//! output's window (257 rows of 16 channels, 32 KB) stays in the L1 cache
//! from one output to the next. rubato reads 64 separate per-channel
//! windows a frame (128 KB). On x86_64 the loop runs in a copy compiled
//! for FMA when the CPU has it (asked once, when the engine is made).
//!
//! Nothing allocates after the engine is made.

use crate::{ConvertError, Fixed};

#[cfg(test)]
mod tests;

/// The most phases a table may hold: 1024 kernels of 257 taps are 2.1 MB.
/// A ratio with more (11.025 -> 192 kHz has 2560) keeps the sinc core.
pub const MAX_PHASES: u64 = 1024;

/// The channel groups' widths, widest first: the channels are split into
/// groups of these, each kept contiguous.
const WIDTHS: [usize; 5] = [16, 8, 4, 2, 1];

/// Blackman-Harris's coefficients (the windowfunctions crate's, which
/// rubato squares for `BlackmanHarris2`).
const BH: [f64; 4] = [0.35875, 0.48829, 0.14128, 0.01168];

/// The ratio's period, (input frames, output frames) a period:
/// `(from / gcd, to / gcd)`. Both rates are a converter's, never 0.
pub(crate) fn period(from: u32, to: u32) -> (u64, u64) {
    let (from, to) = (u64::from(from), u64::from(to));
    let g = crate::gcd(from, to);
    (from / g, to / g)
}

/// The windowed sinc prototype rubato builds (`sinc.rs` `make_sincs`):
/// `taps * factor` points of `BlackmanHarris2` times the sinc at `cutoff`,
/// scaled so each of the `factor` interleaved rows sums to about 1. Row
/// `s`, tap `k` is point `factor * k + factor - 1 - s`.
fn prototype(taps: usize, factor: usize, cutoff: f32) -> Vec<f64> {
    let total = taps * factor;
    let len = total as f64;
    let half = (total / 2) as f64;
    let (fc, fac) = (f64::from(cutoff), factor as f64);
    let pi = std::f64::consts::PI;
    let mut y = Vec::with_capacity(total);
    let mut sum = 0.0;
    for m in 0..total {
        let x = m as f64;
        let bh = BH[0] - BH[1] * (2.0 * pi * x / len).cos() + BH[2] * (4.0 * pi * x / len).cos()
            - BH[3] * (6.0 * pi * x / len).cos();
        let arg = (x - half) * fc / fac;
        let sinc = if arg == 0.0 {
            1.0
        } else {
            (arg * pi).sin() / (arg * pi)
        };
        let v = bh * bh * sinc;
        sum += v;
        y.push(v);
    }
    let norm = sum / fac;
    for v in &mut y {
        *v /= norm;
    }
    y
}

/// rubato's cubic blend weights at `x` in [0, 1) for the points at -1, 0,
/// 1 and 2 (`asynchro_sinc.rs` `interp_cubic_weights`).
fn cubic_weights(x: f64) -> [f64; 4] {
    let x2 = x * x;
    let x3 = x2 * x;
    [
        (-1.0 / 3.0) * x + 0.5 * x2 - (1.0 / 6.0) * x3,
        1.0 - 0.5 * x - x2 + 0.5 * x3,
        x + 0.5 * x2 - 0.5 * x3,
        -(1.0 / 6.0) * x + (1.0 / 6.0) * x3,
    ]
}

/// One kernel a phase, `taps + 1` long.
#[derive(Debug, Clone)]
pub(crate) struct Table {
    /// Taps a kernel: the sinc's length and one more (the blend can reach
    /// one frame further).
    width: usize,
    /// The kernels, phase-major.
    kernels: Vec<f64>,
    /// Each phase's first tap: 1 frame before the output's frame, or 0.
    lead: Vec<usize>,
}

impl Table {
    /// The kernels of the `phases` phases of a sinc of `taps` taps,
    /// prototype `factor`x oversampled at `cutoff` (relative to the lower
    /// rate's Nyquist, as rubato's).
    pub(crate) fn new(taps: usize, factor: usize, cutoff: f32, phases: u64) -> Table {
        let y = prototype(taps, factor, cutoff);
        let row = |s: usize, k: usize| y[factor * k + factor - 1 - s];
        let width = taps + 1;
        let count = usize::try_from(phases).unwrap_or(usize::MAX);
        let mut kernels = vec![0.0; count * width];
        let mut lead = vec![0; count];
        let q = u128::from(phases);
        let f = factor as i64;
        for (phase, (kernel, lead)) in kernels.chunks_exact_mut(width).zip(&mut lead).enumerate() {
            // The output's fraction is phase / q; rubato's grid point below
            // it is `sub`, and the fraction past that point `x`.
            let at = phase as u128 * factor as u128;
            let sub = i64::try_from(at / q).unwrap_or(0);
            let x = (at % q) as f64 / phases as f64;
            let points: [(i64, usize); 4] = std::array::from_fn(|i| {
                let s = sub - 1 + i as i64;
                if s < 0 {
                    (-1, (s + f) as usize)
                } else if s >= f {
                    (1, (s - f) as usize)
                } else {
                    (0, s as usize)
                }
            });
            let first = points.iter().map(|p| p.0).min().unwrap_or(0);
            *lead = usize::from(first < 0);
            for ((d, s), w) in points.iter().zip(cubic_weights(x)) {
                let shift = (d - first) as usize;
                for k in 0..taps {
                    kernel[shift + k] += w * row(*s, k);
                }
            }
        }
        Table {
            width,
            kernels,
            lead,
        }
    }

    /// Phase `phase`'s kernel.
    fn kernel(&self, phase: usize) -> &[f64] {
        &self.kernels[phase * self.width..(phase + 1) * self.width]
    }
}

/// `w * x + acc`, fused when `FMA`.
#[inline(always)]
fn madd<const FMA: bool>(w: f64, x: f64, acc: f64) -> f64 {
    if FMA { w.mul_add(x, acc) } else { acc + w * x }
}

/// One output of a group of `G` channels: `kernel` over `rows` (frame-major,
/// `G` values a frame, at least `kernel.len()` frames), in `U` interleaved
/// partial sums (more independent sums for a narrow group).
#[inline(always)]
fn dot<const G: usize, const U: usize, const FMA: bool>(kernel: &[f64], rows: &[f64]) -> [f64; G] {
    let mut acc = [[0.0; G]; U];
    let (ks, tail) = kernel.as_chunks::<U>();
    let frames = &rows.as_chunks::<G>().0[..kernel.len()];
    let (body, rest) = frames.split_at(ks.len() * U);
    for (k, r) in ks.iter().zip(body.as_chunks::<U>().0) {
        for u in 0..U {
            for c in 0..G {
                acc[u][c] = madd::<FMA>(k[u], r[u][c], acc[u][c]);
            }
        }
    }
    for (&w, r) in tail.iter().zip(rest) {
        for c in 0..G {
            acc[0][c] = madd::<FMA>(w, r[c], acc[0][c]);
        }
    }
    let mut sum = [0.0; G];
    for a in &acc {
        for c in 0..G {
            sum[c] += a[c];
        }
    }
    sum
}

/// The outputs of one group of `G` channels: `plan` gives each output's
/// first row and phase; output `k` of the group's channel `c` goes to
/// `out[c * stride + k]`.
#[inline(always)]
fn group<const G: usize, const U: usize, const FMA: bool>(
    table: &Table,
    plan: &[(usize, usize)],
    rows: &[f64],
    out: &mut [f64],
    stride: usize,
) {
    let w = table.width;
    for (k, &(row, phase)) in plan.iter().enumerate() {
        let sum = dot::<G, U, FMA>(table.kernel(phase), &rows[row * G..(row + w) * G]);
        for (c, v) in sum.into_iter().enumerate() {
            out[c * stride + k] = v;
        }
    }
}

/// Channels `first..first + width`, frame-major from the history's first
/// frame.
#[derive(Debug, Clone)]
struct Group {
    first: usize,
    width: usize,
    rows: Vec<f64>,
}

/// Every group's outputs for `plan` into `output` (channel-major at
/// `stride`).
#[inline(always)]
fn convolve<const FMA: bool>(
    table: &Table,
    plan: &[(usize, usize)],
    groups: &[Group],
    output: &mut [f64],
    stride: usize,
) {
    for g in groups {
        let out = &mut output[g.first * stride..];
        let rows = &g.rows[..];
        match g.width {
            16 => group::<16, 1, FMA>(table, plan, rows, out, stride),
            8 => group::<8, 4, FMA>(table, plan, rows, out, stride),
            4 => group::<4, 4, FMA>(table, plan, rows, out, stride),
            2 => group::<2, 8, FMA>(table, plan, rows, out, stride),
            _ => group::<1, 8, FMA>(table, plan, rows, out, stride),
        }
    }
}

/// [`convolve`] compiled for FMA.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "fma")]
fn convolve_fma(
    table: &Table,
    plan: &[(usize, usize)],
    groups: &[Group],
    output: &mut [f64],
    stride: usize,
) {
    convolve::<true>(table, plan, groups, output, stride);
}

/// The channel groups for `channels` channels, [`WIDTHS`] widest first,
/// each with room for `rows` frames.
fn groups_for(channels: usize, rows: usize) -> Vec<Group> {
    let mut groups = Vec::new();
    let mut first = 0;
    for width in WIDTHS {
        while channels - first >= width {
            groups.push(Group {
                first,
                width,
                rows: vec![0.0; rows * width],
            });
            first += width;
        }
    }
    groups
}

/// The fixed-ratio engine for `channels` channels, `block` frames on its
/// [`Fixed`] side.
#[derive(Debug, Clone)]
pub(crate) struct Poly {
    table: Table,
    /// Input and output frames a period (`to / gcd` is the phase count).
    p: u64,
    q: u64,
    /// The sinc's length.
    taps: usize,
    fixed: Fixed,
    block: usize,
    channels: usize,
    groups: Vec<Group>,
    /// The input frame (since the reset) of the history's first row, and
    /// the rows held.
    first: i64,
    held: usize,
    /// Rows a group has room for.
    room: usize,
    /// Input frames taken and output frames given since the reset.
    taken: u64,
    given: u64,
    /// Each output's first row and phase in this call.
    plan: Vec<(usize, usize)>,
    /// The CPU has FMA (x86_64): the loop runs [`convolve_fma`].
    fma: bool,
    /// The most frames a call takes and gives.
    max_in: usize,
    max_out: usize,
}

impl Poly {
    /// The engine from `from` to `to` Hz, or `None` when the ratio has
    /// more than [`MAX_PHASES`] phases. Rates, channels and block are a
    /// converter's (checked by it, never 0).
    pub(crate) fn new(
        from: u32,
        to: u32,
        channels: usize,
        block: usize,
        fixed: Fixed,
        taps: usize,
        factor: usize,
    ) -> Option<Poly> {
        let (p, q) = period(from, to);
        if q > MAX_PHASES {
            return None;
        }
        let ratio = f64::from(to) / f64::from(from);
        // rubato's automatic cutoff (`resolve_cutoff`), in f32 as there.
        let auto = rubato::calculate_cutoff::<f32>(taps, rubato::WindowFunction::BlackmanHarris2);
        let cutoff = if ratio >= 1.0 {
            auto
        } else {
            auto * ratio as f32
        };
        // The core's sizes (`calculate_max_*_size`, a ratio that never
        // moves).
        let (max_in, max_out) = match fixed {
            Fixed::Input => (block, (block as f64 * ratio + 10.0) as usize),
            Fixed::Output => (
                ((block as f64 / ratio).ceil() + 2.0 + (taps / 2) as f64) as usize,
                block,
            ),
        };
        // The history never needs more than taps + 3 rows before a call
        // appends its input (module tests): room for two calls more, so
        // most calls move nothing.
        let room = 2 * (taps + 4) + 2 * max_in;
        let mut poly = Poly {
            table: Table::new(taps, factor, cutoff, q),
            p,
            q,
            taps,
            fixed,
            block,
            channels,
            groups: groups_for(channels, room),
            first: 0,
            held: 0,
            room,
            taken: 0,
            given: 0,
            plan: Vec::with_capacity(max_out),
            fma: false,
            max_in,
            max_out,
        };
        #[cfg(target_arch = "x86_64")]
        {
            poly.fma = std::arch::is_x86_feature_detected!("fma");
        }
        poly.reset();
        Some(poly)
    }

    /// Output `j`'s first input frame (since the reset) and its phase.
    fn place(&self, j: u64) -> (i64, usize) {
        let taps = i128::try_from(self.taps).unwrap_or(i128::MAX);
        let q = i128::from(self.q);
        let n = (i128::from(j) + 1) * i128::from(self.p) - (taps - 1) * q;
        let phase = usize::try_from(n.rem_euclid(q)).unwrap_or(0);
        let frame = i64::try_from(n.div_euclid(q)).unwrap_or(i64::MAX);
        (frame - self.table.lead[phase] as i64, phase)
    }

    /// The outputs given in all once `taken` input frames came in (a fixed
    /// input): `floor((taken - 2) * to / from)`, none under 2.
    fn given_after(&self, taken: u64) -> u64 {
        let over = u128::from(taken.saturating_sub(2));
        u64::try_from(over * u128::from(self.q) / u128::from(self.p)).unwrap_or(u64::MAX)
    }

    /// The input frames taken in all once `given` outputs went out (a fixed
    /// output): `ceil(given * from / to) + 1`, none before the first.
    fn taken_for(&self, given: u64) -> u64 {
        if given == 0 {
            return 0;
        }
        let need = (u128::from(given) * u128::from(self.p)).div_ceil(u128::from(self.q));
        u64::try_from(need + 1).unwrap_or(u64::MAX)
    }

    /// (input frames, output frames) of the next call.
    fn next(&self) -> (u64, u64) {
        let block = self.block as u64;
        match self.fixed {
            Fixed::Input => (
                block,
                self.given_after(self.taken + block)
                    .saturating_sub(self.given),
            ),
            Fixed::Output => (self.taken_for(self.given + block) - self.taken, block),
        }
    }

    pub(crate) fn input_frames_next(&self) -> usize {
        usize::try_from(self.next().0).unwrap_or(usize::MAX)
    }

    pub(crate) fn input_frames_max(&self) -> usize {
        self.max_in
    }

    pub(crate) fn output_frames_max(&self) -> usize {
        self.max_out
    }

    /// The core's delay (`output_delay`): half the sinc at the output rate.
    pub(crate) fn delay_frames(&self, ratio: f64) -> usize {
        (self.taps as f64 * ratio / 2.0) as usize
    }

    /// Forgets the history: silence before input frame 0.
    pub(crate) fn reset(&mut self) {
        self.taken = 0;
        self.given = 0;
        self.restart_history();
    }

    /// History of silence from the next output's first frame to the input
    /// taken so far.
    fn restart_history(&mut self) {
        let taken = i64::try_from(self.taken).unwrap_or(i64::MAX);
        self.first = self.place(self.given).0.min(taken);
        self.held = usize::try_from(taken - self.first).unwrap_or(0);
        for g in &mut self.groups {
            g.rows[..self.held * g.width].fill(0.0);
        }
    }

    /// Steps over `frames` frames of silence on the fixed side, as calls
    /// of silence would (the history must be silence); returns the input
    /// frames taken and the output frames given.
    pub(crate) fn skip(&mut self, frames: u64) -> (u64, u64) {
        let (taken, given) = (self.taken, self.given);
        match self.fixed {
            Fixed::Input => {
                self.taken += frames;
                self.given = self.given_after(self.taken).max(self.given);
            }
            Fixed::Output => {
                self.given += frames;
                self.taken = self.taken_for(self.given).max(self.taken);
            }
        }
        self.restart_history();
        (self.taken - taken, self.given - given)
    }

    /// Converts as [`crate::Converter::process`]: takes the next call's
    /// input frames from `input` (channel `c` at `c * frames_in`) and
    /// writes its outputs into `output` (channel `c` at `c * out_frames`).
    pub(crate) fn process(
        &mut self,
        input: &[f64],
        frames_in: usize,
        output: &mut [f64],
        out_frames: usize,
    ) -> Result<(usize, usize), ConvertError> {
        let (take, make) = self.next();
        let take = usize::try_from(take).unwrap_or(usize::MAX);
        let make = usize::try_from(make).unwrap_or(usize::MAX);
        let channels = self.channels;
        if frames_in < take
            || input.len() < channels * frames_in
            || out_frames < make
            || output.len() < channels * out_frames
        {
            return Err(ConvertError::Core(format!(
                "{channels} channels: {take} input frames into room for {make}, given \
                 {frames_in} frames ({} samples) and room for {out_frames} ({} samples)",
                input.len(),
                output.len()
            )));
        }
        // Drop the rows no output needs any more, when the input would not
        // fit after them.
        if self.held + take > self.room {
            let (base, _) = self.place(self.given);
            let drop = usize::try_from(base - self.first)
                .unwrap_or(0)
                .min(self.held);
            for g in &mut self.groups {
                g.rows.copy_within(drop * g.width..self.held * g.width, 0);
            }
            self.first += drop as i64;
            self.held -= drop;
        }
        for g in &mut self.groups {
            let w = g.width;
            for c in 0..w {
                let src = &input[(g.first + c) * frames_in..][..take];
                for (f, &v) in src.iter().enumerate() {
                    g.rows[(self.held + f) * w + c] = v;
                }
            }
        }
        self.held += take;
        self.taken += take as u64;
        self.plan.clear();
        for j in self.given..self.given + make as u64 {
            let (frame, phase) = self.place(j);
            self.plan
                .push((usize::try_from(frame - self.first).unwrap_or(0), phase));
        }
        self.given += make as u64;
        self.run(output, out_frames);
        Ok((take, make))
    }

    /// The call's outputs, through the FMA copy when the CPU has it.
    fn run(&self, output: &mut [f64], stride: usize) {
        #[cfg(target_arch = "x86_64")]
        if self.fma {
            // SAFETY: `fma` is set only when the CPU reported FMA
            // (`Poly::new`).
            unsafe { convolve_fma(&self.table, &self.plan, &self.groups, output, stride) };
            return;
        }
        convolve::<false>(&self.table, &self.plan, &self.groups, output, stride);
    }
}
