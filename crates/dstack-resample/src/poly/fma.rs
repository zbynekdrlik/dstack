//! The table's loop for a CPU with FMA (x86_64), in AVX and FMA intrinsics
//! (dstack #462): one call per group of channels, as the portable
//! [`super::convolve`] but with no loop over the channels. Written out so,
//! a coverage build's counters (one a loop pass) cannot stop the
//! vectoriser: the portable loop's inner loops, each with its counter, ran
//! 10 to 60 times slower under `-C instrument-coverage` than rubato's
//! intrinsics, and CI's coverage job ran out of time.

use core::arch::x86_64::{
    __m128d, __m256d, _mm_add_pd, _mm_fmadd_pd, _mm_loadu_pd, _mm_set1_pd, _mm_setzero_pd,
    _mm_storeu_pd, _mm256_add_pd, _mm256_fmadd_pd, _mm256_loadu_pd, _mm256_set1_pd,
    _mm256_setzero_pd, _mm256_storeu_pd,
};

use super::{Group, Table};

/// Every group's outputs for `plan` into `output` (channel-major at
/// `stride`), as [`super::convolve`].
#[target_feature(enable = "avx,fma")]
pub(super) fn convolve(
    table: &Table,
    plan: &[(usize, usize)],
    groups: &[Group],
    output: &mut [f64],
    stride: usize,
) {
    for g in groups {
        let out = &mut output[g.first * stride..];
        match g.width {
            16 => outputs(table, plan, &g.rows, out, stride, dot16),
            8 => outputs(table, plan, &g.rows, out, stride, dot8),
            4 => outputs(table, plan, &g.rows, out, stride, dot4),
            2 => outputs(table, plan, &g.rows, out, stride, dot2),
            _ => outputs(table, plan, &g.rows, out, stride, dot1),
        }
    }
}

/// The outputs of one group of `G` channels through `dot`.
#[target_feature(enable = "avx,fma")]
fn outputs<const G: usize>(
    table: &Table,
    plan: &[(usize, usize)],
    rows: &[f64],
    out: &mut [f64],
    stride: usize,
    dot: unsafe fn(&[f64], &[f64]) -> [f64; G],
) {
    let w = table.width;
    for (k, &(row, phase)) in plan.iter().enumerate() {
        // SAFETY: `dot` is one of this module's, compiled for AVX and FMA,
        // which this function has too.
        let sum = unsafe { dot(table.kernel(phase), &rows[row * G..(row + w) * G]) };
        for (c, v) in sum.into_iter().enumerate() {
            out[c * stride + k] = v;
        }
    }
}

/// `v`'s four lanes.
#[target_feature(enable = "avx")]
fn lanes(v: __m256d) -> [f64; 4] {
    let mut out = [0.0; 4];
    // SAFETY: `out` holds four f64.
    unsafe { _mm256_storeu_pd(out.as_mut_ptr(), v) };
    out
}

/// `v`'s two lanes.
#[target_feature(enable = "avx")]
fn lanes2(v: __m128d) -> [f64; 2] {
    let mut out = [0.0; 2];
    // SAFETY: `out` holds two f64.
    unsafe { _mm_storeu_pd(out.as_mut_ptr(), v) };
    out
}

/// Four f64 from `p`.
///
/// # Safety
///
/// `p` and the three f64 after it are readable.
#[target_feature(enable = "avx")]
unsafe fn load(p: *const f64) -> __m256d {
    // SAFETY: the caller's.
    unsafe { _mm256_loadu_pd(p) }
}

/// Sixteen channels: four registers, one tap a pass.
#[target_feature(enable = "avx,fma")]
fn dot16(kernel: &[f64], rows: &[f64]) -> [f64; 16] {
    let mut a = [_mm256_setzero_pd(); 4];
    for (&w, f) in kernel.iter().zip(rows.as_chunks::<16>().0) {
        let w = _mm256_set1_pd(w);
        let p = f.as_ptr();
        // SAFETY: `f` holds 16 f64.
        unsafe {
            a[0] = _mm256_fmadd_pd(w, load(p), a[0]);
            a[1] = _mm256_fmadd_pd(w, load(p.add(4)), a[1]);
            a[2] = _mm256_fmadd_pd(w, load(p.add(8)), a[2]);
            a[3] = _mm256_fmadd_pd(w, load(p.add(12)), a[3]);
        }
    }
    let (l0, l1, l2, l3) = (lanes(a[0]), lanes(a[1]), lanes(a[2]), lanes(a[3]));
    std::array::from_fn(|c| [l0, l1, l2, l3][c / 4][c % 4])
}

