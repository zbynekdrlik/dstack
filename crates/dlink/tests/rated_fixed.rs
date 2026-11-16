// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #330: a fixed app block at 44.1 kHz on a 96 kHz device (block
//! 128, `A` = 59) stays exact for ten simulated minutes. Its own test
//! binary: the run takes about a minute unoptimised, more than the
//! mutation gate's 20 s bound on a unit test (the unit tests run the same
//! checks over 20 s, `rated::tests::fixed`).

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use dlink::Rated;
use dstack_audio_io::{Block, Process};

const A: usize = 59;

/// The app's calls, recorded without keeping the audio.
#[derive(Default)]
struct Seen {
    /// Calls whose block was not `A` frames, or did not follow on.
    wrong: u64,
    /// Calls whose play time is not A / R after the last one's, within an
    /// app frame (the frames' phase in their device block) and the ns
    /// rounding of each.
    wrong_play: u64,
    first: Option<i64>,
    last: Option<(u64, i64)>,
    /// The device block each call came in (set by the test).
    block: u64,
    /// The calls' device blocks against their index, summed for the fit.
    calls: u64,
    sum_x: f64,
    sum_xx: f64,
    sum_xy: f64,
}

/// The app: it sends 0.5 and records its calls where the test reads them.
struct Calls(Arc<Mutex<Seen>>);

impl Process for Calls {
    fn process(&mut self, block: &mut Block<'_>) {
        block.output(0).fill(0.5);
        self.0.lock().unwrap().see(block);
    }
}

impl Seen {
    fn see(&mut self, block: &Block<'_>) {
        let (start, play) = (block.start().unwrap(), block.play_at().unwrap());
        if block.frames() != A {
            self.wrong += 1;
        }
        if let Some((s, p)) = self.last {
            if start != s + A as u64 {
                self.wrong += 1;
            }
            let step = (play - p) as f64;
            if (step - A as f64 * 1e9 / 44_100.0).abs() > 1e9 / 44_100.0 + 2.0 {
                self.wrong_play += 1;
            }
        }
        self.first.get_or_insert(play);
        self.last = Some((start, play));
        let (x, y) = (self.block as f64, self.calls as f64);
        self.sum_x += x;
        self.sum_xx += x * x;
        self.sum_xy += x * y;
        self.calls += 1;
    }
}

/// Over ten simulated minutes every call gets exactly `A` frames in
/// follow-on time and play time, at most one call comes in a device block
/// (so the RX FIFO, which then always holds less than `A`, never runs
/// dry), the app's 0.5 leaves the device whole and the TX FIFO never runs
/// dry, and the calls come at R / A a second within 1 ppm (a least-squares
/// fit over every call, and the mean play-time step).
#[test]
fn a_fixed_block_at_44_1_khz_stays_exact_for_ten_minutes() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let app = Calls(Arc::clone(&seen));
    let mut r = Rated::with_fixed_app_block(app, 96_000, 44_100, 128, 1, 1).unwrap();
    assert_eq!(r.app_block(), Some(A));
    let dry = r.dry();
    let blocks = 600 * 96_000 / 128u64;
    let input = [0.25f64; 128];
    let mut out = [0.0f64; 128];
    let mut twice = 0u64;
    let mut cut = 0u64;
    let mut calls = 0u64;
    for b in 0..blocks {
        let before = {
            let mut s = seen.lock().unwrap();
            s.block = b;
            s.calls
        };
        r.process(
            &mut Block::new(128, &input, &mut out)
                .at(b * 128)
                .played_at(b as i64 * 4_000_000 / 3),
        );
        calls = seen.lock().unwrap().calls;
        if calls > before + 1 {
            twice += 1;
        }
        if b > 100 && !out.iter().all(|v| (v - 0.5).abs() < 1e-6) {
            cut += 1;
        }
    }
    let c = seen.lock().unwrap();
    assert_eq!((c.wrong, c.wrong_play, twice, cut), (0, 0, 0, 0));
    assert_eq!(dry.load(Ordering::Relaxed), 0);
    let want = 128.0 * 44_100.0 / (96_000.0 * A as f64);
    let n = calls as f64;
    let mean_y = (n - 1.0) / 2.0;
    let slope = (c.sum_xy - c.sum_x * mean_y) / (c.sum_xx - c.sum_x * c.sum_x / n);
    let ppm = (slope / want - 1.0) * 1e6;
    assert!(
        ppm.abs() < 1.0,
        "{slope} calls a block, {ppm} ppm off {want}"
    );
    let span = (c.last.unwrap().1 - c.first.unwrap()) as f64 / (n - 1.0);
    let ppm = (span / (A as f64 * 1e9 / 44_100.0) - 1.0) * 1e6;
    assert!(ppm.abs() < 1.0, "a call every {span} ns, {ppm} ppm");
    assert!((blocks as f64 * want - n).abs() < 2.0, "{n} calls");
}
