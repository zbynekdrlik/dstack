// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

use std::f64::consts::TAU;

use crate::bridge::chunk_ns;

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

/// dlisten's default (dstack #247): its 10 ms WASAPI callback and one pump
/// block at the network's rate.
fn buffer(net_rate: u32) -> AppBuffer {
    AppBuffer::new(10_000_000, chunk_ns(PUMP_BLOCK as u64, net_rate))
}

/// [`buffer`]'s target at 48 kHz, frames: 10 ms + 128 frames.
const TARGET_48K: usize = 480 + 128;

impl Sim {
    fn new(net_rate: u32, out_rate: u32, drift_ppm: f64, channels: usize, outputs: usize) -> Self {
        Self::with(
            buffer(net_rate),
            net_rate,
            out_rate,
            drift_ppm,
            channels,
            outputs,
        )
    }

    fn with(
        buffer: AppBuffer,
        net_rate: u32,
        out_rate: u32,
        drift_ppm: f64,
        channels: usize,
        outputs: usize,
    ) -> Self {
        let fifo = Arc::new(fifo_for(channels, net_rate));
        let player = Player::new(
            Arc::clone(&fifo),
            net_rate,
            out_rate,
            buffer,
            buffer.chunk_ns(),
            Arc::default(),
        )
        .unwrap();
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

/// (a) An output crystal ±100 ppm off the network's at equal rates over
/// four simulated minutes at dlisten's default target (10 ms + a block):
/// the depth settles at the target, the controller follows the drift (an
/// output fast by +d takes more frames: +d ppm, more output per input), no
/// underrun after the priming, no overflow.
#[test]
fn an_output_100_ppm_fast_keeps_the_default_target_with_no_underrun() {
    drifting(100.0, buffer(48_000));
}

#[test]
fn an_output_100_ppm_slow_keeps_the_default_target_with_no_underrun() {
    drifting(-100.0, buffer(48_000));
}

/// ±200 ppm at a larger app buffer (dstack #247): the correction may move
/// only 5 ppm a second (inaudibly), so reaching 200 ppm takes 40 s and
/// costs about 4 to 6 ms of depth meanwhile. At the default an output
/// 200 ppm fast drops out once or twice at the start (the slow one holds);
/// 20 ms holds both.
#[test]
fn an_output_200_ppm_fast_keeps_a_20_ms_target_with_no_underrun() {
    drifting(200.0, buffer(48_000).set(Some(20_000_000)));
}

#[test]
fn an_output_200_ppm_slow_keeps_a_20_ms_target_with_no_underrun() {
    drifting(-200.0, buffer(48_000).set(Some(20_000_000)));
}

fn drifting(drift: f64, buffer: AppBuffer) {
    {
        // One channel: the clocks are the test, the converter costs per channel.
        let mut s = Sim::with(buffer, 48_000, 48_000, drift, 1, 1);
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
            (depth - s.player.target_s() * 1000.0).abs() < 2.5,
            "{drift}: depth {depth} ms"
        );
        assert!((stats.target_ms() - buffer.target_ns() as f64 / 1e6).abs() < 1e-9);
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
    // The FIFO's 12.7 ms drain, then silence.
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

/// No growth after a dropout (dstack #247): each underrun primes to the
/// same target, the count goes up, and once playing again the depth sits
/// at the target, not over it.
#[test]
fn a_dropout_is_counted_and_never_grows_the_target() {
    let mut s = Sim::new(48_000, 48_000, 0.0, 1, 1);
    let mut levels = Vec::new();
    for n in 1..=6 {
        s.run(1.0);
        s.pumping = false;
        s.run(0.2);
        let State::Priming(level) = s.player.state else {
            panic!("primes after the underrun")
        };
        levels.push(level);
        assert_eq!(s.player.stats().underruns(), n);
        assert_eq!(s.player.ctl.cushion_s(), 0.0, "no cushion");
        s.pumping = true;
    }
    assert_eq!(levels, [TARGET_48K; 6]);
    assert!((s.player.target_s() - 0.012_666_667).abs() < 1e-9);
    s.run(30.0);
    s.depth_ms();
    s.run(10.0);
    let depth = s.depth_ms();
    assert!(
        (depth - s.player.target_s() * 1000.0).abs() < 2.5,
        "back at the target, not over it: {depth} ms"
    );
    assert_eq!(s.player.stats().underruns(), 6);
    assert!((s.player.stats().target_ms() - 12.666_667).abs() < 1e-3);
}

/// A configured app buffer is the target as given (dstack #247): a 40 ms
/// one keeps the depth at 40 ms, whatever the chunk.
#[test]
fn a_configured_app_buffer_is_the_target_verbatim() {
    let set = buffer(48_000).set(Some(40_000_000));
    let mut s = Sim::with(set, 48_000, 48_000, 0.0, 1, 1);
    assert_eq!(s.player.target_s(), 0.040);
    assert_eq!(s.player.state, State::Priming(1920));
    s.run(5.0);
    s.depth_ms();
    s.run(5.0);
    let depth = s.depth_ms();
    assert!((depth - 40.0).abs() < 2.5, "depth {depth} ms");
    assert_eq!(s.player.stats().target_ms(), 40.0);
    assert_eq!(s.player.stats().underruns(), 0);
}

/// The excess bound follows the target: twice it, and at least twice the
/// chunk. There is no floor (dstack #303): a deficit is a dropout only when
/// the FIFO cannot serve the converter's call
/// (`a_low_depth_that_still_serves_the_call_plays_on`).
#[test]
fn the_excess_follows_the_target() {
    assert_eq!(hard_excess_s(0.0226, 0.0213), 0.0452);
    // A target set under the chunk: twice the chunk, at most twice the
    // largest target.
    assert_eq!(hard_excess_s(0.002, 0.010), 0.020);
    assert_eq!(hard_excess_s(0.150, 0.010), 0.300);
    assert_eq!(hard_excess_s(0.010, 0.200), 0.300);
}

/// Audio that came before the output ran is skipped at the start, not
/// drained over minutes: playing starts at the target.
#[test]
fn an_excess_at_the_start_is_skipped_to_the_target() {
    let fifo = Arc::new(fifo_for(1, 48_000));
    fifo.push_with(9600, |_, _| 0.25); // 200 ms
    let mut player = Player::new(
        Arc::clone(&fifo),
        48_000,
        48_000,
        buffer(48_000),
        buffer(48_000).chunk_ns(),
        Arc::default(),
    )
    .unwrap();
    let mut out = [0.0f32; 1];
    player.fill_with(&mut out, 1, |x| x as f32);
    assert!(!player.stats().priming());
    // The target and half the chunk (10 ms, wider than the one-frame
    // callback) and the block (the reads' sawtooth), less the first
    // block's input.
    let need = player.consumed() as usize;
    assert!(need > 0);
    assert_eq!(fifo.len(), TARGET_48K + (480 + need) / 2 - need);
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
        (depth - s.player.target_s() * 1000.0).abs() < 6.0,
        "back near the target: {depth} ms"
    );
}

/// A low depth that still serves the converter's call plays on (dstack
/// #303): no re-centre, no dropout. A writer late inside its app's pass
/// (OBS's NDI output ahead of dlink's TX tap, ~10 ms) took the depth under
/// the old 5 ms floor while the FIFO still held audio, and the floor's
/// re-prime made a whole target of silence out of it. Only the call the
/// FIFO cannot serve is a dropout: counted as an underrun (not as a
/// re-centre), and primed to the same target, no cushion (dstack #247).
#[test]
fn a_low_depth_that_still_serves_the_call_plays_on() {
    let mut s = Sim::with(
        buffer(48_000).set(Some(40_000_000)),
        48_000,
        48_000,
        0.0,
        1,
        1,
    );
    s.run(5.0);
    // Hand out the rest of the current block: the next fill starts one.
    let rest = BLOCK - s.player.pos;
    let mut out = vec![0.0; rest];
    s.player.fill_with(&mut out, 1, |x| x);
    // Leave 3 ms: under the old 5 ms floor, over a block's input.
    let held = s.fifo.len();
    s.fifo.skip(held - 144);
    let need = s.player.conv.input_frames_next();
    assert!(need <= 144, "a call's input fits: {need}");
    let mut out = vec![9.0; BLOCK];
    s.player.fill_with(&mut out, 1, |x| x);
    let stats = s.player.stats();
    assert_eq!(stats.recentres(), 0, "no floor");
    assert_eq!(stats.underruns(), 0, "the call was served");
    assert_eq!(s.player.state, State::Running);
    assert!(!stats.priming());
    assert!(out.iter().any(|&x| x != 0.0), "audio, not silence");
    assert_eq!(s.fifo.len(), 144 - need);
    // No more input: the call the FIFO cannot serve is the dropout.
    let mut n = 0;
    while s.player.state == State::Running {
        let mut out = vec![9.0; BLOCK];
        s.player.fill_with(&mut out, 1, |x| x);
        n += 1;
        assert!(n < 10, "the FIFO runs dry within a few calls");
    }
    let stats = s.player.stats();
    assert_eq!(stats.underruns(), 1, "a real underrun is a dropout");
    assert_eq!(stats.recentres(), 0, "an underrun, not a re-centre");
    assert_eq!(
        s.player.state,
        State::Priming(1920),
        "to the target, no cushion"
    );
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
    assert!(
        Player::new(
            Arc::clone(&fifo),
            0,
            48_000,
            buffer(48_000),
            buffer(48_000).chunk_ns(),
            Arc::default()
        )
        .is_err()
    );
    let mut player = Player::new(
        fifo,
        48_000,
        48_000,
        buffer(48_000),
        buffer(48_000).chunk_ns(),
        Arc::default(),
    )
    .unwrap();
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
    // Playing again, then a burst past the excess bound: an underrun (the
    // stop) and a re-centre (the burst) to count on from. The deficit
    // re-centre of the floor is gone (dstack #303).
    s.run(1.0);
    s.fifo.push_with(4800, |_, _| 0.0);
    s.run(0.5);
    let stats = s.player.stats();
    let (played, underruns, recentres) = (stats.played(), stats.underruns(), stats.recentres());
    assert!(played > 0, "played {played}");
    assert_eq!(underruns, 1, "the stop");
    assert_eq!(recentres, 1, "the burst");
    let mut next = Player::new(
        Arc::clone(&s.fifo),
        48_000,
        44_100,
        buffer(48_000),
        buffer(48_000).chunk_ns(),
        stats.clone(),
    )
    .unwrap();
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
        let mut player = Player::new(
            Arc::clone(&fifo),
            48_000,
            48_000,
            buffer(48_000),
            buffer(48_000).chunk_ns(),
            Arc::default(),
        )
        .unwrap();
        let need = player.conv.input_frames_next();
        // One frame a callback: the 10 ms chunk is the wider swing.
        let level = TARGET_48K + (480 + need) / 2;
        fifo.push_with(level - short, |_, _| 0.25);
        let mut out = [0.0f64; 1];
        player.fill_with(&mut out, 1, |x| x);
        assert_eq!(!player.stats().priming(), primed, "{short} short");
        let taken = if primed { need as u64 } else { 0 };
        assert_eq!(player.consumed(), taken);
        assert_eq!(fifo.len() as u64, (level - short) as u64 - taken);
    }
}

/// A FIFO emptied at once: the block
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
    assert_eq!(s.player.state, State::Priming(TARGET_48K));
}

