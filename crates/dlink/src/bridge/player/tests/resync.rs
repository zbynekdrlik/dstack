// SPDX-License-Identifier: MIT OR Apache-2.0

//! The pump skips blocks (dstack #303): a rebootstrap after a late pump
//! (`dstack::stream`'s `Due::Rebootstrap`) drops blocks, so the next
//! block's media start jumps while the app went on writing the FIFO on its
//! own clock.

use super::*;

/// dsend's and obs's direction: the app (OBS's 1024-frame ticks at 48 kHz,
/// its crystal `drift_ppm` off true time) writes the FIFO; the pump (the
/// device's 64-frame blocks at 96 kHz on true time, the media clock) fills
/// each block through the player, the block's media start in hand.
struct Tx {
    fifo: Arc<Fifo>,
    player: Player,
    drift_ppm: f64,
    /// True time, s.
    t: f64,
    /// The app's frames written.
    pushed: u64,
    /// The next block's media start, device frames.
    start: u64,
    out: Vec<f64>,
}

const APP_RATE: u32 = 48_000;
const DEV_RATE: u32 = 96_000;
const APP_CHUNK: usize = 1024;
const DEV_BLOCK: usize = 64;

impl Tx {
    fn new(drift_ppm: f64) -> Self {
        let fifo = Arc::new(fifo_for(2, APP_RATE));
        let buffer = AppBuffer::new(
            chunk_ns(APP_CHUNK as u64, APP_RATE),
            chunk_ns(DEV_BLOCK as u64, DEV_RATE),
        );
        let player =
            Player::new(Arc::clone(&fifo), APP_RATE, DEV_RATE, buffer, Arc::default()).unwrap();
        Tx {
            fifo,
            player,
            drift_ppm,
            t: 0.0,
            pushed: 0,
            start: 0,
            out: vec![0.0; DEV_BLOCK * 2],
        }
    }

    /// The app's chunks due by the true time.
    fn app_catches_up(&mut self) {
        let due = (self.t * f64::from(APP_RATE) * (1.0 + self.drift_ppm * 1e-6)) as u64;
        while self.pushed + APP_CHUNK as u64 <= due {
            let base = self.pushed;
            self.fifo.push_with(APP_CHUNK, |i, c| {
                let x = (base + i as u64) as f64 / f64::from(APP_RATE);
                0.5 * (TAU * 1000.0 * x + c as f64).sin()
            });
            self.pushed += APP_CHUNK as u64;
        }
    }

    /// One pump block: the true time moves a block, the app writes what is
    /// due, the pump fills the block at its media start.
    fn block(&mut self) {
        self.t += DEV_BLOCK as f64 / f64::from(DEV_RATE);
        self.app_catches_up();
        self.player.follow(Some(self.start), DEV_BLOCK);
        self.player.fill_with(&mut self.out, 2, |x| x);
        self.start += DEV_BLOCK as u64;
    }

    /// `n` blocks the pump never runs (a rebootstrap): the time goes on,
    /// the app writes, the media start moves past them.
    fn skip(&mut self, n: u64) {
        for _ in 0..n {
            self.t += DEV_BLOCK as f64 / f64::from(DEV_RATE);
            self.app_catches_up();
        }
        self.start += n * DEV_BLOCK as u64;
    }

    fn run(&mut self, seconds: f64) {
        let end = self.t + seconds;
        while self.t < end {
            self.block();
        }
    }
}

/// dstack #303 (field: a rebootstrap skipped 8 blocks, then re-centres
/// about 0.8 a second for an hour and the ratio swinging to -75 ppm): the
/// player resyncs once on the jump, then holds the target with no
/// re-centre and the ratio in its normal band.
fn a_skip_of_8_blocks(drift_ppm: f64) {
    let mut s = Tx::new(drift_ppm);
    // Settled: the rate fit locked (60 s) and the correction at the drift.
    s.run(240.0);
    let stats = s.player.stats();
    let (recentres, underruns) = (stats.recentres(), stats.underruns());
    assert_eq!(stats.resyncs(), 0, "{drift_ppm}: no skip yet");
    s.skip(8);
    s.run(1.0);
    assert_eq!(stats.resyncs(), 1, "{drift_ppm}: one resync on the skip");
    // The next ten minutes: no re-centre, no dropout, the ratio near the
    // drift (an app fast by +d gives more input per output: -d ppm).
    let end = s.t + 600.0;
    let mut max_off = 0f64;
    while s.t < end {
        s.block();
        max_off = max_off.max((stats.ppm() + drift_ppm).abs());
    }
    println!(
        "drift {drift_ppm} ppm: recentres {} (before {recentres}), underruns {} (before \
         {underruns}), resyncs {}, max off the drift {max_off:.1} ppm, now {:.1} ppm",
        stats.recentres(),
        stats.underruns(),
        stats.resyncs(),
        stats.ppm()
    );
    assert_eq!(stats.recentres(), recentres, "{drift_ppm}: no re-centre after");
    assert_eq!(stats.underruns(), underruns, "{drift_ppm}: no dropout after");
    assert!(max_off < 20.0, "{drift_ppm}: {max_off} ppm off the drift");
    assert_eq!(stats.resyncs(), 1, "{drift_ppm}: one resync only");
}

#[test]
fn a_pump_skipping_8_blocks_resyncs_once_and_keeps_the_target() {
    a_skip_of_8_blocks(0.0);
}

#[test]
fn a_pump_skipping_8_blocks_resyncs_once_on_a_drifting_app() {
    a_skip_of_8_blocks(6.0);
}
