// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #303: the resync on a skipped block start and the re-prime on a
//! chunked producer's schedule (OBS's shape: 1024-frame writes, 64-frame
//! calls, 48 kHz both sides).

use super::*;

use crate::bridge::chunk_ns;

const RATE: u32 = 48_000;
const CHUNK: usize = 1024;
const CALL: usize = 64;
/// OBS's tick jitter each side, frames (2 ms).
const JITTER: u64 = 96;

/// OBS's buffer: its chunk and one pump block (128 frames at 96 kHz).
fn buffer() -> AppBuffer {
    AppBuffer::new(chunk_ns(CHUNK as u64, RATE), chunk_ns(128, 96_000))
}

/// The producer writes a chunk every chunk period of its own clock; the
/// player is called every 64 frames with the block's start.
struct Obs {
    fifo: Arc<Fifo>,
    player: Player,
    /// The caller's frames so far (its clock) and the producer's.
    frames: u64,
    written: u64,
    /// The producer writes while true.
    writing: bool,
    /// The next write's offset, 0 ..= 2 x [`JITTER`] frames, and its
    /// generator's state.
    jitter: u64,
    seed: u64,
    out: Vec<f64>,
    /// Depth after each call, summed, and the count.
    depth_sum: f64,
    depth_n: f64,
}

impl Obs {
    fn new() -> Self {
        let fifo = Arc::new(fifo_for(1, RATE));
        let player = Player::new(Arc::clone(&fifo), RATE, RATE, buffer(), Arc::default()).unwrap();
        Obs {
            fifo,
            player,
            frames: 0,
            written: 0,
            writing: true,
            jitter: JITTER,
            seed: 0x303,
            out: vec![0.0; CALL],
            depth_sum: 0.0,
            depth_n: 0.0,
        }
    }

    /// The producer catches up to the caller's clock, a chunk at a time
    /// (after a stall: the chunks it owes, at once), each write up to 2 ms
    /// either side of its tick (OBS's jitter, a fixed seed).
    fn produce(&mut self) {
        while self.writing && self.written + CHUNK as u64 + JITTER <= self.frames + self.jitter {
            self.fifo.push_with(CHUNK, |_, _| 0.25);
            self.written += CHUNK as u64;
            self.seed ^= self.seed << 13;
            self.seed ^= self.seed >> 7;
            self.seed ^= self.seed << 17;
            self.jitter = self.seed % (2 * JITTER + 1);
        }
    }

    /// One call at the caller's clock.
    fn call(&mut self) {
        self.produce();
        self.player
            .fill_at(Some(self.frames), &mut self.out, 1, |x| x);
        self.frames += CALL as u64;
        self.depth_sum += self.fifo.len() as f64;
        self.depth_n += 1.0;
    }

    fn run(&mut self, seconds: f64) {
        for _ in 0..(seconds * f64::from(RATE) / CALL as f64) as u64 {
            self.call();
        }
    }

    /// The caller skips `calls` calls' time (the pump's rebootstrap): the
    /// producer writes on.
    fn skip(&mut self, calls: u64) {
        for _ in 0..calls {
            self.frames += CALL as u64;
            self.produce();
        }
    }

    /// The mean depth since the last call, ms.
    fn depth_ms(&mut self) -> f64 {
        let mean = self.depth_sum / self.depth_n / f64::from(RATE) * 1000.0;
        (self.depth_sum, self.depth_n) = (0.0, 0.0);
        mean
    }
}

#[test]
fn contiguous_starts_or_none_never_resync() {
    let mut s = Obs::new();
    s.run(5.0);
    let mut out = vec![0.0; CALL];
    s.player.fill_with(&mut out, 1, |x| x);
    let stats = s.player.stats();
    assert_eq!((stats.resyncs(), stats.underruns()), (0, 0));
    assert_eq!(s.player.skipped, 0);
}

/// Settled (the rate fit locked, past a minute), then eight blocks skipped
/// (#302's rebootstrap) and run on for `after_s`: the mean depth before,
/// over the 2 s just after, and the largest correction since, ppm.
fn skip_after_settling(starts: bool, after_s: f64) -> (Obs, f64, f64, f64) {
    let mut s = Obs::new();
    s.run(90.0);
    s.depth_ms();
    s.run(5.0);
    let before = s.depth_ms();
    s.skip(9);
    let mut out = vec![0.0; CALL];
    let mut worst: f64 = 0.0;
    let mut just_after = 0.0;
    let calls = (after_s * f64::from(RATE) / CALL as f64) as u64;
    for k in 0..calls {
        if starts {
            s.call();
        } else {
            s.produce();
            s.player.fill_with(&mut out, 1, |x| x);
            s.frames += CALL as u64;
            s.depth_sum += s.fifo.len() as f64;
            s.depth_n += 1.0;
        }
        worst = worst.max(s.player.stats().ppm().abs());
        if k + 1 == 2 * u64::from(RATE) / CALL as u64 {
            just_after = s.depth_ms();
        }
    }
    (s, before, just_after, worst)
}