/// dstack #303: a converter error plays its block as silence and is
/// counted (`Stats::convert_errors`), never silent in the counts; the
/// output runs on (no re-prime), and a later player on the same stats
/// counts on from it.
#[test]
fn a_converter_error_plays_silence_and_is_counted() {
    let mut s = Sim::new(48_000, 48_000, 0.0, 1, 1);
    s.run(2.0);
    // Hand out the rest of the current block: the next fill starts one.
    let rest = BLOCK - s.player.pos;
    let mut out = vec![0.0f64; rest];
    s.player.fill_with(&mut out, 1, |x| x);
    // A converter for two channels on a one-channel player: its process
    // refuses the player's one-channel buffers (the injected error).
    let good = std::mem::replace(
        &mut s.player.conv,
        Converter::drifting(48_000, 48_000, 2, BLOCK, Fixed::Output).unwrap(),
    );
    let consumed = s.player.consumed();
    let mut block = vec![9.0f64; BLOCK];
    s.player.fill_with(&mut block, 1, |x| x);
    assert!(block.iter().all(|&x| x == 0.0), "silence");
    let stats = s.player.stats();
    assert_eq!(stats.convert_errors(), 1);
    assert_eq!(stats.underruns(), 0, "a converter error, not an underrun");
    assert_eq!(s.player.state, State::Running, "no re-prime");
    assert!(s.player.consumed() > consumed, "its input is taken");
    s.player.fill_with(&mut block, 1, |x| x);
    assert_eq!(s.player.stats().convert_errors(), 2, "one a block");
    s.player.conv = good;
    s.player.fill_with(&mut block, 1, |x| x);
    assert_eq!(
        s.player.stats().convert_errors(),
        2,
        "none once it converts"
    );
    let stats = s.player.stats();
    let mut next = Player::new(
        Arc::clone(&s.fifo),
        48_000,
        48_000,
        buffer(48_000),
        buffer(48_000).chunk_ns(),
        Arc::clone(&stats),
    )
    .unwrap();
    let mut out = vec![0.0f64; 64];
    next.fill_with(&mut out, 1, |x| x);
    assert_eq!(stats.convert_errors(), 2, "the next player counts on");
}

