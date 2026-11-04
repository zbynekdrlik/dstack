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
        (48_000, 96_000),
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
    // A ratio that does not divide the block (the sinc, with its PRIME): the
    // half-band's 2:1 gives TX exactly what it takes and never runs dry.
    let mut r = Rated::new(Probe::new(0.5), 96_000, 44_100, 32, 1, 1).unwrap();
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
    // The FIFO starts again with its own prime: none for the half-band's
    // 2:1, PRIME for a ratio that does not divide the block.
    for (app, prime) in [(48_000u32, 0), (44_100, PRIME)] {
        let mut r = Rated::new(Probe::new(0.0), 96_000, app, 32, 1, 1).unwrap();
        assert_eq!(r.fifo[0].len(), prime, "{app}");
        run(&mut r, (1, 1), 32, 100, 0, |_| 1.0);
        r.fifo[0].push_back(1.0);
        r.discontinuity();
        assert_eq!(r.inner.resets, 1);
        assert_eq!(r.fifo[0].len(), prime, "{app}");
        assert!(r.fifo[0].iter().all(|&v| v == 0.0));
        assert!(r.last.is_none());
        let before = r.inner.heard.len();
        run(&mut r, (1, 1), 32, 1, 0, |_| 0.0);
        assert!(r.inner.heard[before..].iter().all(|v| v.abs() < 1e-12));
    }
}

#[test]
fn a_rate_the_converter_refuses_is_an_error_at_the_start() {
    assert!(Rated::new(Probe::new(0.0), 96_000, 0, 32, 1, 1).is_err());
}

#[test]
fn a_fifo_drain_takes_what_it_has_and_fills_the_rest_with_silence() {
    let mut fifo: VecDeque<f64> = [1.0, 2.0, 3.0].into_iter().collect();
    let mut dst = [9.0; 5];
    assert_eq!(drain_into(&mut fifo, &mut dst), 2);
    assert_eq!(dst, [1.0, 2.0, 3.0, 0.0, 0.0]);
    assert!(fifo.is_empty());
    let mut fifo: VecDeque<f64> = [1.0, 2.0, 3.0, 4.0, 5.0].into_iter().collect();
    let mut dst = [9.0; 3];
    assert_eq!(drain_into(&mut fifo, &mut dst), 0);
    assert_eq!(dst, [1.0, 2.0, 3.0]);
    assert_eq!(fifo.into_iter().collect::<Vec<_>>(), vec![4.0, 5.0]);
}

/// Sends `levels[c]` on output `c` and records input `c`'s last sample.
struct Levels {
    levels: Vec<f64>,
    last_in: Vec<f64>,
}

impl Process for Levels {
    fn process(&mut self, block: &mut Block<'_>) {
        for (c, last) in self.last_in.iter_mut().enumerate() {
            if let Some(&v) = block.input(c).last() {
                *last = v;
            }
        }
        for (c, &l) in self.levels.iter().enumerate() {
            block.output(c).fill(l);
        }
    }
}

#[test]
fn each_channel_keeps_its_own_level_both_ways_at_a_ratio_that_does_not_divide() {
    for app in [44_100u32, 48_000] {
        let probe = Levels {
            levels: vec![0.25, 0.75, -0.5],
            last_in: vec![0.0; 3],
        };
        let mut r = Rated::new(probe, 96_000, app, 32, 3, 3).unwrap();
        let mut out = vec![0.0; 3 * 32];
        // Input channel c carries 0.1 * (c + 1).
        let input: Vec<f64> = (0..3)
            .flat_map(|c| std::iter::repeat_n(0.1 * (c as f64 + 1.0), 32))
            .collect();
        for b in 0..2_000u64 {
            r.process(&mut Block::new(32, &input, &mut out).at(b * 32));
        }
        for (c, want) in [0.25, 0.75, -0.5].into_iter().enumerate() {
            for v in &out[c * 32..(c + 1) * 32] {
                assert!((v - want).abs() < 1e-6, "{app}: out {c} = {v}");
            }
            let heard = r.inner.last_in[c];
            assert!(
                (heard - 0.1 * (c as f64 + 1.0)).abs() < 1e-6,
                "{app}: in {c} = {heard}"
            );
        }
    }
}

