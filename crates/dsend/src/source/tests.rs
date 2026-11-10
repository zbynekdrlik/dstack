// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

use std::f64::consts::TAU;
use std::sync::Arc;

use dstack_endpoint::fifo::Fifo;
use dstack_endpoint::player::{Stats, TARGET_S, fifo_for};

use crate::capture::push_interleaved;

/// The pump's block (dlink's default).
const PUMP_BLOCK: usize = 128;

/// The captured output and the network as two clocks: the capture hands
/// packets of 10 ms (WASAPI shared) at its rate, `drift_ppm` fast on true
/// time (its crystal); the pump takes blocks of 128 frames at the
/// network's rate on true time (PTP). Channel `c` of the capture is
/// `signal(frame, c)`.
struct Sim {
    fifo: Arc<Fifo>,
    source: Source,
    switch: Switch,
    stats: Arc<Stats>,
    cap_rate: f64,
    net_rate: f64,
    drift_ppm: f64,
    channels: usize,
    packet: usize,
    signal: fn(u64, usize, f64) -> f64,
    /// True time, s.
    t: f64,
    /// Frames the capture handed over.
    pushed: u64,
    /// The capture runs (false: it stopped, no packets).
    capturing: bool,
    /// The last block's TX output, channel-major.
    out: Vec<f64>,
    max_depth: usize,
    depth_sum: f64,
    depth_n: f64,
}

/// A 1 kHz sine at 0.5, channel `c` phase-shifted by `c` radians.
fn tone(frame: u64, c: usize, rate: f64) -> f64 {
    0.5 * (TAU * 1000.0 * frame as f64 / rate + c as f64).sin()
}

impl Sim {
    fn new(cap_rate: u32, net_rate: u32, drift_ppm: f64, channels: usize) -> Self {
        let fifo = Arc::new(fifo_for(channels, cap_rate));
        let stats: Arc<Stats> = Arc::default();
        let player = Player::new(Arc::clone(&fifo), cap_rate, net_rate, Arc::clone(&stats))
            .expect("the player");
        let (source, switch) = source(player);
        Sim {
            fifo,
            source,
            switch,
            stats,
            cap_rate: f64::from(cap_rate),
            net_rate: f64::from(net_rate),
            drift_ppm,
            channels,
            packet: cap_rate as usize / 100,
            signal: tone,
            t: 0.0,
            pushed: 0,
            capturing: true,
            out: vec![0.0; PUMP_BLOCK * TX_CHANNELS],
            max_depth: 0,
            depth_sum: 0.0,
            depth_n: 0.0,
        }
    }

    /// One pump block: the capture catches up to true time, then the pump
    /// takes its block.
    fn step(&mut self) {
        self.t += PUMP_BLOCK as f64 / self.net_rate;
        let due = (self.t * self.cap_rate * (1.0 + self.drift_ppm * 1e-6)) as u64;
        while self.capturing && self.pushed + self.packet as u64 <= due {
            let ch = self.channels;
            let data: Vec<f32> = (0..self.packet * ch)
                .map(|i| {
                    let frame = self.pushed + (i / ch) as u64;
                    (self.signal)(frame, i % ch, self.cap_rate) as f32
                })
                .collect();
            push_interleaved(&self.fifo, &data, ch, f64::from);
            self.pushed += self.packet as u64;
        }
        if !self.capturing {
            // A stopped capture: its clock still runs, no packets come.
            self.pushed = due;
        }
        self.max_depth = self.max_depth.max(self.fifo.len());
        let before = self.fifo.len();
        self.source
            .process(&mut Block::new(PUMP_BLOCK, &[], &mut self.out));
        self.depth_sum += (before + self.fifo.len()) as f64;
        self.depth_n += 2.0;
    }

    fn run(&mut self, seconds: f64) {
        let end = self.t + seconds;
        while self.t < end {
            self.step();
        }
    }

    /// Runs `seconds` and returns TX channel `ch`.
    fn record(&mut self, seconds: f64, ch: usize) -> Vec<f64> {
        let mut got = Vec::new();
        let end = self.t + seconds;
        while self.t < end {
            self.step();
            got.extend_from_slice(&self.out[ch * PUMP_BLOCK..(ch + 1) * PUMP_BLOCK]);
        }
        got
    }