#[test]
fn under_mono_both_speakers_play_the_sum_of_our_channels() {
    let mut s = Sim::new(48_000, 48_000, 0.0, 2, 2);
    let stand_in = Player::new(
        Arc::new(fifo_for(2, 48_000)),
        48_000,
        48_000,
        buffer(48_000),
        buffer(48_000).chunk_ns(),
        Arc::default(),
    );
    let player = std::mem::replace(&mut s.player, stand_in.unwrap());
    s.player = player.with_mono(true);
    s.run(2.0);
    for f in s.out.as_chunks::<2>().0 {
        assert_eq!(f[0], f[1], "the sum on both");
    }
    // L and R one radian apart, summed at 0 dB: 2 cos(0.5) of one's level.
    let want = s.amp * 2.0 * (0.5f64).cos() / 2f64.sqrt();
    assert!(
        (rms(&s.out) / want - 1.0).abs() < 0.02,
        "{} vs {want}",
        rms(&s.out)
    );
}

/// A player on an output 50 ppm fast, seeded with `seed`, after 2 s.
fn seeded(seed: Option<f64>) -> Sim {
    let mut s = Sim::new(48_000, 48_000, 50.0, 1, 1);
    let player = Player::new(
        Arc::clone(&s.fifo),
        48_000,
        48_000,
        buffer(48_000),
        buffer(48_000).chunk_ns(),
        s.player.stats(),
    )
    .unwrap();
    s.player = match seed {
        Some(ppm) => player.with_seed_ppm(ppm),
        None => player,
    };
    s.run(2.0);
    s
}

