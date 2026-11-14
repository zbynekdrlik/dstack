// SPDX-License-Identifier: MIT OR Apache-2.0

//! The soak's path (dstack #304), shared by the simulated cases
//! (`tests/soak.rs`) and the real-time one (`tests/soak_real_time.rs`):
//! dlink's real app path in OBS's shape, the app's ends of its bridges and
//! the counts a case reads.

// Each test binary uses its own part of the harness.
#![allow(dead_code)]

use std::f64::consts::TAU;
use std::sync::{Arc, Mutex, PoisonError};

use dlink::bridge::{self, AppBuffer, Fifo, Player, Stats};
use dlink::{Block, Config, Map, Process, Rated};
use dstack::clock::to_samples;
use dstack::media::ring::Ring;
use dstack::media::rx::RxShared;
use dstack::media::tx::TxShared;
use dstack::stream::{Pump, PumpStats, Timing};

/// The app's rate and the device's (stream-snv: OBS at 48 kHz, the site's
/// 96 kHz network).
pub(crate) const APP_RATE: u32 = 48_000;
pub(crate) const DEVICE_RATE: u32 = 96_000;
/// The device's block (dlink's default) and the app block it makes.
pub(crate) const BLOCK: u64 = 128;
pub(crate) const APP_BLOCK: u64 = 64;
/// OBS's audio tick, app frames.
pub(crate) const CHUNK: usize = 1024;
/// The channels each way (OBS's program L/R; the RX pair).
pub(crate) const CHANNELS: usize = 2;
/// W: inside it the 8 ms late wakes the owner named (#304).
pub(crate) const W_NS: u64 = 9_000_000;
/// The media clock's start (1000 s) and the app clock's reading there.
pub(crate) const T0: u64 = 1_000_000_000_000;
pub(crate) const APP0: i64 = 7_000_000_000;
pub(crate) const SECOND: u64 = 1_000_000_000;
pub(crate) const MS: u64 = 1_000_000;

/// One app block as the app saw it: its start (app frames) and play time.
pub(crate) type Mark = (Option<u64>, Option<i64>);

/// The app's processor at its own rate (inside [`Rated`]): OBS's
/// dante-input shape.
pub(crate) struct App {
    /// The TX bridge: the app's own writes, converted to the device's
    /// clock.
    pub(crate) tx: Player,
    /// Interleaved TX frames of one block.
    pub(crate) out: Vec<f64>,
    /// The RX bridge's FIFO (the app's own player reads it).
    pub(crate) rx: Arc<Fifo>,
    /// Each block's start and play time, for the test.
    pub(crate) marks: Arc<Mutex<Vec<Mark>>>,
}

impl Process for App {
    fn process(&mut self, block: &mut Block<'_>) {
        let frames = block.frames();
        let out = &mut self.out[..frames * CHANNELS];
        self.tx.fill_at(block.start(), out, CHANNELS, |x| x);
        for c in 0..CHANNELS {
            for (f, y) in block.output(c).iter_mut().enumerate() {
                *y = out[f * CHANNELS + c];
            }
        }
        let input = [block.input(0), block.input(1)];
        self.rx.push_with(frames, |f, c| input[c][f]);
        self.marks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((block.start(), block.play_at()));
    }
}

/// The device's config: stream-snv's shape (#303).
pub(crate) fn config(dir: &std::path::Path) -> Config {
    Config::new("soak", dir.join("state.toml"))
        .rx(["In L", "In R"])
        .tx(["Out L", "Out R"])
        .sample_rate(APP_RATE)
        .network_rate(DEVICE_RATE)
        .work_margin_ns(W_NS)
        .flow_slots(1)
        .app_buffer(bridge::chunk_ns(CHUNK as u64, APP_RATE), None)
}

/// The path, built as dlink builds it: the pump, and the app's ends of
/// its bridges.
pub(crate) struct Path {
    pub(crate) pump: Pump<Rated<App>>,
    pub(crate) ends: Ends,
}

/// The app's ends of the bridges and the counts the test reads.
pub(crate) struct Ends {
    pub(crate) pump_stats: Arc<PumpStats>,
    pub(crate) timing: Timing,
    /// The app's TX writes.
    pub(crate) tx_fifo: Arc<Fifo>,
    pub(crate) tx_stats: Arc<Stats>,
    /// The app's RX reads.
    pub(crate) rx_player: Player,
    pub(crate) rx_stats: Arc<Stats>,
    pub(crate) marks: Arc<Mutex<Vec<Mark>>>,
    pub(crate) _dir: tempfile::TempDir,
}

