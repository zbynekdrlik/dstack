use super::*;
use crate::{OVERSAMPLING, TAPS};

/// The engine for `from` to `to` Hz.
fn poly(from: u32, to: u32, channels: usize, block: usize, fixed: Fixed) -> Poly {
    Poly::new(from, to, channels, block, fixed, TAPS, OVERSAMPLING).unwrap()
}

/// `channels` channels of `frames` frames, channel-major, each its own
/// pattern.
fn signal(channels: usize, frames: usize, seed: usize) -> Vec<f64> {
    (0..channels * frames)
        .map(|i| (((i + seed) as f64) * 0.7311 + (i / frames) as f64).sin() * 0.6)
        .collect()
}

/// Runs `calls` calls of `p` on fresh signal; returns the inputs per
/// channel (from frame 0) and the last call's output frames.
fn feed(p: &mut Poly, calls: usize) -> (Vec<Vec<f64>>, usize) {
    let mut heard = vec![Vec::new(); p.channels];
    let mut out = vec![0.0; p.channels * p.output_frames_max()];
    let mut made = 0;
    for k in 0..calls {
        let n = p.input_frames_next();
        let x = signal(p.channels, n, 977 * k);
        for (c, h) in heard.iter_mut().enumerate() {
            h.extend_from_slice(&x[c * n..(c + 1) * n]);
        }
        made = p.process(&x, n, &mut out, p.output_frames_max()).unwrap().1;
    }
    (heard, made)
}

/// The FMA loop and the portable one both give, for every group width,
/// the plain sum of each output's kernel over its input frames (silence
/// before frame 0), read straight from the inputs: the planned rows, the
/// phases and the groups' layout are right, and so is each loop.
#[test]
fn both_loops_give_the_plain_sum_of_each_kernel() {
    for (from, to, fixed) in [
        (96_000, 44_100, Fixed::Input),
        (44_100, 96_000, Fixed::Output),
    ] {
        // 31 channels: one group of each width. A block of 640 puts every
        // phase in the last call, so the kernels whose last (257th) tap
        // is not 0 run too (the loops' odd tap past their partial sums).
        let mut p = poly(from, to, 31, 640, fixed);
        let (input, made) = feed(&mut p, 3);
        assert_eq!(p.groups.len(), 5);
        assert!(made > 0 && p.plan.len() == made);
        let mut phases: Vec<usize> = p.plan.iter().map(|&(_, ph)| ph).collect();
        phases.sort_unstable();
        phases.dedup();
        assert_eq!(phases.len() as u64, p.q, "{from}->{to}: every phase");
        // The last tap is the window's edge (3e-13 to 7.5e-12 at 96 ->
        // 44.1), still above the sums' 1e-12 bound below once doubled.
        assert!(
            phases.iter().any(|&ph| p.table.kernel(ph)[TAPS] != 0.0),
            "{from}->{to}: a kernel with its last tap"
        );
        let first_out = p.given - made as u64;
        let stride = made;
        let (mut fused, mut plain) = (vec![0.0; 31 * stride], vec![0.0; 31 * stride]);
        // SAFETY: dev1 and CI's runners have FMA; `Poly::new` read it.
        assert!(p.fma);
        unsafe { fma::convolve(&p.table, &p.plan, &p.groups, &mut fused, stride) };
        convolve(&p.table, &p.plan, &p.groups, &mut plain, stride);
        let mut ran = vec![0.0; 31 * stride];
        p.run(&mut ran, stride);
        for k in 0..made {
            let (frame, phase) = p.place(first_out + k as u64);
            let kernel = p.table.kernel(phase);
            for (c, x) in input.iter().enumerate() {
                let want: f64 = kernel
                    .iter()
                    .enumerate()
                    .map(|(i, w)| {
                        let at = frame + i as i64;
                        w * if at < 0 { 0.0 } else { x[at as usize] }
                    })
                    .sum();
                for (name, got) in [("fma", &fused), ("plain", &plain), ("run", &ran)] {
                    let v = got[c * stride + k];
                    assert!(
                        (v - want).abs() < 1e-12,
                        "{from}->{to} {name}: channel {c}, output {k}: {v} vs {want}"
                    );
                }
            }
        }
    }
}

