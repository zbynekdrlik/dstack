use super::*;
use std::f64::consts::PI;

/// Records each block's frames, start and input 0, and sends `level` on
/// every output.
struct Probe {
    frames: Vec<usize>,
    starts: Vec<Option<u64>>,
    heard: Vec<f64>,
    level: f64,
    resets: u32,
}

impl Probe {
    fn new(level: f64) -> Self {
        Probe {
            frames: Vec::new(),
            starts: Vec::new(),
            heard: Vec::new(),
            level,
            resets: 0,
        }
    }
}

impl Process for Probe {
    fn process(&mut self, block: &mut Block<'_>) {
        self.frames.push(block.frames());
        self.starts.push(block.start());
        self.heard.extend_from_slice(block.input(0));
        for c in 0..block.outputs() {
            block.output(c).fill(self.level);
        }
    }

    fn discontinuity(&mut self) {
        self.resets += 1;
    }
}

/// Runs `blocks` device blocks of `n` frames (input: `f(i)` on every
/// channel, `i` the device sample) from media time `start`; returns the
/// device output of channel 0.
fn run(
    r: &mut Rated<Probe>,
    ch: (usize, usize),
    n: usize,
    blocks: usize,
    start: u64,
    f: impl Fn(u64) -> f64,
) -> Vec<f64> {
    let mut sent = Vec::new();
    for b in 0..blocks {
        let at = start + (b * n) as u64;
        let input: Vec<f64> = (0..ch.0)
            .flat_map(|_| (0..n).map(|i| f(at + i as u64)))
            .collect();
        let mut out = vec![0.0; ch.1 * n];
        r.process(&mut Block::new(n, &input, &mut out).at(at));
        sent.extend_from_slice(&out[..n.min(out.len())]);
    }
    sent
}

#[test]
fn the_app_gets_its_rate_s_share_of_frames_in_follow_on_time() {
    for (dev, app) in [(96_000u32, 48_000u32), (96_000, 44_100), (48_000, 96_000)] {
        let mut r = Rated::new(Probe::new(0.0), dev, app, 32, 1, 1).unwrap();
        run(&mut r, (1, 1), 32, 4_000, 1_000, |_| 0.0);
        let frames = &r.inner.frames;
        // Once the filter is full, the app's frames add up to the ratio.
        let steady: usize = frames[1_000..].iter().sum();
        let want = 32.0 * 3_000.0 * f64::from(app) / f64::from(dev);
        assert!(
            (steady as f64 - want).abs() <= 1.0,
            "{dev} -> {app}: {steady} of {want}"
        );
        // Each block starts where the last one ended.
        for w in r.inner.starts.windows(2).zip(frames.windows(2)).skip(1) {
            let (s, f) = w;
            assert_eq!(s[1].unwrap(), s[0].unwrap() + f[0] as u64, "{dev} -> {app}");
        }
        // The first start is the device's at the app's rate.
        let first = (1_000.0 * f64::from(app) / f64::from(dev)) as u64;
        assert_eq!(r.inner.starts[0], Some(first));
    }
}

#[test]
fn a_device_block_that_does_not_follow_on_restarts_the_app_s_time() {
    let mut r = Rated::new(Probe::new(0.0), 96_000, 48_000, 32, 1, 1).unwrap();
    run(&mut r, (1, 1), 32, 10, 0, |_| 0.0);
    run(&mut r, (1, 1), 32, 1, 96_000, |_| 0.0);
    assert_eq!(r.inner.starts.last(), Some(&Some(48_000)));
    // A block with no media time stays without one.
    let (i, mut o) = ([0.0; 32], [0.0; 32]);
    r.process(&mut Block::new(32, &i, &mut o));
    assert_eq!(r.inner.starts.last(), Some(&None));
}

#[test]
fn a_tone_comes_in_at_the_app_s_rate_and_the_app_s_level_goes_out() {
    for app in [48_000u32, 44_100] {
        let mut r = Rated::new(Probe::new(0.5), 96_000, app, 32, 1, 1).unwrap();
        let sent = run(&mut r, (1, 1), 32, 3_000, 0, |i| {
            (2.0 * PI * 1_000.0 * i as f64 / 96_000.0).sin() * 0.5
        });
        // The app heard a 0.5 peak tone (1 kHz is far inside the band).
        let heard = &r.inner.heard[2_000..];
        let peak = heard.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        assert!((peak - 0.5).abs() < 1e-3, "{app}: heard peak {peak}");
        // The app's constant 0.5 left the device at 0.5, once settled.
        assert!(
            sent[2_000..].iter().all(|v| (v - 0.5).abs() < 1e-6),
            "{app}"
        );
    }
}