/// dstack #90 (obs, 3.11.2026): an impulse into the device reaches the app
/// after the RX delay, and a step from the app reaches the device after
/// the TX delay; both as [`Rated::delay_ns`] says, within a frame.
#[test]
fn an_impulse_and_a_step_take_the_delay_each_way_says() {
    for (app, rx_ms, tx_ms) in [(48_000u32, 0.63, 0.64), (44_100, 1.32, 3.08)] {
        let mut r = Rated::new(Probe::new(0.5), 96_000, app, 32, 1, 1).unwrap();
        let (rx_ns, tx_ns) = r.delay_ns();
        assert!(
            (rx_ns as f64 / 1e6 - rx_ms).abs() < 0.01,
            "{app}: rx {rx_ns}"
        );
        assert!(
            (tx_ns as f64 / 1e6 - tx_ms).abs() < 0.01,
            "{app}: tx {tx_ns}"
        );
        let at = 3_200u64;
        let sent = run(&mut r, (1, 1), 32, 200, 0, |i| f64::from(u8::from(i == at)));
        // RX: the app's loudest frame, after the impulse's own app frame.
        let heard = &r.inner.heard;
        let peak = (0..heard.len())
            .max_by(|&a, &b| heard[a].abs().total_cmp(&heard[b].abs()))
            .unwrap();
        let rx_frames = peak as f64 - at as f64 * f64::from(app) / 96_000.0;
        let want = rx_ns as f64 * f64::from(app) / 1e9;
        assert!(
            (rx_frames - want).abs() <= 1.0,
            "{app}: rx {rx_frames} of {want}"
        );
        // TX: the app sends 0.5 from its first frame; the device's output
        // passes half of it at the TX delay.
        let half = sent.iter().position(|v| *v >= 0.25).unwrap() as f64;
        let want = tx_ns as f64 * 96_000.0 / 1e9;
        assert!((half - want).abs() <= 2.0, "{app}: tx {half} of {want}");
    }
}

/// dstack #68: `delay_ns` (what `dlink::split` budgets) is what a `Rated`
/// for the same rates and block says, for every block and rate pair; one
/// rate needs no `Rated` and delays nothing.
#[test]
fn the_delay_before_the_start_is_the_running_rated_s() {
    for block in dstack::stream::BLOCKS {
        let b = block as usize;
        for (dev, app) in [
            (96_000u32, 48_000u32),
            (96_000, 44_100),
            (48_000, 96_000),
            (48_000, 44_100),
        ] {
            let r = Rated::new(Probe::new(0.5), dev, app, b, 2, 2).unwrap();
            assert_eq!(
                delay_ns(dev, app, b).unwrap(),
                r.delay_ns(),
                "{dev} {app} {b}"
            );
        }
        assert_eq!(delay_ns(96_000, 96_000, b).unwrap(), (0, 0));
    }
    // 48 kHz on 96 kHz: RX 0.63 ms, TX 0.64 ms (the module's table: the
    // half-band, dstack #1; the sinc took 1.33 and 2.83 ms), exactly 30
    // frames at 48 kHz and 61 at 96 kHz with no FIFO prime; 96 kHz on
    // 48 kHz the other way round.
    assert_eq!(delay_ns(96_000, 48_000, 128).unwrap(), (625_000, 635_416));
    assert_eq!(delay_ns(48_000, 96_000, 128).unwrap(), (635_416, 625_000));
    // 44.1 kHz stays on the sinc, with the FIFO's PRIME in its TX delay.
    let (rx, tx) = delay_ns(96_000, 44_100, 128).unwrap();
    assert_eq!((rx / 10_000, tx / 10_000), (131, 307));
    assert!(delay_ns(96_000, 0, 128).is_err());
}
