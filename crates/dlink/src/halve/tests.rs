use super::*;

const NET: f64 = 96_000.0;

/// A sine of `hz` at `rate`, from sample `from`, `n` samples.
fn sine(hz: f64, rate: f64, from: usize, n: usize) -> Vec<f64> {
    (from..from + n)
        .map(|i| (2.0 * std::f64::consts::PI * hz * i as f64 / rate).sin())
        .collect()
}

/// The largest magnitude in `x`.
fn peak(x: &[f64]) -> f64 {
    x.iter().fold(0.0, |m, v| m.max(v.abs()))
}

/// Records each block it gets on input 0, and sends `out` on output 0.
struct Probe {
    heard: Vec<f64>,
    frames: Vec<usize>,
    starts: Vec<Option<u64>>,
    out: Vec<f64>,
    resets: u32,
}

impl Probe {
    fn new(out: Vec<f64>) -> Self {
        Probe {
            heard: Vec::new(),
            frames: Vec::new(),
            starts: Vec::new(),
            out,
            resets: 0,
        }
    }
}

impl Process for Probe {
    fn process(&mut self, block: &mut Block<'_>) {
        let m = block.frames();
        self.frames.push(m);
        self.starts.push(block.start());
        self.heard.extend_from_slice(block.input(0));
        let at = self.heard.len() - m;
        let src = &self.out[at..at + m];
        block.output(0).copy_from_slice(src);
    }

    fn discontinuity(&mut self) {
        self.resets += 1;
    }
}

/// Runs `input` (network rate) through a Halve over `inner` in blocks of
/// `b` frames from media time `start`; returns the network-rate output.
fn run(halve: &mut Halve<Probe>, input: &[f64], b: usize, start: u64) -> Vec<f64> {
    let mut out = vec![0.0; input.len()];
    for (k, (i, o)) in input.chunks(b).zip(out.chunks_mut(b)).enumerate() {
        let mut block = Block::new(b, i, o).at(start + (k * b) as u64);
        halve.process(&mut block);
    }
    out
}

#[test]
fn the_filter_is_symmetric_and_sums_to_one() {
    let h = half_band();
    assert!((h.iter().sum::<f64>() - 1.0).abs() < 1e-12);
    for n in 0..TAPS {
        assert!((h[n] - h[TAPS - 1 - n]).abs() < 1e-15);
    }
    // A half-band: every second tap from the centre is zero.
    let c = (TAPS - 1) / 2;
    assert!((h[c] - 0.5).abs() < 0.01, "{}", h[c]);
    assert!(h[c + 2].abs() < 1e-12 && h[c + 4].abs() < 1e-12);
}

#[test]
fn rx_keeps_the_audio_band_and_holds_down_what_would_fold() {
    let n = 96 * 32 * 8; // 8 x 96 blocks of 32
    let skip = TAPS; // the filter's start
    for (hz, keep) in [
        (1_000.0, true),
        (10_000.0, true),
        (19_000.0, true),
        (30_000.0, false),
        (40_000.0, false),
    ] {
        let mut h = Halve::new(Probe::new(vec![0.0; n]), 1, 1);
        run(&mut h, &sine(hz, NET, 0, n), 32, 0);
        let got = peak(&h.inner.heard[skip..]);
        let db = 20.0 * got.log10();
        if keep {
            assert!(db.abs() < 0.1, "{hz} Hz: {db} dB");
        } else {
            assert!(db < -STOP_DB, "{hz} Hz folds in at {db} dB");
        }
    }
}

#[test]
fn rx_hands_on_every_second_sample_of_a_slow_signal_in_time() {
    // A 1 kHz sine at 96 kHz becomes the same sine at 48 kHz, delayed by
    // the filter's (TAPS - 1) / 2 network samples.
    let n = 32 * 60;
    let mut h = Halve::new(Probe::new(vec![0.0; n]), 1, 1);
    run(&mut h, &sine(1_000.0, NET, 0, n), 32, 0);
    let delay = (TAPS - 1) / 2;
    for k in 100..200 {
        let want = (2.0 * std::f64::consts::PI * 1_000.0 * (2 * k + 1 - delay) as f64 / NET).sin();
        assert!((h.inner.heard[k] - want).abs() < 1e-3, "sample {k}");
    }
}

