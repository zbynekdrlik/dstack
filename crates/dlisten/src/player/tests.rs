// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

use std::f64::consts::TAU;

/// The network and the output as two clocks (the issue's test plan): the
/// pump pushes blocks of 128 frames at the network's rate on true time;
/// the output takes `callback` frames at its own rate, `drift_ppm` fast on
/// true time (its crystal). The signal is a sine of `freq` Hz at `amp` on
/// every channel (channel `c` phase-shifted by `c`, to tell them apart).
struct Sim {
    fifo: Arc<Fifo>,
    player: Player,
    net_rate: f64,
    out_rate: f64,
    drift_ppm: f64,
    callback: usize,
    outputs: usize,
    freq: f64,
    amp: f64,
    /// True time, s.
    t: f64,
    /// Frames the pump pushed.
    pushed: u64,
    /// The pump runs (false: no Dante data).
    pumping: bool,
    /// The last callback's output, interleaved.
    out: Vec<f64>,
    max_depth: usize,
    /// The depth summed before and after each callback (the sawtooth's
    /// top and bottom), frames, and the count.
    depth_sum: f64,
    depth_n: f64,
}

const PUMP_BLOCK: usize = 128;

impl Sim {
    fn new(net_rate: u32, out_rate: u32, drift_ppm: f64, channels: usize, outputs: usize) -> Self {
        let fifo = Arc::new(fifo_for(channels, net_rate));
        let player = Player::new(Arc::clone(&fifo), net_rate, out_rate, Arc::default()).unwrap();
        // A WASAPI shared callback: 10 ms.
        let callback = out_rate as usize / 100;
        Sim {
            fifo,
            player,
            net_rate: f64::from(net_rate),
            out_rate: f64::from(out_rate),
            drift_ppm,
            callback,
            outputs,
            freq: 1000.0,
            amp: 0.5,
            t: 0.0,
            pushed: 0,
            pumping: true,
            out: vec![0.0; callback * outputs],
            max_depth: 0,
            depth_sum: 0.0,
            depth_n: 0.0,
        }
    }

    /// One output callback: the pump catches up to true time, then the
    /// output takes its frames.
    fn step(&mut self) {
        self.t += self.callback as f64 / (self.out_rate * (1.0 + self.drift_ppm * 1e-6));
        let due = (self.t * self.net_rate) as u64;
        while self.pumping && self.pushed + PUMP_BLOCK as u64 <= due {
            let (base, rate, f, a) = (self.pushed, self.net_rate, self.freq, self.amp);
            self.fifo.push_with(PUMP_BLOCK, |i, c| {
                a * (TAU * f * (base + i as u64) as f64 / rate + c as f64).sin()
            });
            self.pushed += PUMP_BLOCK as u64;
        }
        if !self.pumping {
            // A stopped pump: its clock still runs, no frames come.
            self.pushed = due;
        }
        self.max_depth = self.max_depth.max(self.fifo.len());
        let before = self.fifo.len();
        self.player.fill_with(&mut self.out, self.outputs, |x| x);
        self.depth_sum += (before + self.fifo.len()) as f64;
        self.depth_n += 2.0;
    }

    fn run(&mut self, seconds: f64) {
        let end = self.t + seconds;
        while self.t < end {
            self.step();
        }
    }

    /// Runs `seconds` and returns channel `ch` of the output.
    fn record(&mut self, seconds: f64, ch: usize) -> Vec<f64> {
        let mut got = Vec::new();
        let end = self.t + seconds;
        while self.t < end {
            self.step();
            got.extend(self.out.iter().skip(ch).step_by(self.outputs));
        }
        got
    }

    /// The mean depth since the last call, ms.
    fn depth_ms(&mut self) -> f64 {
        let mean = self.depth_sum / self.depth_n / self.net_rate * 1000.0;
        (self.depth_sum, self.depth_n) = (0.0, 0.0);
        mean
    }
}