/// dstack #382: the ASRC seeded with the last run's locked rate starts at
/// it (the controller's own first-minute test, `asrc::tests::seed`, shows
/// the depth it saves), and the counts keep the rate the fit locks on, for
/// the next run, across a reopen's new player too.
#[test]
fn a_seeded_player_starts_at_the_seed_and_its_counts_keep_the_lock() {
    let unseeded = seeded(None).player.stats().ppm();
    let mut s = seeded(Some(50.0));
    let stats = s.player.stats();
    assert!(unseeded < 10.0, "unseeded: slewing from 0: {unseeded}");
    assert!(
        (stats.ppm() - 50.0).abs() < 1.0,
        "seeded: from the seed: {}",
        stats.ppm()
    );
    assert_eq!(stats.locked_ppm(), None, "no lock before 60 s");
    s.run(58.0);
    assert_eq!(stats.locked_ppm(), None, "no lock before 60 s");
    s.run(20.0);
    let locked = stats.locked_ppm().expect("locked after 60 s");
    assert!((locked - 50.0).abs() < 1.0, "{locked}");
    assert_eq!(stats.underruns(), 0);
    // The next player on the same counts (a reopen) keeps the last lock
    // until it locks itself.
    s.player = Player::new(
        Arc::clone(&s.fifo),
        48_000,
        48_000,
        buffer(48_000),
        buffer(48_000).chunk_ns(),
        Arc::clone(&stats),
    )
    .unwrap()
    .with_seed_ppm(locked);
    s.run(1.0);
    assert_eq!(stats.locked_ppm(), Some(locked));
}