/// One resync, no underrun, no re-centre: the excess the producer wrote
/// meanwhile is skipped at once, so the depth is back at its mean right
/// after the skip, and the ratio stays within the bound.
#[test]
fn a_skipped_block_start_resyncs_once_and_skips_the_excess() {
    let (s, before, just_after, worst) = skip_after_settling(true, 30.0);
    let stats = s.player.stats();
    assert_eq!(
        (stats.resyncs(), stats.underruns(), stats.recentres()),
        (1, 0, 0)
    );
    assert_eq!(
        s.player.skipped,
        9 * CALL as u64,
        "the clock counts the skip"
    );
    assert!(
        (just_after - before).abs() < 1.5,
        "mean depth {just_after} ms just after, {before} ms before"
    );
    assert!(
        worst <= LEVEL_BOUND_PPM + 0.5,
        "the ratio swung to {worst} ppm"
    );
}

/// The same skip without the starts (`fill_with`): the excess stays in
/// the FIFO, for the ratio to drain.
#[test]
fn without_the_starts_a_skip_stays_as_an_excess() {
    let (s, before, just_after, _) = skip_after_settling(false, 3.0);
    assert_eq!(s.player.stats().resyncs(), 0);
    assert!(
        just_after - before > 8.0,
        "mean depth {just_after} ms just after, {before} ms before"
    );
}

/// A start that goes back (a schedule started over) resyncs without
/// counting skipped time.
#[test]
fn a_start_that_goes_back_resyncs_and_skips_no_time() {
    let mut s = Obs::new();
    s.run(2.0);
    s.frames -= 10 * CALL as u64;
    s.call();
    assert_eq!(s.player.stats().resyncs(), 1);
    assert_eq!(s.player.skipped, 0);
}

/// A new player on the same stats counts its resyncs on.
#[test]
fn a_new_player_counts_on_its_stats_resyncs() {
    let mut s = Obs::new();
    s.run(1.0);
    s.skip(3);
    s.call();
    let stats = s.player.stats();
    assert_eq!(stats.resyncs(), 1);
    let mut next = Player::new(Arc::clone(&s.fifo), RATE, RATE, buffer(), stats).unwrap();
    let mut out = vec![0.0; CALL];
    next.fill_at(Some(0), &mut out, 1, |x| x);
    next.fill_at(Some(5 * CALL as u64), &mut out, 1, |x| x);
    assert_eq!(next.stats().resyncs(), 2);
}

#[test]
fn the_phased_level_puts_the_mean_on_the_target() {
    // Right after a write: the target and half the chunk and the call,
    // today's priming level.
    assert_eq!(Player::phased_level(1088, 1024, 1024, 64), 1088 + 544);
    // The next write 6 ms away: 288 frames to come before it.
    assert_eq!(
        Player::phased_level(1088, 1024, 288, 64),
        1088 + 288 + 32 - 512
    );
    assert_eq!(
        Player::phased_level(1088, 1024, 289, 64),
        1088 + 289 + 32 - 512
    );
    // A write due now: the sawtooth's bottom.
    assert_eq!(Player::phased_level(1088, 1024, 0, 64), 1088 + 32 - 512);
    // Never under 0.
    assert_eq!(Player::phased_level(0, 1024, 0, 0), 0);
}

/// A dropout mid-cycle (the producer stalls past the FIFO's depth, then
/// writes the chunks it owes at once): the re-prime starts at the depth
/// whose mean is the target on the producer's schedule, so the mean
/// after it is the target, not up to a chunk over it.
#[test]
fn a_re_prime_after_a_stall_starts_on_the_producer_s_schedule() {
    let mut s = Obs::new();
    s.run(5.0);
    s.depth_ms();
    s.run(2.0);
    let before = s.depth_ms();
    // A stall of 40 ms that starts 4 ms (three calls) after a write.
    while s.frames % CHUNK as u64 != 3 * CALL as u64 {
        s.call();
    }
    s.writing = false;
    for _ in 0..(40 * 48 / CALL) {
        s.call();
    }
    s.writing = true;
    s.run(0.1);
    let stats = s.player.stats();
    assert_eq!(stats.underruns(), 1, "the stall emptied the FIFO once");
    assert!(!stats.priming());
    s.depth_ms();
    s.run(2.0);
    let after = s.depth_ms();
    assert!(
        (after - before).abs() < 1.0,
        "mean depth {after} ms after the re-prime, {before} ms before"
    );
}

/// A block start after blocks with none (free pacing, no media clock)
/// resyncs once: the schedule the starts name came back on another clock
/// (dstack #303 follow-up). Today the `None` forgot the last start, so the
/// return to the media clock never resynced. Its frames were played, so
/// no time is skipped.
#[test]
fn a_start_after_a_block_without_one_resyncs_once() {
    let mut s = Obs::new();
    s.run(2.0);
    let mut out = vec![0.0; CALL];
    s.produce();
    s.player.fill_at(None, &mut out, 1, |x| x);
    s.frames += CALL as u64;
    s.call();
    assert_eq!(s.player.stats().resyncs(), 1);
    assert_eq!(s.player.skipped, 0);
    // Contiguous starts after it: no other resync.
    s.run(1.0);
    assert_eq!(s.player.stats().resyncs(), 1);
}

/// Blocks without a start before the first one with a start never resync:
/// nothing was interrupted.
#[test]
fn a_first_start_after_blocks_without_one_does_not_resync() {
    let mut s = Obs::new();
    let mut out = vec![0.0; CALL];
    for _ in 0..10 {
        s.produce();
        s.player.fill_at(None, &mut out, 1, |x| x);
        s.frames += CALL as u64;
    }
    s.run(1.0);
    assert_eq!(s.player.stats().resyncs(), 0);
}