#[test]
fn the_tx_fifo_never_runs_dry_at_any_ratio() {
    for (dev, app) in [
        (96_000u32, 48_000u32),
        (96_000, 44_100),
        (48_000, 44_100),
        (44_100, 48_000),
    ] {
        for n in [16usize, 32, 256] {
            let mut r = Rated::new(Probe::new(0.0), dev, app, n, 2, 2).unwrap();
            let dry = r.dry();
            run(&mut r, (2, 2), n, 20_000 / n * 10, 0, |_| 0.0);
            assert_eq!(dry.load(Ordering::Relaxed), 0, "{dev} -> {app}, block {n}");
        }
    }
}

#[test]
fn a_dry_fifo_plays_silence_and_is_counted() {
    let mut r = Rated::new(Probe::new(0.5), 96_000, 48_000, 32, 1, 1).unwrap();
    r.fifo[0].clear();
    let dry = r.dry();
    // An app that hands back nothing: an RX side with no frames yet is
    // the same as the FIFO cleared.
    let (i, mut o) = ([0.0; 32], [1.0; 32]);
    r.process(&mut Block::new(32, &i, &mut o).at(0));
    assert!(dry.load(Ordering::Relaxed) > 0);
    assert!(o.iter().all(|v| v.is_finite()));
}

#[test]
fn every_channel_has_its_own_filters() {
    let mut r = Rated::new(Probe::new(0.0), 96_000, 48_000, 32, 2, 2).unwrap();
    for _ in 0..100 {
        let mut input = vec![0.0; 64];
        input[32..].fill(1.0);
        let mut out = vec![0.0; 64];
        r.process(&mut Block::new(32, &input, &mut out));
    }
    // Channel 0 (silent) stays silent in the app.
    assert!(r.inner.heard.iter().all(|v| v.abs() < 1e-12));
}

#[test]
fn an_app_with_no_rx_or_no_tx_still_runs_at_its_rate() {
    let mut r = Rated::new(Probe::new(0.25), 96_000, 48_000, 32, 0, 1).unwrap();
    let sent = run(&mut r, (0, 1), 32, 400, 0, |_| 0.0);
    assert!(r.inner.frames[100..].iter().all(|&k| k == 16));
    assert!((sent[sent.len() - 1] - 0.25).abs() < 1e-6);
    let mut r = Rated::new(Probe::new(0.0), 96_000, 48_000, 32, 1, 0).unwrap();
    run(&mut r, (1, 0), 32, 400, 0, |_| 0.0);
    assert!(r.inner.frames[100..].iter().all(|&k| k == 16));
}

#[test]
fn a_block_of_another_size_reaches_no_app_and_sends_silence() {
    let mut r = Rated::new(Probe::new(1.0), 96_000, 48_000, 32, 1, 1).unwrap();
    let (i, mut o) = ([0.0; 64], [1.0; 64]);
    r.process(&mut Block::new(64, &i, &mut o));
    assert!(o.iter().all(|&v| v == 0.0));
    assert!(r.inner.frames.is_empty());
}

#[test]
fn a_discontinuity_clears_the_filters_and_reaches_the_app() {
    let mut r = Rated::new(Probe::new(0.0), 96_000, 48_000, 32, 1, 1).unwrap();
    run(&mut r, (1, 1), 32, 100, 0, |_| 1.0);
    r.discontinuity();
    assert_eq!(r.inner.resets, 1);
    assert_eq!(r.fifo[0].len(), PRIME);
    assert!(r.last.is_none());
    let before = r.inner.heard.len();
    run(&mut r, (1, 1), 32, 1, 0, |_| 0.0);
    assert!(r.inner.heard[before..].iter().all(|v| v.abs() < 1e-12));
}

#[test]
fn a_rate_the_converter_refuses_is_an_error_at_the_start() {
    assert!(Rated::new(Probe::new(0.0), 96_000, 0, 32, 1, 1).is_err());
}