#[test]
fn tx_keeps_the_audio_band_and_holds_down_its_image() {
    let n = 32 * 96 * 4;
    for (hz, image) in [(1_000.0, 47_000.0), (15_000.0, 33_000.0)] {
        let app = sine(hz, NET / 2.0, 0, n / 2);
        let mut h = Halve::new(Probe::new(app), 1, 1);
        let out = run(&mut h, &vec![0.0; n], 32, 0);
        let tail = &out[2 * TAPS..];
        let db = 20.0 * peak(tail).log10();
        assert!(db.abs() < 0.1, "{hz} Hz out at {db} dB");
        // The image: correlate with a sine and a cosine at its frequency.
        let (mut s, mut c) = (0.0, 0.0);
        for (i, v) in tail.iter().enumerate() {
            let w = 2.0 * std::f64::consts::PI * image * i as f64 / NET;
            s += v * w.sin();
            c += v * w.cos();
        }
        let level = 2.0 * (s * s + c * c).sqrt() / tail.len() as f64;
        assert!(
            20.0 * level.log10() < -STOP_DB,
            "{hz} Hz image at {}",
            20.0 * level.log10()
        );
    }
}

#[test]
fn the_app_gets_half_the_frames_and_half_the_media_time() {
    let mut h = Halve::new(Probe::new(vec![0.0; 1024]), 1, 1);
    run(&mut h, &[0.0; 256], 64, 1_000);
    assert_eq!(h.inner.frames, vec![32; 4]);
    assert_eq!(
        h.inner.starts,
        vec![Some(500), Some(532), Some(564), Some(596)]
    );
    // A block with no media time stays without one.
    let (i, mut o) = ([0.0; 32], [0.0; 32]);
    h.process(&mut Block::new(32, &i, &mut o));
    assert_eq!(h.inner.starts.last(), Some(&None));
}

#[test]
fn an_odd_or_oversized_block_reaches_no_app_and_sends_silence() {
    let mut h = Halve::new(Probe::new(vec![1.0; 1024]), 1, 1);
    for n in [31, MAX_FRAMES + 2] {
        let i = vec![0.0; n];
        let mut o = vec![1.0; n];
        h.process(&mut Block::new(n, &i, &mut o));
        assert!(o.iter().all(|&v| v == 0.0), "{n}");
    }
    assert!(h.inner.frames.is_empty());
}

#[test]
fn a_discontinuity_clears_the_filters_and_reaches_the_app() {
    let n = 32 * 8;
    let mut h = Halve::new(Probe::new(vec![0.0; 4 * n]), 1, 1);
    run(&mut h, &vec![1.0; n], 32, 0);
    h.discontinuity();
    assert_eq!(h.inner.resets, 1);
    // After the reset, silence in gives silence out at once (no history).
    let before = h.inner.heard.len();
    run(&mut h, &vec![0.0; 32], 32, 0);
    assert!(h.inner.heard[before..].iter().all(|&v| v == 0.0));
}

#[test]
fn every_channel_has_its_own_filter() {
    let n = 32 * 40;
    let mut h = Halve::new(Probe::new(vec![0.0; n]), 2, 2);
    let mut input = vec![0.0; 2 * 32];
    let mut out = vec![0.0; 2 * 32];
    // Channel 1 carries a constant; channel 0 silence: channel 0's app
    // input stays silent.
    for _ in 0..40 {
        input[32..].fill(1.0);
        h.process(&mut Block::new(32, &input, &mut out));
    }
    assert!(h.inner.heard.iter().all(|&v| v == 0.0));
    assert!((h.app_in[16 + 15] - 1.0).abs() < 1e-9);
}
