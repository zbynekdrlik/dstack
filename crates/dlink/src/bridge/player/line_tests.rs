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
/// jitter at all, and a caller at 64 frames a call naming its starts
/// (`starts`) or not; returns the ratio's correction each second.
fn steady(drift_ppm: f64, seconds: u64, starts: bool) -> Vec<f64> {
    let now = Arc::new(AtomicI64::new(0));
    let read = Arc::clone(&now);
    let fifo =
        Arc::new(fifo_for(1, RATE).with_clock(Arc::new(move || read.load(Ordering::Relaxed))));
    let buffer = AppBuffer::new(chunk_ns(CHUNK as u64, RATE), chunk_ns(128, 96_000));
    let mut player = Player::new(Arc::clone(&fifo), RATE, RATE, buffer, Arc::default()).unwrap();
    let mut out = vec![0.0; CALL];
    let write_ns = CHUNK as f64 * 1e9 / (f64::from(RATE) * (1.0 + drift_ppm * 1e-6));
    let mut writes = 0u64;
    let mut ppm = Vec::new();
    let calls = seconds * u64::from(RATE) / CALL as u64;
    for k in 0..calls {
        let call_ns = (k * CALL as u64) as f64 * 1e9 / f64::from(RATE);
        while (writes + 1) as f64 * write_ns <= call_ns {
            writes += 1;
            now.store((writes as f64 * write_ns).round() as i64, Ordering::Relaxed);
            fifo.push_with(CHUNK, |_, _| 0.25);
        }
        now.store(call_ns.round() as i64, Ordering::Relaxed);
        let start = starts.then_some(k * CALL as u64);
        player.fill_at(start, &mut out, 1, |x| x);
        if (k + 1) % (u64::from(RATE) / CALL as u64) == 0 {
            ppm.push(player.stats().ppm());
        }
    }
    let stats = player.stats();
    assert_eq!(
        (stats.underruns(), stats.recentres(), stats.resyncs()),
        (0, 0, 0)
    );
    ppm
}

/// A steady writer 100 ppm fast (its write instants cross a converter
/// call every ~27 s): on the producer's line the correction holds the
/// drift every second of the last minute. On the sampled depth (no
/// starts) it held still and then stepped at each crossing.
#[test]
fn a_steady_writer_on_the_line_holds_its_rate() {
    let ppm = steady(100.0, 150, true);
    for (s, p) in ppm.iter().enumerate().skip(90) {
        assert!((p + 100.0).abs() < 3.0, "second {s}: {p} ppm");
    }
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
