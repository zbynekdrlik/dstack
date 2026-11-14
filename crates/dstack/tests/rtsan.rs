//! The engine's pump block under RealtimeSanitizer. The `rtsan` CI job
//! builds this test with a pinned nightly, `-Zsanitizer=realtime`,
//! `-Zbuild-std` and `--cfg iem_rtsan`; `stream::Pump::block` and
//! `dstack_audio_io::hist::PeriodHist::record` then carry
//! `#[sanitize(realtime = "nonblocking")]`, and any allocation, lock or
//! blocking syscall inside them aborts the test binary. That build also
//! proves both crates compile with the attribute, which an application's
//! rtsan job (iemmixer's) needs. Built normally (every other job), the
//! same workload runs without the sanitizer.
#![cfg_attr(iem_rtsan, feature(sanitize))]

use std::net::Ipv4Addr;
use std::process::Command;
use std::sync::Arc;

use dstack::device::DeviceConfig;
use dstack::media::ring::Ring;
use dstack::media::rx::{RxPlan, RxRoute, RxShared};
use dstack::media::source::Source;
use dstack::media::tx::TxShared;
use dstack::stream::{BLOCK, Map, Pump, PumpStats, Timing, WORK_MARGIN};
use dstack_audio_io::{Block, Process};

/// Ten years of a grandmaster's uptime at 96 kHz, as a position.
const T: u64 = 10 * 365 * 86_400 * 96_000;
const RATE: u32 = 96_000;
const INPUTS: usize = 32;
const OUTPUTS: usize = 24;
const PROBE: &str = "DSTACK_RTSAN_PROBE";

/// Output j is input j, through a stack buffer.
struct Copy;

/// The pump's blocks that carried a play time (dstack #295).
static STAMPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl Process for Copy {
    fn process(&mut self, block: &mut Block<'_>) {
        if block.play_at().is_some() {
            STAMPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let mut x = [0.0f64; BLOCK as usize];
        for ch in 0..block.outputs() {
            x.copy_from_slice(block.input(ch));
            block.output(ch).copy_from_slice(&x);
        }
    }
}

#[test]
fn the_pump_block_is_realtime_safe() {
    let mut cfg = DeviceConfig::from_toml(include_str!("../../../config/test-device.toml"))
        .expect("config/test-device.toml");
    cfg.tx = (1..=OUTPUTS).map(|n| format!("TX {n}")).collect();
    assert_eq!(cfg.rx.len(), INPUTS);
    let rings: Arc<[Ring]> = (0..INPUTS).map(|_| Ring::new()).collect::<Vec<_>>().into();
    for (rx, ring) in rings.iter().enumerate() {
        let v: Vec<i32> = (T - 1_000..T + 11_000)
            .map(|p| (((p + rx as u64) % 65_536) << 8) as i32)
            .collect();
        ring.write_at(T - 1_000, &v);
    }
    let rx = Arc::new(RxShared::new(RATE, Ipv4Addr::LOCALHOST, Arc::clone(&rings)).unwrap());
    rx.publish(
        |_| RxPlan::default(),
        |c| {
            Some(RxRoute {
                flow: 0,
                slot: c % 8,
            })
        },
    );
    let engine: Vec<usize> = (0..OUTPUTS).collect();
    let tx = Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, rings, 1, &engine).unwrap());
    for c in 0..OUTPUTS {
        assert!(tx.set_source(c, Source::Engine));
    }
    let timing = Timing {
        block: BLOCK,
        playout: 96,
        margin: WORK_MARGIN,
    };
    let stats = Arc::new(PumpStats::new(timing, RATE));
    let map = Map {
        rx: (0..INPUTS).collect(),
        tx: engine,
    };
    let mut pump = Pump::new(Copy, map, timing, 24, rx, tx, Arc::clone(&stats));
    let mut now = T;
    // A pair read first (dstack #295), so each block computes its play
    // time inside the real-time part.
    let media = dstack::clock::ManualClock::new(dstack::media::tx::ns_of(T, RATE));
    assert!(pump.read_clocks(&media, || 7_000_000_000).is_some());
    for _ in 0..300 {
        now = pump.media(now, 0);
    }
    let v = stats.view();
    assert_eq!((v.blocks, v.late, v.rebootstraps), (300, 0, 0), "{v:?}");
    assert_eq!(
        STAMPED.load(std::sync::atomic::Ordering::Relaxed),
        300,
        "every block stamped"
    );
    let total = |h: &[(u32, u64)]| h.iter().map(|e| e.1).sum::<u64>();
    assert_eq!(
        total(&stats.histograms().process),
        300,
        "the process histogram recorded every block"
    );
}

/// Allocates in a real-time context: a violation under the sanitizer.
#[cfg_attr(iem_rtsan, sanitize(realtime = "nonblocking"))]
fn nonblocking_alloc(n: usize) -> usize {
    vec![0u8; n].len()
}

/// Run by `rtsan_detects_a_violation` in a child process; on its own it
/// does nothing.
#[test]
fn probe() {
    if std::env::var_os(PROBE).is_some() {
        assert_eq!(nonblocking_alloc(64), 64);
    }
}

#[test]
fn rtsan_detects_a_violation() {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "probe", "--nocapture", "--test-threads", "1"])
        .env(PROBE, "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if cfg!(iem_rtsan) {
        assert!(!out.status.success(), "the allocation went unnoticed");
        assert!(stderr.contains("RealtimeSanitizer"), "{stderr}");
    } else {
        assert!(out.status.success(), "{stderr}");
    }
}