    /// The mean depth since the last call, ms of the capture's rate.
    fn depth_ms(&mut self) -> f64 {
        let mean = self.depth_sum / self.depth_n / self.cap_rate * 1000.0;
        (self.depth_sum, self.depth_n) = (0.0, 0.0);
        mean
    }
}

fn rms(x: &[f64]) -> f64 {
    (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt()
}

/// A tone's frequency from its rising zero crossings, Hz.
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

/// The issue's test: a captured output ±200 ppm off the network over four
/// simulated minutes. The depth settles at the target, the controller
/// follows the drift (a capture fast by +d gives more frames than the
/// network takes: −d ppm, less output per input), and after the priming
/// there is no underrun and no overflow.
fn drifting(drift: f64) {
    // One channel (a mono output on both TX channels): the clocks are the
    // test, the converter costs per channel.
    let mut s = Sim::new(48_000, 48_000, drift, 1);
    s.run(210.0);
    s.depth_ms();
    s.run(30.0);
    let depth = s.depth_ms();
    let stats = Arc::clone(&s.stats);
    println!(
        "capture drift {drift} ppm: mean depth {depth:.2} ms, ppm {:.2}, underruns {}, \
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
    assert!(!stats.priming(), "{drift}: sending");
    assert!(
        (depth - TARGET_S * 1000.0).abs() < 3.0,
        "{drift}: depth {depth} ms"
    );
    assert!(
        (stats.ppm() + drift).abs() < 3.0,
        "{drift}: correction {} ppm",
        stats.ppm()
    );
    // The pump got every frame it asked for, in whole blocks.
    assert_eq!(stats.played() % PUMP_BLOCK as u64, 0);
    assert!(stats.played() as f64 >= 239.0 * 48_000.0);
    // L and R both carry the mono capture.
    let l = s.record(0.05, 0);
    let r = s.record(0.05, 1);
    assert!(rms(&l) > 0.3 && rms(&r) > 0.3, "both channels send");
}

#[test]
fn a_capture_200_ppm_fast_holds_the_target_with_no_underrun_or_overrun() {
    drifting(200.0);
}

#[test]
fn a_capture_200_ppm_slow_holds_the_target_with_no_underrun_or_overrun() {
    drifting(-200.0);
}

/// The captured output at another rate than the network: the capture's
/// frames are taken at its rate per network second, and a 1 kHz sine is
/// sent at 1 kHz at its level, L and R each their own.
fn converts(cap_rate: u32, net_rate: u32) {
    let mut s = Sim::new(cap_rate, net_rate, 0.0, 2);
    // The 10 ms packets make the first fit ride a transient (up to about
    // 140 ppm at 44.1 to 48 kHz); a minute settles it.
    s.run(60.0);
    let consumed = s.source.player.consumed();
    let played = s.stats.played();
    let l = s.record(10.0, 0);
    let per_s = (s.source.player.consumed() - consumed) as f64
        / ((s.stats.played() - played) as f64 / f64::from(net_rate));
    let freq = frequency(&l, f64::from(net_rate));
    let level_db = 20.0 * (rms(&l) / (0.5 / 2f64.sqrt())).log10();
    println!(
        "{cap_rate} -> {net_rate}: {per_s:.2} capture frames a network second, {freq:.4} Hz, \
         level {level_db:+.4} dB, depth {:.2} ms",
        s.depth_ms()
    );
    assert_eq!(s.stats.underruns(), 0);
    assert_eq!(s.fifo.overflows(), 0);
    assert!(
        (per_s / f64::from(cap_rate) - 1.0).abs() < 100e-6,
        "{cap_rate} -> {net_rate}: {per_s}"
    );
    assert!((freq - 1000.0).abs() < 0.1, "{freq} Hz");
    assert!(level_db.abs() < 0.5, "{level_db} dB");
    let (l, r) = (&s.out[..PUMP_BLOCK], &s.out[PUMP_BLOCK..]);
    assert!(
        l.iter().zip(r).any(|(l, r)| (l - r).abs() > 0.1),
        "R is its own channel"
    );
}

#[test]
fn a_44_1_khz_output_is_sent_on_a_48_khz_network_at_its_frequency_and_level() {
    converts(44_100, 48_000);
}

#[test]
fn a_48_khz_output_is_sent_on_a_96_khz_network_at_its_frequency_and_level() {
    converts(48_000, 96_000);
}

/// Channel `c` holds the constant `0.1 * (c + 1)`.
fn level(_: u64, c: usize, _: f64) -> f64 {
    0.1 * (c + 1) as f64
}

fn settled(channels: usize) -> (f64, f64) {
    let mut s = Sim::new(48_000, 48_000, 0.0, channels);
    s.signal = level;
    s.run(2.0);
    let last = PUMP_BLOCK - 1;
    (s.out[last], s.out[PUMP_BLOCK + last])
}

#[test]
fn l_and_r_come_from_the_output_s_first_two_channels() {
    let near = |x: f64, want: f64| (x - want).abs() < 1e-4;
    // Stereo: L, R.
    let (l, r) = settled(2);
    assert!(near(l, 0.1) && near(r, 0.2), "{l} {r}");
    // 5.1 (FL FR C LFE SL SR): the front pair, the rest left out.
    let (l, r) = settled(6);
    assert!(near(l, 0.1) && near(r, 0.2), "{l} {r}");
    // Mono: on both.
    let (l, r) = settled(1);
    assert!(near(l, 0.1) && near(r, 0.1), "{l} {r}");
}

fn silence(_: u64, _: usize, _: f64) -> f64 {
    0.0
}

#[test]
fn an_idle_output_kept_alive_sends_exact_silence_with_no_underrun() {
    // The keep-alive stream makes the loopback hand over zeros.
    let mut s = Sim::new(48_000, 48_000, 0.0, 2);
    s.signal = silence;
    s.run(5.0);
    let l = s.record(1.0, 0);
    let r = s.record(1.0, 1);
    assert!(l.iter().chain(&r).all(|&x| x == 0.0), "exact silence");
    assert_eq!(s.stats.underruns(), 0);
    assert!(!s.stats.priming());
}

#[test]
fn a_stopped_capture_sends_silence_counts_an_underrun_and_recovers() {
    let mut s = Sim::new(48_000, 48_000, 0.0, 2);
    s.run(5.0);
    assert_eq!(s.stats.underruns(), 0);
    s.capturing = false;
    s.run(0.5);
    let quiet = s.record(1.0, 0);
    assert!(
        quiet.iter().all(|&x| x == 0.0),
        "silence while it is stopped"
    );
    assert_eq!(s.stats.underruns(), 1, "one dropout, then priming");
    assert!(s.stats.priming());
    s.capturing = true;
    s.run(3.0);
    let back = s.record(1.0, 0);
    assert!(rms(&back) > 0.3, "the sound is back");
    assert_eq!(s.stats.underruns(), 1, "no underrun after the new priming");
}

#[test]
fn another_output_s_player_takes_over_at_the_next_block() {
    let mut s = Sim::new(48_000, 48_000, 0.0, 2);
    s.run(2.0);
    let before = s.stats.played();
    // The new output: 44.1 kHz, mono, a constant 0.25.
    let fifo = Arc::new(fifo_for(1, 44_100));
    let next = Player::new(Arc::clone(&fifo), 44_100, 48_000, Arc::clone(&s.stats)).unwrap();
    assert_eq!(s.switch.send(next), Ok(()));
    // The pump has not taken it: another is refused, never queued.
    let spare = Player::new(Arc::clone(&fifo), 44_100, 48_000, Arc::clone(&s.stats)).unwrap();
    assert_eq!(s.switch.send(spare), Err(NOT_TAKEN));
    assert_eq!(s.switch.collect(), 0, "nothing replaced yet");
    s.fifo = fifo;
    s.cap_rate = 44_100.0;
    s.packet = 441;
    s.channels = 1;
    s.pushed = (s.t * 44_100.0) as u64;
    s.signal = level;
    s.step();
    assert_eq!(
        s.switch.collect(),
        1,
        "the old player came back to be freed"
    );
    s.run(2.0);
    let last = PUMP_BLOCK - 1;
    assert!((s.out[last] - 0.1).abs() < 1e-4, "{}", s.out[last]);
    assert!((s.out[PUMP_BLOCK + last] - 0.1).abs() < 1e-4);
    assert!(s.stats.played() > before, "the counts go on");
    assert_eq!(s.stats.out_rate(), 48_000);
}

#[test]
fn a_block_longer_than_a_chunk_or_with_one_tx_channel_is_filled_whole() {
    let mut s = Sim::new(48_000, 48_000, 0.0, 2);
    s.signal = level;
    s.run(2.0);
    let frames = CHUNK * 2 + 300;
    let mut out = vec![9.0; frames * TX_CHANNELS];
    // Enough capture for the long block.
    let data = [0.1f32, 0.2].repeat(frames);
    push_interleaved(&s.fifo, &data, 2, f64::from);
    let played = s.stats.played();
    s.source.process(&mut Block::new(frames, &[], &mut out));
    assert_eq!(
        s.stats.played() - played,
        frames as u64,
        "exactly the block"
    );
    assert!(out[frames - 1] != 9.0 && out[2 * frames - 1] != 9.0);
    assert!((out[frames - 1] - 0.1).abs() < 1e-4, "{}", out[frames - 1]);
    assert!((out[2 * frames - 1] - 0.2).abs() < 1e-4);
    // One TX channel mapped: L only, no panic.
    let mut one = vec![9.0; PUMP_BLOCK];
    s.source.process(&mut Block::new(PUMP_BLOCK, &[], &mut one));
    assert!((one[PUMP_BLOCK - 1] - 0.1).abs() < 1e-4);
}

#[test]
fn a_switch_to_a_pump_that_is_gone_gives_the_player_back() {
    let fifo = Arc::new(fifo_for(2, 48_000));
    let player = || Player::new(Arc::clone(&fifo), 48_000, 48_000, Arc::default()).unwrap();
    let (source, mut switch) = source(player());
    drop(source);
    assert_eq!(switch.send(player()), Err(GONE));
    assert_eq!(TX_NAMES, ["L", "R"]);
}

#[test]
fn every_replaced_player_comes_back_to_be_freed_never_in_the_pump() {
    let fifo = Arc::new(fifo_for(2, 48_000));
    let stats: Arc<Stats> = Arc::default();
    let player = || Player::new(Arc::clone(&fifo), 48_000, 48_000, Arc::clone(&stats)).unwrap();
    let (mut source, mut switch) = source(player());
    let mut out = vec![0.0; PUMP_BLOCK * TX_CHANNELS];
    let mut back = 0;
    for _ in 0..5 {
        assert_eq!(switch.send(player()), Ok(()));
        source.process(&mut Block::new(PUMP_BLOCK, &[], &mut out));
        // A block with no handover retires nothing.
        source.process(&mut Block::new(PUMP_BLOCK, &[], &mut out));
        back += switch.collect();
    }
    assert_eq!(back, 5, "each handover's predecessor, on this thread");
    assert_eq!(switch.pending, 0);
}

#[test]
fn a_send_is_refused_while_the_room_back_is_taken() {
    let fifo = Arc::new(fifo_for(2, 48_000));
    let player = || Player::new(Arc::clone(&fifo), 48_000, 48_000, Arc::default()).unwrap();
    let (mut source, mut switch) = source(player());
    let mut out = vec![0.0; PUMP_BLOCK * TX_CHANNELS];
    // Players replaced but not yet back (the pump's try_send not run).
    switch.pending = RETIRED;
    assert_eq!(switch.send(player()), Err(NOT_TAKEN));
    switch.pending = RETIRED - 1;
    assert_eq!(switch.send(player()), Ok(()));
    assert_eq!(switch.pending, RETIRED);
    source.process(&mut Block::new(PUMP_BLOCK, &[], &mut out));
    // One came back: room again.
    assert_eq!(switch.send(player()), Ok(()));
}