fn rms(x: &[f64]) -> f64 {
    (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt()
}

/// A tone's frequency from its rising zero crossings (linear
/// interpolation between samples), Hz.
fn frequency(x: &[f64], rate: f64) -> f64 {
    let ups: Vec<f64> = x
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w[0] < 0.0 && w[1] >= 0.0)
        .map(|(i, w)| i as f64 + w[0] / (w[0] - w[1]))
        .collect();
    let (first, last) = (ups[0], ups[ups.len() - 1]);
    (ups.len() - 1) as f64 * rate / (last - first)
}

/// (a) An output crystal ±200 ppm off the network's at equal rates over
/// four simulated minutes: the depth settles at the target, the controller
/// follows the drift (an output fast by +d takes more frames: +d ppm, more
/// output per input), no underrun after the priming, no overflow.
#[test]
fn an_output_200_ppm_fast_keeps_the_fifo_at_its_target_with_no_underrun() {
    drifting(200.0);
}

#[test]
fn an_output_200_ppm_slow_keeps_the_fifo_at_its_target_with_no_underrun() {
    drifting(-200.0);
}

fn drifting(drift: f64) {
    {
        // One channel: the clocks are the test, the converter costs per channel.
        let mut s = Sim::new(48_000, 48_000, drift, 1, 1);
        s.run(210.0);
        s.depth_ms();
        s.run(30.0);
        let depth = s.depth_ms();
        let stats = s.player.stats();
        println!(
            "drift {drift} ppm: mean depth {depth:.2} ms (last 30 s), ppm {:.2}, underruns {}, \
             recentres {}, max depth {:.1} ms, overflows {}",
            stats.ppm(),
            stats.underruns(),
            stats.recentres(),
            s.max_depth as f64 / 48.0,
            s.fifo.overflows()
        );
        assert_eq!(stats.underruns(), 0, "{drift}: no underrun after priming");
        assert_eq!(stats.recentres(), 0, "{drift}");
        assert_eq!(s.fifo.overflows(), 0, "{drift}: no overflow");
        assert!(!stats.priming(), "{drift}: playing");
        // The mean over the callback's sawtooth reads a little over the
        // controller's own reads (after each block); near, not exact.
        assert!(
            (depth - TARGET_S * 1000.0).abs() < 2.5,
            "{drift}: depth {depth} ms"
        );
        assert!(
            (s.player.ctl.rate_ppm() - drift).abs() < 1.0,
            "{drift}: rate fit {} ppm",
            s.player.ctl.rate_ppm()
        );
        assert!(
            (stats.ppm() - drift).abs() < 3.0,
            "{drift}: correction {} ppm",
            stats.ppm()
        );
        // The driver got every frame it asked for, in whole callbacks.
        assert_eq!(stats.played() % s.callback as u64, 0);
        assert!(stats.played() as f64 >= 239.0 * 48_000.0);
    }
}

/// (b) The output at another rate than the network: after the settling,
/// the network's frames are taken at the network's rate per output second,
/// and a 1 kHz sine comes out at 1 kHz at its level (within 0.5 dB).
fn converts(net_rate: u32, out_rate: u32) {
    let mut s = Sim::new(net_rate, out_rate, 0.0, 2, 2);
    s.run(30.0);
    let (consumed, played) = (s.player.consumed(), s.player.stats().played());
    let tone = s.record(10.0, 0);
    let stats = s.player.stats();
    let per_s = (s.player.consumed() - consumed) as f64
        / ((stats.played() - played) as f64 / f64::from(out_rate));
    let freq = frequency(&tone, f64::from(out_rate));
    let level_db = 20.0 * (rms(&tone) / (s.amp / 2f64.sqrt())).log10();
    println!(
        "{net_rate} -> {out_rate}: {per_s:.2} network frames per output second, \
         {freq:.4} Hz, level {level_db:+.4} dB, depth {:.2} ms, underruns {}",
        s.depth_ms(),
        stats.underruns()
    );
    assert_eq!(stats.underruns(), 0);
    assert_eq!(s.fifo.overflows(), 0);
    assert!(
        (per_s / f64::from(net_rate) - 1.0).abs() < 100e-6,
        "{net_rate} -> {out_rate}: {per_s} frames a second"
    );
    assert!(
        (freq - s.freq).abs() < 0.1,
        "{net_rate} -> {out_rate}: {freq} Hz"
    );
    assert!(
        level_db.abs() < 0.5,
        "{net_rate} -> {out_rate}: {level_db} dB"
    );
    // R is its own channel (the sine one radian later), not L again.
    let r = s.record(0.1, 1);
    let l_now: Vec<f64> = s.out.iter().step_by(2).copied().collect();
    let r_now: Vec<f64> = s.out.iter().skip(1).step_by(2).copied().collect();
    assert!(rms(&r) > 0.3, "R plays");
    assert!(
        l_now.iter().zip(&r_now).any(|(l, r)| (l - r).abs() > 0.1),
        "R differs from L"
    );
}