/// The groups: widest first, every channel once, in order.
#[test]
fn channels_go_into_groups_widest_first() {
    let widths = |channels| {
        groups_for(channels, 3)
            .iter()
            .map(|g| (g.first, g.width, g.rows.len()))
            .collect::<Vec<_>>()
    };
    assert_eq!(widths(1), [(0, 1, 3)]);
    assert_eq!(widths(2), [(0, 2, 6)]);
    assert_eq!(widths(3), [(0, 2, 6), (2, 1, 3)]);
    assert_eq!(
        widths(64),
        [(0, 16, 48), (16, 16, 48), (32, 16, 48), (48, 16, 48)]
    );
    assert_eq!(
        widths(31),
        [
            (0, 16, 48),
            (16, 8, 24),
            (24, 4, 12),
            (28, 2, 6),
            (30, 1, 3)
        ]
    );
}

/// The core's counts: a fixed input gives `floor((T - 2) * to / from)`
/// for `T` inputs (none under 2), a fixed output takes `ceil(J * from /
/// to) + 1` for `J` outputs (none for none).
#[test]
fn the_counts_are_the_core_s_formulas() {
    // 96 -> 44.1 kHz: 147 outputs for 320 inputs.
    let p = poly(96_000, 44_100, 1, 16, Fixed::Input);
    assert_eq!((p.p, p.q), (320, 147));
    let given: Vec<u64> = [0, 1, 2, 3, 4, 5, 322, 323, 643]
        .iter()
        .map(|&t| p.given_after(t))
        .collect();
    assert_eq!(given, [0, 0, 0, 0, 0, 1, 147, 147, 294]);
    let taken: Vec<u64> = [0, 1, 2, 147, 148]
        .iter()
        .map(|&j| p.taken_for(j))
        .collect();
    assert_eq!(taken, [0, 4, 6, 321, 324]);
}

/// Output `j` sits at input `(j + 1) * from / to - (L - 1)`: its frame
/// and phase, and a kernel that starts a frame early below the first grid
/// point of the prototype only.
#[test]
fn an_output_s_place_is_exact() {
    let p = poly(96_000, 44_100, 1, 16, Fixed::Input);
    // j = 0: 320 / 147 - 255 = -253 + 26 / 147.
    assert_eq!(p.place(0), (-253, 26));
    // j = 146: 147 * 320 / 147 - 255 = 65, phase 0, a frame early.
    assert_eq!(p.table.lead[0], 1);
    assert_eq!(p.place(146), (64, 0));
    assert_eq!(p.place(146 + 147), (64 + 320, 0));
    // Phase 1 is past the first grid point (256 / 147 >= 1).
    assert_eq!(p.table.lead[1], 0);
    assert!(p.table.lead.iter().skip(1).all(|&l| l == 0));
    // 44.1 -> 96: phases below 1 / 256 of a frame start early.
    let up = poly(44_100, 96_000, 1, 16, Fixed::Input);
    assert_eq!(up.table.lead.iter().filter(|&&l| l == 1).count(), 2);
    assert_eq!(
        (up.table.lead[0], up.table.lead[1], up.table.lead[2]),
        (1, 1, 0)
    );
}

/// Every phase's kernel passes DC at unit gain (the prototype's rows are
/// scaled so, and the blend's weights sum to 1), on both sides of a rate
/// change; it is `taps + 1` long.
#[test]
fn every_kernel_passes_dc_at_unit_gain() {
    for (from, to) in [(96_000, 44_100), (44_100, 96_000)] {
        let p = poly(from, to, 1, 16, Fixed::Input);
        assert_eq!(p.table.width, TAPS + 1);
        for phase in 0..p.q as usize {
            let k = p.table.kernel(phase);
            assert_eq!(k.len(), TAPS + 1);
            let sum: f64 = k.iter().sum();
            assert!((sum - 1.0).abs() < 1e-6, "{from}->{to} {phase}: {sum}");
        }
    }
}

/// The history never holds more than `taps + 3` frames before a call's
/// input (the room assumes it), however the calls run: rows from the next
/// output's first frame to the input taken. And the groups never grow.
#[test]
fn the_history_stays_within_its_room() {
    for (from, to) in [
        (96_000, 44_100),
        (44_100, 96_000),
        (96_000, 22_050),
        (48_000, 44_100),
    ] {
        for fixed in [Fixed::Input, Fixed::Output] {
            for block in [1, 16, 37, 256] {
                let mut p = poly(from, to, 3, block, fixed);
                let lens: Vec<usize> = p.groups.iter().map(|g| g.rows.len()).collect();
                let mut out = vec![0.0; 3 * p.output_frames_max()];
                // Long enough for the history to move several times.
                let calls = (4 * p.room / block).min(400) + 8;
                for k in 0..calls {
                    let (base, _) = p.place(p.given);
                    let needed = p.taken as i64 - base;
                    assert!(
                        needed <= TAPS as i64 + 3,
                        "{from}->{to} {fixed:?} {block} call {k}: {needed}"
                    );
                    let n = p.input_frames_next();
                    let x = signal(3, n, k);
                    p.process(&x, n, &mut out, p.output_frames_max()).unwrap();
                }
                let after: Vec<usize> = p.groups.iter().map(|g| g.rows.len()).collect();
                assert_eq!(lens, after);
            }
        }
    }
}

