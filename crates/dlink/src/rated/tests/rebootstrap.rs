//! The app's timeline across a re-bootstrap (dstack #341, dasio#152): at a
//! ratio that does not divide the block (44.1 kHz on 96 kHz) an impulse
//! the app sends before a pump restart and hears after it comes back after
//! the same number of app samples, wherever the new start lands.

use super::*;

/// The app: sends one impulse at app frame `at` (its media time) on
/// output 0 and records every input frame of channel 0 with its media time.
struct Pulse {
    at: u64,
    heard: Vec<(u64, f64)>,
}

impl Process for Pulse {
    fn process(&mut self, block: &mut Block<'_>) {
        let Some(start) = block.start() else {
            return;
        };
        for (i, &v) in block.input(0).iter().enumerate() {
            self.heard.push((start + i as u64, v));
        }
        if let Some(i) = self.at.checked_sub(start)
            && (i as usize) < block.frames()
        {
            block.output(0)[i as usize] = 1.0;
        }
    }
}

const N: usize = 128;
/// The loop back through the network, device frames (50 ms at 96 kHz).
const LOOP: u64 = 4_800;
/// The impulse's app frame.
const AT: u64 = 2_000;
/// The first start: 896 * 147 / 320 = 411.6, not a whole app frame.
const FIRST: u64 = 7 * N as u64;

/// Runs a device whose output comes back to its input `LOOP` frames later;
/// with `restart = Some(gap)` the pump re-bootstraps once while the impulse
/// is in the network, `gap` blocks later than the next block (a stall).
/// Returns the impulse's round trip in app frames, from its peak with a
/// parabola through its neighbours.
fn round_trip(fixed: bool, restart: Option<u64>) -> f64 {
    let app = Pulse {
        at: AT,
        heard: Vec::new(),
    };
    let mut r = if fixed {
        Rated::with_fixed_app_block(app, 96_000, 44_100, N, 1, 1)
    } else {
        Rated::new(app, 96_000, 44_100, N, 1, 1)
    }
    .unwrap();
    let mut net = vec![0.0; 16_000];
    // Well after the impulse left the TX converter, well before it is back.
    let stall = AT * 96_000 / 44_100 + 1_500;
    let mut pending = restart;
    let mut s = FIRST;
    while s < 14_000 {
        if s >= stall
            && let Some(gap) = pending.take()
        {
            r.discontinuity();
            s += gap * N as u64;
        }
        let input: Vec<f64> = (s..s + N as u64)
            .map(|t| t.checked_sub(LOOP).map_or(0.0, |t| net[t as usize]))
            .collect();
        let mut out = vec![0.0; N];
        r.process(&mut Block::new(N, &input, &mut out).at(s));
        net[s as usize..s as usize + N].copy_from_slice(&out);
        s += N as u64;
    }
    let heard = &r.inner.heard;
    let peak = (1..heard.len() - 1)
        .max_by(|&a, &b| heard[a].1.abs().total_cmp(&heard[b].1.abs()))
        .unwrap();
    let (y0, y1, y2) = (heard[peak - 1].1, heard[peak].1, heard[peak + 1].1);
    assert_eq!(heard[peak + 1].0, heard[peak - 1].0 + 2, "a gap at the peak");
    let shift = 0.5 * (y0 - y2) / (y0 - 2.0 * y1 + y2);
    heard[peak].0 as f64 + shift - AT as f64
}

#[test]
fn an_impulse_comes_back_after_the_same_app_samples_across_a_re_bootstrap() {
    for fixed in [false, true] {
        let want = round_trip(fixed, None);
        // Gaps of 0 to 4 blocks put the new start on every app phase the
        // 128-frame grid has at 44.1 kHz (5: 640 device frames are 294
        // app frames).
        for gap in 0..5 {
            let got = round_trip(fixed, Some(gap));
            assert_eq!(
                got.round(),
                want.round(),
                "fixed {fixed}, gap {gap}: {got} app frames, not {want}"
            );
            assert!(
                (got - want).abs() < 0.01,
                "fixed {fixed}, gap {gap}: {got} app frames, not {want}"
            );
        }
    }
}