#[test]
fn a_48_khz_network_plays_on_a_44_1_khz_output_at_its_rate_frequency_and_level() {
    converts(48_000, 44_100);
}

#[test]
fn a_96_khz_network_plays_on_a_48_khz_output_at_its_rate_frequency_and_level() {
    converts(96_000, 48_000);
}

/// (c) The pump stops (no Dante data): the output plays silence and counts
/// underruns, without a panic; when the data comes back the tone does too,
/// with no underrun after the new priming.
#[test]
fn a_stopped_pump_plays_silence_counts_underruns_and_recovers() {
    let mut s = Sim::new(48_000, 48_000, 0.0, 2, 2);
    s.run(5.0);
    assert_eq!(s.player.stats().underruns(), 0);
    s.pumping = false;
    // The FIFO's 40 ms drain, then silence.
    s.run(0.2);
    let quiet = s.record(2.0, 0);
    let stats = s.player.stats();
    assert!(quiet.iter().all(|&x| x == 0.0), "silence while no data");
    assert_eq!(stats.underruns(), 1, "one dropout, one count");
    assert!(stats.priming(), "waiting for data");
    let played = stats.played();
    assert!(played > 0);
    s.pumping = true;
    s.run(1.0);
    let after = s.player.stats().underruns();
    let tone = s.record(3.0, 0);
    let stats = s.player.stats();
    assert!(!stats.priming(), "playing again");
    assert_eq!(stats.underruns(), after, "no underrun once primed again");
    assert_eq!(after, 1);
    let level_db = 20.0 * (rms(&tone[tone.len() / 2..]) / (s.amp / 2f64.sqrt())).log10();
    assert!(level_db.abs() < 0.5, "the tone is back: {level_db} dB");
    assert!(stats.played() > played);
}

/// Each underrun primes 10 ms more than the last, up to 40 ms over the
/// target, and the controller keeps that excess as its cushion.
#[test]
fn each_underrun_primes_a_larger_cushion_up_to_its_cap() {
    let mut s = Sim::new(48_000, 48_000, 0.0, 1, 1);
    let mut levels = Vec::new();
    for _ in 0..6 {
        s.run(1.0);
        s.pumping = false;
        s.run(0.2);
        let State::Priming(level) = s.player.state else {
            panic!("primes after the underrun")
        };
        levels.push(level);
        assert!(
            (s.player.ctl.cushion_s() - s.player.extra_s()).abs() < 1e-12,
            "cushion {} for extra {}",
            s.player.ctl.cushion_s(),
            s.player.extra_s()
        );
        s.pumping = true;
    }
    // 48 frames a ms: 40 ms + 10, 20, 30, 40, then the cap.
    assert_eq!(levels, [2400, 2880, 3360, 3840, 3840, 3840]);
    assert_eq!(s.player.stats().underruns(), 6);
    // Once playing, the cushion holds the depth over the target.
    s.run(0.5);
    s.depth_ms();
    s.run(2.0);
    let depth = s.depth_ms();
    assert!(depth > 60.0, "the cushion is kept: {depth} ms");
}

/// Audio that came before the output ran is skipped at the start, not
/// drained over minutes: playing starts at the target.
#[test]
fn an_excess_at_the_start_is_skipped_to_the_target() {
    let fifo = Arc::new(fifo_for(1, 48_000));
    fifo.push_with(9600, |_, _| 0.25); // 200 ms
    let mut player = Player::new(Arc::clone(&fifo), 48_000, 48_000, Arc::default()).unwrap();
    let mut out = [0.0f32; 1];
    player.fill_with(&mut out, 1, |x| x as f32);
    assert!(!player.stats().priming());
    // 40 ms and half the callback and the block (the reads' sawtooth),
    // less the first block's input.
    let need = player.consumed() as usize;
    assert!(need > 0);
    assert_eq!(fifo.len(), 1920 + need.div_ceil(2) - need);
}

