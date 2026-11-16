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
fn round_trip(fixed: bool, first: u64, restart: Option<u64>) -> f64 {
    let app = Pulse {
        at: AT,
        heard: Vec::new(),
    };
    let mut r = if fixed {
        Rated::with_fixed_app_block(app, 96_000, 44_100, N, 1, 2)
    } else {
        Rated::new(app, 96_000, 44_100, N, 1, 2)
    }
    .unwrap();
    let mut net = vec![0.0; 16_000];
    // Well after the impulse left the TX converter, well before it is back.
    let stall = AT * 96_000 / 44_100 + 1_500;
    let mut pending = restart;
    let mut s = first;
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
        let mut out = vec![0.0; 2 * N];
        r.process(&mut Block::new(N, &input, &mut out).at(s));
        net[s as usize..s as usize + N].copy_from_slice(&out[..N]);
        s += N as u64;
    }
    let heard = &r.inner.heard;
    let peak = (1..heard.len() - 1)
        .max_by(|&a, &b| heard[a].1.abs().total_cmp(&heard[b].1.abs()))
        .unwrap();
    let (y0, y1, y2) = (heard[peak - 1].1, heard[peak].1, heard[peak + 1].1);
    assert_eq!(
        heard[peak + 1].0,
        heard[peak - 1].0 + 2,
        "a gap at the peak"
    );
    let shift = 0.5 * (y0 - y2) / (y0 - 2.0 * y1 + y2);
    heard[peak].0 as f64 + shift - AT as f64
}

#[test]
fn a_restart_leads_with_the_blocks_back_to_a_whole_app_frame() {
    // 44.1 kHz on 96 kHz: a whole app frame every 320 device frames.
    for (start, lead) in [
        (0u64, 0u64),
        (128, 1),
        (256, 2),
        (640, 0),
        (896, 2),
        (1_152, 4),
    ] {
        assert_eq!(lead_blocks(start, 128, 96_000, 44_100), lead, "{start}");
    }
    // Off the block grid no start back is whole: the old label stays.
    assert_eq!(lead_blocks(1_000, 32, 96_000, 44_100), 0);
    // A whole ratio needs none.
    assert_eq!(lead_blocks(128, 128, 96_000, 48_000), 0);
    assert_eq!(lead_blocks(4_800, 128, 48_000, 96_000), 0);
    // 22.05 kHz on 96 kHz at 16 frames: 39 blocks, within MAX_LEAD.
    assert_eq!(lead_blocks(39 * 16, 16, 96_000, 22_050), 39);
    // 32 kHz on a 44.1 kHz device (a whole app frame every 441 device
    // frames) would need 440 blocks: past MAX_LEAD, the old label.
    assert_eq!(lead_blocks(440 * 128, 128, 44_100, 32_000), 0);
    assert_eq!(lead_blocks(441 * 128, 128, 44_100, 32_000), 0);
    assert_eq!(lead_blocks(442 * 128, 128, 44_100, 32_000), 1);
    // 44.1 kHz on 48 kHz: every 160 device frames.
    assert_eq!(lead_blocks(4 * 128, 128, 48_000, 44_100), 4);
    // At most MAX_LEAD blocks (one-frame blocks, a made-up rate whose
    // period is 96000 frames).
    assert_eq!(lead_blocks(MAX_LEAD, 1, 96_000, 44_099), MAX_LEAD);
    assert_eq!(lead_blocks(MAX_LEAD + 1, 1, 96_000, 44_099), 0);
    assert_eq!(gcd(96_000, 44_100), 300);
    assert_eq!(gcd(44_100, 96_000), 300);
    assert_eq!(gcd(7, 0), 7);
}

#[test]
fn a_device_frame_at_the_app_s_rate_is_exact() {
    // 640 * 147 / 320 = 294: whole, exactly.
    for k in 0..10_000u64 {
        assert_eq!(app_frame(640 * k, 96_000, 44_100), 294 * k);
    }
    assert_eq!(app_frame(896, 96_000, 44_100), 411);
    assert_eq!(app_frame(1_000, 96_000, 48_000), 500);
    assert_eq!(app_frame(u64::MAX, 48_000, 96_000), u64::MAX);
}

#[test]
fn an_impulse_comes_back_after_the_same_app_samples_across_a_re_bootstrap() {
    for fixed in [false, true] {
        let want = round_trip(fixed, FIRST, None);
        // Gaps of 0 to 4 blocks put the new start on every app phase the
        // 128-frame grid has at 44.1 kHz (5: 640 device frames are 294
        // app frames).
        for gap in 0..5 {
            let got = round_trip(fixed, FIRST, Some(gap));
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

/// The same from any first start, with no re-bootstrap: the TX side too
/// starts on a whole app frame (an impulse sent after a restart goes out
/// through the restarted TX converter).
#[test]
fn an_impulse_comes_back_after_the_same_app_samples_from_any_first_start() {
    for fixed in [false, true] {
        let want = round_trip(fixed, FIRST, None);
        // Leads of 0, 1, 3 and 4 blocks (FIRST's is 2).
        for first in [640, 768, 1_024, 1_152] {
            let got = round_trip(fixed, first, None);
            assert!(
                (got - want).abs() < 0.01,
                "fixed {fixed}, first {first}: {got} app frames, not {want}"
            );
        }
    }
}

/// The app's (media start, frames) for each device block start, from a
/// `Rated` started at `first` that restarts at `restart` (a discontinuity
/// and a jump to that start) and runs to device frame 20 000.
fn labels(ch: (usize, usize), first: u64, restart: Option<u64>) -> Vec<(u64, Option<u64>, usize)> {
    let mut r = Rated::new(Probe::new(0.0), 96_000, 44_100, N, ch.0, ch.1).unwrap();
    let mut seen = Vec::new();
    let mut s = first;
    while s < 20_000 {
        if Some(s) == restart.map(|r| r - 7 * N as u64) {
            r.discontinuity();
            s = restart.unwrap_or(s);
        }
        let input = vec![0.0; ch.0 * N];
        let mut out = vec![0.0; ch.1 * N];
        let calls = r.inner.starts.len();
        r.process(&mut Block::new(N, &input, &mut out).at(s));
        for (start, frames) in r.inner.starts[calls..].iter().zip(&r.inner.frames[calls..]) {
            seen.push((s, *start, *frames));
        }
        s += N as u64;
    }
    seen
}

/// Every app block a restarted `Rated::new` gives carries the media start
/// and frames that a `Rated` running from device frame 0 gives at the same
/// device block, with no RX channel or no TX channel too. (A fixed app
/// block's calls fall where its RX FIFO has `A` frames, which a restart
/// moves; its round trip is checked above.)
#[test]
fn a_restarted_app_block_is_labelled_as_one_that_ran_from_the_start() {
    for ch in [(1, 1), (0, 1), (1, 0)] {
        let whole = labels(ch, 0, None);
        for (first, restart) in [(896, None), (FIRST, Some(10_112)), (128, Some(9_984))] {
            let got = labels(ch, first, restart);
            // From the first block on, after each start, every block's
            // label is the whole run's.
            let from = restart.unwrap_or(first);
            let tail: Vec<_> = got.iter().filter(|b| b.0 >= from).collect();
            assert!(tail.len() > 40, "{ch:?} {first}");
            for b in tail {
                assert!(
                    whole.contains(b),
                    "{ch:?}, from {first}, restart {restart:?}: {b:?}"
                );
            }
        }
    }
}
