// SPDX-License-Identifier: MIT OR Apache-2.0

//! The test's sending device as a bridge test's verdict reads it (dstack
//! #436): its TX schedule restarts with their lines ([`super::restarts`],
//! dstack #401) and its pump's restarts with the silence they sent.
//! Shared through `#[path]` by the consumer witness (`consumer/witness.rs`:
//! `tests/rx_pull.rs`, dstack-asio's `tests/to_asio_pull.rs`) and
//! `tests/rx_pull_held.rs`, each also declaring `tests/tx_restart/
//! restarts.rs` as `mod restarts` and this file as `mod sender`. Each
//! verdict keeps its own rule over these counts.
//!
//! A pump restart after a wake more than W late leaves the sender's
//! engine ring's positions unwritten, and its TX thread sends them as
//! silence (the TX lanes' `holes`): whole packets, so no RX counter sees
//! them, and they play as samples off with no underrun and no ring hole
//! (plain at dev1 load ~24 `rx_pull_held` read that `OURS` 8 runs of 9).
//! The silence is counted when the sender SENDS it and played
//! [`SILENCE_DELAY`] later at most, so a window's pump counts open that
//! much before it ([`Noted`]).

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dstack::media::tx::{MAX_FLOWS, TxShared};
use dstack::stream::PumpStats;

use super::restarts::{self, Restarts};

/// The samples off the level a sender's gap may cost past its own samples
/// where a verdict bounds them by the silence of late pump restarts: the
/// player's converter rings at both edges of it. Measured under `chrt -f
/// 10` on dev1 (19.11.2026) at most 14 a pump restart (`rx_pull` 10340 off
/// for 10 restarts' 10240 samples of silence, `to_asio_pull` 10380,
/// `rx_pull_held` 10340, at most 10), so 32 is room, not a second cause.
/// Only a gap that left evidence has edges: a pump restart that sent
/// silence, a TX restart line beside holes never written. A hole judged
/// whole costs more (`rx_pull`: one 12 ms TX sleep's 640 holes, 677 to
/// 907 off), so the consumer witness bounds holes alone by the restarts
/// only (#401's rule).
pub(crate) const GAP_EDGES: u64 = 32;

/// How long after the sender sends a sample the consumer plays it, at
/// most: the receiver's latency and the FIFO's wait. Measured in
/// `rx_pull` (dev1 under `chrt -f 10`, 19.11.2026): the receiver plays at
/// 592 samples (12.3 ms) and the FIFO's mean wait is ~14.3 ms, ~27 ms in
/// all; `to_asio_pull`'s FIFO waits less (an 8 ms target), `rx_pull_held`'s
/// card buffer more (up to 1056 frames, 22 ms). With the pump counts taken
/// at the window's first callback, a sender pump restart ~25 ms before the
/// open read 626 samples off past `rx_pull`'s silence (9842 off, 9 late
/// restarts, 9216 silence) and `OURS` in 2 runs of 3.
pub(crate) const SILENCE_DELAY: Duration = Duration::from_millis(50);

/// The sender's pump over a window, or since its start.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Pump {
    /// Its restarts, and those after a wake more than W late
    /// (`rebootstraps_late`).
    pub(crate) restarts: u64,
    pub(crate) late: u64,
    /// The samples its TX thread sent as silence: engine ring positions
    /// its pump left unwritten (the TX lanes' `holes`).
    pub(crate) silence: u64,
}

/// The sending device a verdict reads: its TX side, its pump and its rate.
pub(crate) struct Sender {
    pub(crate) tx: Arc<TxShared>,
    pub(crate) pump: Arc<PumpStats>,
    pub(crate) rate: u32,
}

/// The sender's counts at a window's open.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SenderAt {
    opened: restarts::Opened,
    pump: Pump,
}