/// A 96 <-> 96 kHz player of two channels, its output 50 ppm fast, with
/// `taps` (`None`: the default).
fn with_taps(taps: Option<usize>) -> Sim {
    let mut s = Sim::new(96_000, 96_000, 50.0, 2, 2);
    let player = Player::new(
        Arc::clone(&s.fifo),
        96_000,
        96_000,
        buffer(96_000),
        buffer(96_000).chunk_ns(),
        s.player.stats(),
    )
    .unwrap();
    s.player = match taps {
        Some(taps) => player.with_taps(taps).unwrap(),
        None => player,
    };
    s
}

/// dstack #382: a player's converter length. The default keeps 256 taps
/// (1.333 ms at 96 <-> 96 kHz); 48 taps delay 0.250 ms and play the tone
/// at its rate, frequency and level, both channels, with no underrun; a
/// length the converter refuses is refused.
#[test]
fn a_player_with_48_taps_delays_a_quarter_ms_and_plays_as_before() {
    assert_eq!(
        with_taps(None).player.stats().converter_delay_ns(),
        1_333_333
    );
    assert_eq!(
        with_taps(Some(256)).player.stats().converter_delay_ns(),
        1_333_333
    );
    let mut s = with_taps(Some(48));
    let stats = s.player.stats();
    assert_eq!(stats.converter_delay_ns(), 250_000);
    s.run(20.0);
    let tone = s.record(5.0, 0);
    let freq = frequency(&tone, s.out_rate * (1.0 + s.drift_ppm * 1e-6));
    let level_db = 20.0 * (rms(&tone) / (s.amp / 2f64.sqrt())).log10();
    assert_eq!(stats.underruns(), 0);
    assert_eq!(s.fifo.overflows(), 0);
    assert!((freq - s.freq).abs() < 0.1, "{freq} Hz");
    assert!(level_db.abs() < 0.1, "{level_db} dB");
    let r = s.record(0.1, 1);
    assert!(rms(&r) > 0.3, "R plays");
    // 96 to 44.1 kHz: the same quarter ms, read in whole frames at the
    // output's rate (11 frames, 249433 ns; at the input's 114583).
    let fifo = Arc::new(fifo_for(1, 96_000));
    let p = Player::new(fifo, 96_000, 44_100, buffer(96_000), 0, Arc::default())
        .unwrap()
        .with_taps(48)
        .unwrap();
    let delay = p.stats().converter_delay_ns();
    assert_eq!(delay, 249_433);
    let fifo = Arc::new(fifo_for(1, 96_000));
    let p = Player::new(fifo, 96_000, 96_000, buffer(96_000), 0, Arc::default()).unwrap();
    assert!(p.with_taps(33).is_err());
}