/// A sudden excess past the hard bound (a burst of late blocks) is
/// skipped back toward the target, counted as a re-centre.
#[test]
fn a_burst_past_the_excess_bound_is_skipped_and_counted() {
    let mut s = Sim::new(48_000, 48_000, 0.0, 1, 1);
    s.run(5.0);
    s.fifo.push_with(4800, |_, _| 0.0); // 100 ms at once
    s.run(0.05);
    let stats = s.player.stats();
    assert_eq!(stats.recentres(), 1);
    assert_eq!(stats.underruns(), 0);
    s.depth_ms();
    s.run(1.0);
    let depth = s.depth_ms();
    assert!(
        (depth - 40.0).abs() < 6.0,
        "back near the target: {depth} ms"
    );
}

/// A depth under the floor with data still coming re-primes, counted as a
/// re-centre and as an underrun (the audio breaks for the refill).
#[test]
fn a_depth_under_the_floor_primes_again() {
    let mut s = Sim::new(48_000, 48_000, 0.0, 1, 1);
    s.run(5.0);
    // Leave 3 ms: under the 5 ms floor, over a block's input.
    let held = s.fifo.len();
    s.fifo.skip(held - 144);
    let mut out = vec![0.0; BLOCK];
    s.player.fill_with(&mut out, 1, |x| x);
    let stats = s.player.stats();
    assert_eq!(stats.recentres(), 1);
    assert_eq!(stats.underruns(), 1, "a refill is a dropout");
    assert_eq!(s.player.state, State::Priming(2400), "with a 10 ms cushion");
    assert!(stats.priming());
}

/// The player's mapping: a mono source on both speakers, stereo mixed on a
/// mono output, each sample through the caller's conversion.
#[test]
fn the_player_maps_our_channels_onto_the_output() {
    let mut mono = Sim::new(48_000, 48_000, 0.0, 1, 2);
    mono.run(2.0);
    assert!(rms(&mono.out) > 0.3);
    for f in mono.out.as_chunks::<2>().0 {
        assert_eq!(f[0], f[1], "mono on both");
    }
    let mut down = Sim::new(48_000, 48_000, 0.0, 2, 1);
    down.run(2.0);
    // L and R one radian apart, at half each: cos(0.5) of the level.
    let want = down.amp * (0.5f64).cos() / 2f64.sqrt();
    assert!(
        (rms(&down.out) / want - 1.0).abs() < 0.02,
        "{} vs {want}",
        rms(&down.out)
    );
    let mut ints = [0i32; 9];
    down.player
        .fill_with(&mut ints, 3, |x| (x * 1000.0) as i32 + 7);
    assert!(
        ints.iter().skip(2).step_by(3).all(|&v| v == 7),
        "out 2 silent"
    );
    assert!(ints.iter().step_by(3).any(|&v| v != 7), "out 0 plays");
}

#[test]
fn no_output_channels_or_a_rate_of_zero_are_refused_without_a_panic() {
    let fifo = Arc::new(fifo_for(2, 48_000));
    assert!(Player::new(Arc::clone(&fifo), 0, 48_000, Arc::default()).is_err());
    let mut player = Player::new(fifo, 48_000, 48_000, Arc::default()).unwrap();
    let mut out = [1.0f64; 4];
    player.fill_with(&mut out, 0, |x| x);
    assert_eq!(out, [1.0; 4]);
    assert_eq!(player.stats().played(), 0);
}

#[test]
fn the_fifo_holds_half_a_second_of_the_network() {
    let fifo = fifo_for(2, 96_000);
    assert_eq!((fifo.channels(), fifo.capacity()), (2, 48_000));
}