impl Sender {
    /// Its pump's restarts and the silence its TX lanes sent, since its
    /// start (atomics and seqlocks, no lock).
    pub(crate) fn pump_now(&self) -> Pump {
        let view = self.pump.view();
        Pump {
            restarts: view.rebootstraps,
            late: view.rebootstraps_late,
            silence: (0..MAX_FLOWS).map(|s| self.tx.lane(s).holes).sum(),
        }
    }

    /// A window opens now: its TX restarts from now (one short lock on
    /// the line log, `restarts::open`), its pump's from `pump`
    /// ([`Noted::delayed`]).
    pub(crate) fn open(&self, pump: Pump) -> SenderAt {
        SenderAt {
            opened: restarts::open(&self.tx),
            pump,
        }
    }

    /// The TX and pump restarts since the window opened `at`.
    pub(crate) fn since(&self, at: &SenderAt) -> (Restarts, Pump) {
        let now = self.pump_now();
        let pump = Pump {
            restarts: now.restarts.saturating_sub(at.pump.restarts),
            late: now.late.saturating_sub(at.pump.late),
            silence: now.silence.saturating_sub(at.pump.silence),
        };
        (at.opened.close(&self.tx, self.rate), pump)
    }
}

/// The sender's pump counts noted at the consumer's calls, oldest first,
/// kept so the first is the latest at least [`SILENCE_DELAY`] old (or the
/// oldest): a few entries (~6 at 10 ms calls, ~10 at 5.33 ms).
#[derive(Debug, Default)]
pub(crate) struct Noted {
    seen: VecDeque<(Instant, Pump)>,
}

impl Noted {
    /// Notes the sender's pump at a call done at `now`.
    pub(crate) fn note(&mut self, now: Instant, sender: &Sender) {
        self.seen.push_back((now, sender.pump_now()));
        keep_delayed(&mut self.seen, now, SILENCE_DELAY);
    }

    /// The counts a window opening now starts from: [`SILENCE_DELAY`]
    /// before, or the oldest noted. Call it after a [`Noted::note`].
    pub(crate) fn delayed(&self) -> Pump {
        self.seen.front().expect("noted at this call").1
    }
}

/// Drops the counts of `seen` (oldest first) older than the latest at
/// least `delay` before `now`, so the first is that one (or the oldest,
/// while none is that old).
fn keep_delayed<T>(seen: &mut VecDeque<(Instant, T)>, now: Instant, delay: Duration) {
    let Some(cutoff) = now.checked_sub(delay) else {
        return;
    };
    while seen.get(1).is_some_and(|&(at, _)| at <= cutoff) {
        seen.pop_front();
    }
}

#[cfg(test)]
mod sender_tests {
    use super::*;

    #[test]
    fn a_window_s_pump_counts_open_the_silence_s_delay_before_it() {
        // A call every 10 ms: at 100 ms the first kept is the one at 50 ms
        // (exactly the delay old), at 105 ms still that one (the next, at
        // 60 ms, is 45 ms old), at 110 ms the one at 60 ms.
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut seen = VecDeque::new();
        for ms in (0..=100).step_by(10) {
            seen.push_back((at(ms), ms));
            keep_delayed(&mut seen, at(ms), SILENCE_DELAY);
        }
        assert_eq!(seen.front().map(|&(_, ms)| ms), Some(50), "{seen:?}");
        assert_eq!(seen.len(), 6, "{seen:?}");
        keep_delayed(&mut seen, at(105), SILENCE_DELAY);
        assert_eq!(seen.front().map(|&(_, ms)| ms), Some(50), "{seen:?}");
        seen.push_back((at(110), 110));
        keep_delayed(&mut seen, at(110), SILENCE_DELAY);
        assert_eq!(seen.front().map(|&(_, ms)| ms), Some(60), "{seen:?}");
        // While none is that old, the oldest stays first.
        let mut young = VecDeque::from([(at(0), 0), (at(10), 10)]);
        keep_delayed(&mut young, at(20), SILENCE_DELAY);
        assert_eq!(young.front().map(|&(_, ms)| ms), Some(0), "{young:?}");
    }
}
