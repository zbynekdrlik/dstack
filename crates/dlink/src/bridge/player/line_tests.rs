// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #303: the depth on the producer's line ([`producer_line`]) and a
//! steady chunked writer, the case the sampled depth aliased on.

use super::*;

use std::sync::atomic::AtomicI64;

use crate::bridge::chunk_ns;

const RATE: u32 = 48_000;
const CHUNK: usize = 1024;
const CALL: usize = 64;

/// Two frame pairs equal to a millionth of a frame.
fn close(got: (f64, f64), want: (f64, f64)) {
    assert!(
        (got.0 - want.0).abs() < 1e-6 && (got.1 - want.1).abs() < 1e-6,
        "{got:?} {want:?}"
    );
}

#[test]
fn the_line_runs_from_the_last_write_at_the_nominal_rate() {
    let stamp = Stamp {
        at_ns: 1_000_000_000,
        written: 3_000,
        arrived: 3_100,
        writes: 4,
    };
    // The mean write: 775 frames, half of it 387.5; its period 16.146 ms.
    let rate = f64::from(RATE);
    // At the write: the stored frames less half a write.
    close(
        producer_line(stamp, 1_000_000_000, rate, 1_000),
        (1_612.5, 2_712.5),
    );
    // 10 ms on: 480 frames more.
    close(
        producer_line(stamp, 1_010_000_000, rate, 1_000),
        (2_092.5, 3_192.5),
    );
    // A clock read before the write's stamp: no time since it.
    close(
        producer_line(stamp, 999_000_000, rate, 1_000),
        (1_612.5, 2_712.5),
    );
    // A late write: the line holds one mean write ahead, never more.
    close(
        producer_line(stamp, 1_500_000_000, rate, 1_000),
        (2_387.5, 3_487.5),
    );
    // A reader past the stamp (a write landing between the reads).
    close(
        producer_line(stamp, 1_000_000_000, rate, 3_200),
        (-587.5, 2_712.5),
    );
}

/// A writer of 1024 frames on its own clock, `drift_ppm` fast, with no
/// jitter at all, and a caller at 64 frames a call naming its starts, on
/// one simulated clock.
struct Steady {
    now: Arc<AtomicI64>,
    fifo: Arc<Fifo>,
    player: Player,
    out: Vec<f64>,
    write_ns: f64,
    writes: u64,
    calls: u64,
}

impl Steady {
    fn new(drift_ppm: f64) -> Self {
        let now = Arc::new(AtomicI64::new(0));
        let read = Arc::clone(&now);
        let fifo =
            Arc::new(fifo_for(1, RATE).with_clock(Arc::new(move || read.load(Ordering::Relaxed))));
        let buffer = AppBuffer::new(chunk_ns(CHUNK as u64, RATE), chunk_ns(128, 96_000));
        let player = Player::new(Arc::clone(&fifo), RATE, RATE, buffer, Arc::default()).unwrap();
        Steady {
            now,
            fifo,
            player,
            out: vec![0.0; CALL],
            write_ns: CHUNK as f64 * 1e9 / (f64::from(RATE) * (1.0 + drift_ppm * 1e-6)),
            writes: 0,
            calls: 0,
        }
    }

    /// The writes due by the next call, each at its own time, then the call.
    fn call(&mut self) {
        let call_ns = (self.calls * CALL as u64) as f64 * 1e9 / f64::from(RATE);
        while (self.writes + 1) as f64 * self.write_ns <= call_ns {
            self.writes += 1;
            let at = (self.writes as f64 * self.write_ns).round() as i64;
            self.now.store(at, Ordering::Relaxed);
            self.fifo.push_with(CHUNK, |_, _| 0.25);
        }
        self.now.store(call_ns.round() as i64, Ordering::Relaxed);
        self.player
            .fill_at(Some(self.calls * CALL as u64), &mut self.out, 1, |x| x);
        self.calls += 1;
    }

    /// `seconds` of calls; the ratio's correction each second.
    fn run(&mut self, seconds: u64) -> Vec<f64> {
        let per_second = u64::from(RATE) / CALL as u64;
        (0..seconds)
            .map(|_| {
                for _ in 0..per_second {
                    self.call();
                }
                self.player.stats().ppm()
            })
            .collect()
    }
}

/// A steady writer 100 ppm fast (its write instants cross a converter
/// call every ~27 s): on the producer's line the correction holds the
/// drift every second of the last minute. (On the sampled depth it held
/// still and then stepped at each crossing: -117.7 ppm at 90 s.)
#[test]
fn a_steady_writer_on_the_line_holds_its_rate() {
    let mut s = Steady::new(100.0);
    let ppm = s.run(150);
    for (i, p) in ppm.iter().enumerate().skip(90) {
        assert!((p + 100.0).abs() < 3.0, "second {i}: {p} ppm");
    }
    let stats = s.player.stats();
    assert_eq!(
        (stats.underruns(), stats.recentres(), stats.resyncs()),
        (0, 0, 0)
    );
}

/// A stamp read that meets a write in progress on every try (its writer
/// preempted mid-stamp) skips that observation: the player stays on the
/// line, its writer schedule is kept, and the ratio does not move.
#[test]
fn a_stamp_in_progress_skips_one_observation() {
    let mut s = Steady::new(6.0);
    s.run(5);
    assert!(s.player.writer_on_line);
    // Up to a converter call: the next call converts.
    while s.player.pos != BLOCK {
        s.call();
    }
    let (writer, ppm) = (s.player.writer, s.player.stats().ppm());
    let consumed = s.player.consumed();
    s.fifo.hold_stamp(true);
    s.call();
    s.fifo.hold_stamp(false);
    assert!(s.player.consumed() > consumed, "a block was converted");
    assert!(s.player.writer_on_line);
    assert_eq!(s.player.writer, writer);
    assert_eq!(s.player.stats().ppm(), ppm);
    s.run(1);
    assert_eq!(s.player.stats().underruns(), 0);
}

/// Only a caller that names its starts reads the FIFO's clock:
/// [`Player::fill_with`] (an output callback) keeps the sampled depth.
#[test]
fn only_a_caller_naming_its_starts_reads_the_clock() {
    let reads = Arc::new(AtomicI64::new(0));
    let count = Arc::clone(&reads);
    let fifo = Arc::new(
        fifo_for(1, RATE).with_clock(Arc::new(move || count.fetch_add(1, Ordering::Relaxed))),
    );
    let buffer = AppBuffer::new(chunk_ns(CHUNK as u64, RATE), chunk_ns(128, 96_000));
    let mut player = Player::new(Arc::clone(&fifo), RATE, RATE, buffer, Arc::default()).unwrap();
    let mut out = vec![0.0; CALL];
    // A write every 16 calls (1024 frames), two ahead.
    let mut pushed = 0;
    for k in 0..200 {
        if k % 16 == 0 {
            let ahead = if k == 0 { 3 } else { 1 };
            for _ in 0..ahead {
                fifo.push_with(CHUNK, |_, _| 0.25);
                pushed += 1;
            }
        }
        player.fill_with(&mut out, 1, |x| x);
    }
    assert!(!player.stats().priming(), "it runs");
    assert_eq!(player.stats().underruns(), 0);
    assert_eq!(reads.load(Ordering::Relaxed), pushed, "one read a write");
    let pushed = reads.load(Ordering::Relaxed);
    player.fill_at(Some(0), &mut out, 1, |x| x);
    player.fill_at(Some(CALL as u64), &mut out, 1, |x| x);
    assert!(reads.load(Ordering::Relaxed) > pushed);
}