/// A player for another output (the tray's output menu) counts on from
/// the last one's counts, at its own rate.
#[test]
fn a_new_player_counts_on_in_the_same_stats() {
    let mut s = Sim::new(48_000, 48_000, 0.0, 1, 1);
    s.run(1.0);
    s.pumping = false;
    s.run(0.2);
    s.pumping = true;
    s.fifo.push_with(4800, |_, _| 0.0);
    s.run(0.5);
    let stats = s.player.stats();
    let (played, underruns, recentres) = (stats.played(), stats.underruns(), stats.recentres());
    assert!(played > 0 && underruns > 0 && recentres > 0);
    let mut next = Player::new(Arc::clone(&s.fifo), 48_000, 44_100, stats.clone()).unwrap();
    assert_eq!(stats.out_rate(), 44_100);
    assert!(stats.priming());
    let mut out = [0.0f64; 441];
    next.fill_with(&mut out, 1, |x| x);
    assert_eq!(stats.played(), played + 441);
    assert_eq!(stats.underruns(), underruns);
    assert_eq!(stats.recentres(), recentres);
}

/// Priming ends the block the FIFO holds exactly the level (the target
/// and the sawtooth's half callback and block), not a block later.
#[test]
fn priming_ends_at_exactly_the_level() {
    for (short, primed) in [(1, false), (0, true)] {
        let fifo = Arc::new(fifo_for(1, 48_000));
        let mut player = Player::new(Arc::clone(&fifo), 48_000, 48_000, Arc::default()).unwrap();
        let need = player.conv.input_frames_next();
        // One frame a callback: half of it rounds with the block.
        let level = 1920 + need.div_ceil(2);
        fifo.push_with(level - short, |_, _| 0.25);
        let mut out = [0.0f64; 1];
        player.fill_with(&mut out, 1, |x| x);
        assert_eq!(!player.stats().priming(), primed, "{short} short");
        let taken = if primed { need as u64 } else { 0 };
        assert_eq!(player.consumed(), taken);
        assert_eq!(fifo.len() as u64, (level - short) as u64 - taken);
    }
}

/// The cushion steps down every minute with no dropout, to none.
#[test]
fn the_cushion_steps_down_after_a_calm_minute() {
    let mut s = Sim::new(48_000, 48_000, 0.0, 1, 1);
    for _ in 0..2 {
        s.run(1.0);
        s.pumping = false;
        s.run(0.2);
        s.pumping = true;
    }
    assert_eq!(s.player.steps, 2);
    let since = s.player.calm_since;
    // Just under a minute: no step yet.
    s.run(CUSHION_DECAY_S - 1.0);
    assert_eq!(s.player.steps, 2);
    s.run(2.0);
    assert_eq!(s.player.steps, 1, "one step a calm minute");
    assert!(s.player.calm_since >= since + 60 * 48_000);
    s.run(CUSHION_DECAY_S);
    assert_eq!(s.player.steps, 0);
    s.run(CUSHION_DECAY_S);
    assert_eq!(s.player.steps, 0, "never under none");
    assert_eq!(s.player.stats().underruns(), 2);
    // The next dropout primes one step again.
    s.pumping = false;
    s.run(0.2);
    assert_eq!(s.player.state, State::Priming(2400));
}

/// A FIFO emptied at once (no depth under the floor first): the block
/// whose input is not there plays silence, counted, and the output primes.
#[test]
fn a_block_with_no_input_plays_silence_and_primes() {
    let mut s = Sim::new(48_000, 48_000, 0.0, 1, 1);
    s.run(2.0);
    // Hand out the rest of the current block: the next fill starts one.
    let rest = BLOCK - s.player.pos;
    let mut out = vec![0.0f64; rest];
    s.player.fill_with(&mut out, 1, |x| x);
    let held = s.fifo.len();
    s.fifo.skip(held);
    let mut block = vec![9.0f64; BLOCK];
    s.player.fill_with(&mut block, 1, |x| x);
    assert!(block.iter().all(|&x| x == 0.0), "silence");
    let stats = s.player.stats();
    assert_eq!(stats.underruns(), 1);
    assert_eq!(stats.recentres(), 0, "an underrun, not a re-centre");
    assert_eq!(s.player.state, State::Priming(2400));
}