/// A reset forgets the history: the same input after it gives the same
/// output as from new.
#[test]
fn a_reset_starts_from_silence_again() {
    let mut p = poly(96_000, 44_100, 2, 64, Fixed::Input);
    let mut fresh = poly(96_000, 44_100, 2, 64, Fixed::Input);
    feed(&mut p, 5);
    p.reset();
    assert_eq!((p.taken, p.given), (0, 0));
    let x = signal(2, 64 * 8, 3);
    let room = p.output_frames_max();
    let (mut a, mut b) = (vec![0.0; 2 * room], vec![0.0; 2 * room]);
    for k in 0..8 {
        let block: Vec<f64> = (0..2)
            .flat_map(|c| x[c * 512 + k * 64..c * 512 + (k + 1) * 64].iter().copied())
            .collect();
        let ra = p.process(&block, 64, &mut a, room).unwrap();
        let rb = fresh.process(&block, 64, &mut b, room).unwrap();
        assert_eq!(ra, rb);
        assert_eq!(a, b, "call {k}");
    }
}

/// A skip on each side steps the counts as calls of silence do, and leaves
/// silence behind (the next calls' output as from silence).
#[test]
fn a_skip_moves_the_counts_and_keeps_silence() {
    let mut p = poly(96_000, 44_100, 1, 128, Fixed::Input);
    assert_eq!(p.skip(300), (300, 136));
    assert_eq!(p.skip(20), (20, 10));
    assert_eq!((p.taken, p.given), (320, 146));
    let mut q = poly(44_100, 96_000, 1, 128, Fixed::Output);
    assert_eq!(q.skip(0), (0, 0));
    assert_eq!(q.skip(320), (148, 320));
    assert_eq!(q.skip(1), (1, 1));
    let room = q.output_frames_max();
    let mut out = vec![1.0; room];
    let n = q.input_frames_next();
    q.process(&vec![0.0; n], n, &mut out, room).unwrap();
    assert!(out[..128].iter().all(|&v| v == 0.0));
}

/// The groups' room: two histories of `taps + 3` and a row, and two calls
/// (1032 rows at 256 taps and 256 frames a call), what every group holds.
#[test]
fn the_room_holds_two_histories_and_two_calls() {
    assert_eq!(room_for(256, 256), 1032);
    assert_eq!(room_for(256, 248), 1016);
    assert_eq!(room_for(48, 16), 136);
    let p = poly(44_100, 96_000, 21, 256, Fixed::Output);
    assert_eq!(p.room, room_for(TAPS, 248));
    for g in &p.groups {
        assert_eq!(g.rows.len(), p.room * g.width);
    }
}

/// The history moves only when a call's input would not fit: the rows a
/// call needs, the room, and one more.
#[test]
fn the_history_moves_only_when_the_input_would_not_fit() {
    assert!(!must_compact(900, 132, 1032));
    assert!(!must_compact(776, 256, 1032));
    assert!(must_compact(777, 256, 1032));
    assert!(must_compact(1032, 1, 1032));
    // A run: the first row stays while calls fit, and moves (to the next
    // output's first frame) on the call that would not.
    let mut p = poly(96_000, 44_100, 2, 256, Fixed::Input);
    let mut out = vec![0.0; 2 * p.output_frames_max()];
    let x = signal(2, 256, 1);
    let (mut moves, mut calls) = (0, 0);
    for _ in 0..40 {
        let (first, held) = (p.first, p.held);
        let fits = !must_compact(held, 256, p.room);
        let (base, _) = p.place(p.given);
        p.process(&x, 256, &mut out, p.output_frames_max()).unwrap();
        calls += 1;
        if fits {
            assert_eq!(p.first, first);
            assert_eq!(p.held, held + 256);
        } else {
            moves += 1;
            assert_eq!(p.first, base);
            assert_eq!(p.held as i64, p.taken as i64 - base);
        }
    }
    // 1032 rows of room, 256 a call, about 258 kept: a move every 3 calls.
    assert!((12..=14).contains(&moves), "{moves} moves in {calls} calls");
}