/// Eight channels: two registers each for even and odd taps.
#[target_feature(enable = "avx,fma")]
fn dot8(kernel: &[f64], rows: &[f64]) -> [f64; 8] {
    let (mut a, mut b) = ([_mm256_setzero_pd(); 2], [_mm256_setzero_pd(); 2]);
    let (pairs, tail) = kernel.as_chunks::<2>();
    let (frames, rest) = rows.as_chunks::<16>();
    for (k, f) in pairs.iter().zip(frames) {
        let (wa, wb) = (_mm256_set1_pd(k[0]), _mm256_set1_pd(k[1]));
        let p = f.as_ptr();
        // SAFETY: `f` holds 16 f64 (two frames of eight).
        unsafe {
            a[0] = _mm256_fmadd_pd(wa, load(p), a[0]);
            a[1] = _mm256_fmadd_pd(wa, load(p.add(4)), a[1]);
            b[0] = _mm256_fmadd_pd(wb, load(p.add(8)), b[0]);
            b[1] = _mm256_fmadd_pd(wb, load(p.add(12)), b[1]);
        }
    }
    if let ([w], Some(f)) = (tail, rest.as_chunks::<8>().0.first()) {
        let w = _mm256_set1_pd(*w);
        let p = f.as_ptr();
        // SAFETY: `f` holds 8 f64.
        unsafe {
            a[0] = _mm256_fmadd_pd(w, load(p), a[0]);
            a[1] = _mm256_fmadd_pd(w, load(p.add(4)), a[1]);
        }
    }
    let (l0, l1) = (
        lanes(_mm256_add_pd(a[0], b[0])),
        lanes(_mm256_add_pd(a[1], b[1])),
    );
    std::array::from_fn(|c| [l0, l1][c / 4][c % 4])
}

/// Four channels: one register for each of four taps in turn.
#[target_feature(enable = "avx,fma")]
fn dot4(kernel: &[f64], rows: &[f64]) -> [f64; 4] {
    let mut a = [_mm256_setzero_pd(); 4];
    let (quads, tail) = kernel.as_chunks::<4>();
    let (frames, rest) = rows.as_chunks::<16>();
    for (k, f) in quads.iter().zip(frames) {
        let p = f.as_ptr();
        // SAFETY: `f` holds 16 f64 (four frames of four).
        unsafe {
            a[0] = _mm256_fmadd_pd(_mm256_set1_pd(k[0]), load(p), a[0]);
            a[1] = _mm256_fmadd_pd(_mm256_set1_pd(k[1]), load(p.add(4)), a[1]);
            a[2] = _mm256_fmadd_pd(_mm256_set1_pd(k[2]), load(p.add(8)), a[2]);
            a[3] = _mm256_fmadd_pd(_mm256_set1_pd(k[3]), load(p.add(12)), a[3]);
        }
    }
    for (&w, f) in tail.iter().zip(rest.as_chunks::<4>().0) {
        // SAFETY: `f` holds 4 f64.
        a[0] = _mm256_fmadd_pd(_mm256_set1_pd(w), unsafe { load(f.as_ptr()) }, a[0]);
    }
    let sum = _mm256_add_pd(_mm256_add_pd(a[0], a[1]), _mm256_add_pd(a[2], a[3]));
    lanes(sum)
}

/// Two channels: a 128-bit register for each of four taps in turn.
#[target_feature(enable = "avx,fma")]
fn dot2(kernel: &[f64], rows: &[f64]) -> [f64; 2] {
    let mut a = [_mm_setzero_pd(); 4];
    let (quads, tail) = kernel.as_chunks::<4>();
    let (frames, rest) = rows.as_chunks::<8>();
    for (k, f) in quads.iter().zip(frames) {
        let p = f.as_ptr();
        // SAFETY: `f` holds 8 f64 (four frames of two).
        unsafe {
            a[0] = _mm_fmadd_pd(_mm_set1_pd(k[0]), _mm_loadu_pd(p), a[0]);
            a[1] = _mm_fmadd_pd(_mm_set1_pd(k[1]), _mm_loadu_pd(p.add(2)), a[1]);
            a[2] = _mm_fmadd_pd(_mm_set1_pd(k[2]), _mm_loadu_pd(p.add(4)), a[2]);
            a[3] = _mm_fmadd_pd(_mm_set1_pd(k[3]), _mm_loadu_pd(p.add(6)), a[3]);
        }
    }
    for (&w, f) in tail.iter().zip(rest.as_chunks::<2>().0) {
        // SAFETY: `f` holds 2 f64.
        a[0] = _mm_fmadd_pd(_mm_set1_pd(w), unsafe { _mm_loadu_pd(f.as_ptr()) }, a[0]);
    }
    lanes2(_mm_add_pd(_mm_add_pd(a[0], a[1]), _mm_add_pd(a[2], a[3])))
}

/// One channel: its frames are contiguous, so the taps go four to a
/// register, sixteen a pass.
#[target_feature(enable = "avx,fma")]
fn dot1(kernel: &[f64], rows: &[f64]) -> [f64; 1] {
    let mut a = [_mm256_setzero_pd(); 4];
    let (ks, tail) = kernel.as_chunks::<16>();
    let (rs, rest) = rows[..kernel.len()].as_chunks::<16>();
    for (k, r) in ks.iter().zip(rs) {
        let (pk, pr) = (k.as_ptr(), r.as_ptr());
        // SAFETY: `k` and `r` hold 16 f64 each.
        unsafe {
            a[0] = _mm256_fmadd_pd(load(pk), load(pr), a[0]);
            a[1] = _mm256_fmadd_pd(load(pk.add(4)), load(pr.add(4)), a[1]);
            a[2] = _mm256_fmadd_pd(load(pk.add(8)), load(pr.add(8)), a[2]);
            a[3] = _mm256_fmadd_pd(load(pk.add(12)), load(pr.add(12)), a[3]);
        }
    }
    let l = lanes(_mm256_add_pd(
        _mm256_add_pd(a[0], a[1]),
        _mm256_add_pd(a[2], a[3]),
    ));
    let mut sum = (l[0] + l[1]) + (l[2] + l[3]);
    for (&w, &x) in tail.iter().zip(rest) {
        sum = w.mul_add(x, sum);
    }
    [sum]
}