pub(crate) fn path() -> Path {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let cfg = config.device().expect("the soak's config");
    let buffer: AppBuffer = config.bridge_buffer().expect("an app buffer");
    let timing = Timing::of(&cfg);
    assert_eq!(timing.block, BLOCK);
    assert_eq!(timing.margin, to_samples(W_NS, DEVICE_RATE));

    let tx_fifo = Arc::new(bridge::fifo_for(CHANNELS, APP_RATE));
    let tx_stats = Arc::new(Stats::default());
    let tx = Player::new(
        Arc::clone(&tx_fifo),
        APP_RATE,
        APP_RATE,
        buffer,
        Arc::clone(&tx_stats),
    )
    .unwrap();
    let rx_fifo = Arc::new(bridge::fifo_for(CHANNELS, APP_RATE));
    let rx_stats = Arc::new(Stats::default());
    let rx_player = Player::new(
        Arc::clone(&rx_fifo),
        APP_RATE,
        APP_RATE,
        buffer,
        Arc::clone(&rx_stats),
    )
    .unwrap();
    let marks = Arc::new(Mutex::new(Vec::new()));
    let app = App {
        tx,
        out: vec![0.0; 4 * CHUNK * CHANNELS],
        rx: rx_fifo,
        marks: Arc::clone(&marks),
    };
    let rated = Rated::new(
        app,
        DEVICE_RATE,
        APP_RATE,
        BLOCK as usize,
        CHANNELS,
        CHANNELS,
    )
    .unwrap();

    let rings: Arc<[Ring]> = (0..cfg.rx.len()).map(|_| Ring::new()).collect();
    let rx = Arc::new(
        RxShared::with_slots(
            DEVICE_RATE,
            std::net::Ipv4Addr::LOCALHOST,
            Arc::clone(&rings),
            None,
            1,
        )
        .unwrap(),
    );
    let tx_shared = Arc::new(
        TxShared::new(
            cfg.clone(),
            std::net::Ipv4Addr::LOCALHOST,
            rings,
            1,
            &[0, 1],
        )
        .unwrap(),
    );
    let pump_stats = Arc::new(PumpStats::new(timing, DEVICE_RATE));
    let pump = Pump::new(
        rated,
        Map {
            rx: vec![0, 1],
            tx: vec![0, 1],
        },
        timing,
        24,
        rx,
        tx_shared,
        Arc::clone(&pump_stats),
    );
    Path {
        pump,
        ends: Ends {
            pump_stats,
            timing,
            tx_fifo,
            tx_stats,
            rx_player,
            rx_stats,
            marks,
            _dir: dir,
        },
    }
}

/// The app's tick: `CHUNK` frames of a 1 kHz tone written to TX, `CHUNK`
/// frames read from RX.
pub(crate) struct Ticker {
    pub(crate) written: u64,
    pub(crate) rx_out: Vec<f64>,
}

impl Ticker {
    pub(crate) fn new() -> Self {
        Ticker {
            written: 0,
            rx_out: vec![0.0; CHUNK * CHANNELS],
        }
    }

    pub(crate) fn tick(&mut self, p: &mut Ends) {
        let at = self.written;
        p.tx_fifo.push_with(CHUNK, |f, _| {
            0.25 * (TAU * 1000.0 * (at + f as u64) as f64 / f64::from(APP_RATE)).sin()
        });
        self.written += CHUNK as u64;
        p.rx_player.fill_with(&mut self.rx_out, CHANNELS, |x| x);
    }
}

/// The bridges' counts at one moment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Counts {
    pub(crate) at_ns: u64,
    pub(crate) tx_underruns: u64,
    pub(crate) tx_recentres: u64,
    /// The TX player's resyncs (a block start the pump skipped, dstack #303).
    pub(crate) tx_resyncs: u64,
    pub(crate) rx_underruns: u64,
    pub(crate) rx_recentres: u64,
    pub(crate) tx_ppm: f64,
    pub(crate) rx_ppm: f64,
}

impl Counts {
    pub(crate) fn of(p: &Ends, at_ns: u64) -> Self {
        Counts {
            at_ns,
            tx_underruns: p.tx_stats.underruns(),
            tx_recentres: p.tx_stats.recentres(),
            tx_resyncs: p.tx_stats.resyncs(),
            rx_underruns: p.rx_stats.underruns(),
            rx_recentres: p.rx_stats.recentres(),
            tx_ppm: p.tx_stats.ppm(),
            rx_ppm: p.rx_stats.ppm(),
        }
    }

    /// The TX side's gap counters (dropouts and re-centres; a floor deficit
    /// counts in both, so only a change of the sum is read, as one gap).
    pub(crate) fn tx_sum(&self) -> u64 {
        self.tx_underruns + self.tx_recentres
    }

    /// The RX side's.
    pub(crate) fn rx_sum(&self) -> u64 {
        self.rx_underruns + self.rx_recentres
    }
}
