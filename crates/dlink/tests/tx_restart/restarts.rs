// SPDX-License-Identifier: MIT OR Apache-2.0

//! A sender's TX schedule restarts in a judged window, and whether the
//! runner caused them (dstack #401). Shared through `#[path]` by the
//! whole-device bridge tests: `tests/tx_pull.rs`, `tests/tx_pull_counts.rs`
//! and `tests/rx_pull.rs` here, dstack-asio's `tests/from_asio_pull.rs` and
//! `tests/to_asio_pull.rs`.
//!
//! The TX thread starts a flow's schedule again when a pass reads the
//! clock more than the flow's latency (`max_lag`) past a packet's send
//! time; the packets it skipped never reach the receiver (samples off with
//! no TX hole and no bridge underrun, v0.6.178 tag CI's coverage job: 480
//! off, the TX lane's `rebootstraps: 1`, `tx_thread.oversleep_max_us:
//! 9282`). Its restart line (`the TX schedule starts again`, dstack #359)
//! carries `oversleep_us`, how late the sleep before that pass returned:
//! when that alone is the flow's latency or more, the scheduler did not
//! run the TX thread past the bound, the runner's (the ci-peer gate's rule
//! for a pump re-bootstrap, dstack #339). The status's
//! `tx_thread.oversleep_max_us` is a peak since the start and cannot tell
//! which restart it was (dstack #370), so each line is read on its own.
//!
//! [`listen`] sets one global subscriber per test binary that records
//! only that line (every other callsite is never enabled); a window
//! [`open`]s on the sender's TX side and [`Opened::close`]s into
//! [`Restarts`]: the restarts the TX lanes counted and the lines logged
//! meanwhile. A test binary runs one sending device at a time, so every
//! line in the window is its sender's.

use std::fmt::{self, Debug};
use std::sync::{Mutex, MutexGuard, Once, PoisonError};

use dstack::media::tx::{MAX_FLOWS, TxShared};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::Interest;
use tracing::{Event, Level, Metadata, Subscriber};

/// The restart line's message (`dstack::media::tx::Schedule::due_after`).
const LINE: &str = "the TX schedule starts again";

/// One restart line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Restart {
    /// The pass's lag past the packet's send time, samples.
    pub(crate) lag: i64,
    /// The schedule's bound: the flow's latency, samples.
    pub(crate) max_lag: u64,
    /// How late the TX thread's sleep before the pass returned, µs;
    /// `None` when unmeasured (`-`: no sleep before the pass).
    pub(crate) oversleep_us: Option<u64>,
}

impl Restart {
    /// Whether the sleep before the restart returned the flow's latency
    /// late or more, at the device's `rate`: the TX thread was not run.
    pub(crate) fn slept_past(&self, rate: u32) -> bool {
        self.oversleep_us.is_some_and(|us| {
            u128::from(us) * u128::from(rate) >= u128::from(self.max_lag) * 1_000_000
        })
    }
}

/// The lines logged, each with the name of the thread that logged it.
static LINES: Mutex<Vec<(String, Restart)>> = Mutex::new(Vec::new());

/// The TX thread's name and its relay's (`dstack::media::tx::spawn`): a
/// window takes only their lines, never a unit test's.
const TX_THREAD: &str = "dstack-tx";

fn lines() -> MutexGuard<'static, Vec<(String, Restart)>> {
    LINES.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The global subscriber: the restart line only.
struct Lines;

fn is_restart(meta: &Metadata<'_>) -> bool {
    meta.is_event()
        && *meta.level() == Level::WARN
        && meta.target().starts_with("dstack::media::tx")
}

/// A restart line's fields.
#[derive(Default)]
struct Fields {
    message: bool,
    restart: Restart,
}

impl Visit for Fields {
    fn record_i64(&mut self, field: &Field, value: i64) {
        if field.name() == "lag" {
            self.restart.lag = value;
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "max_lag" {
            self.restart.max_lag = value;
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        let text = format!("{value:?}");
        match field.name() {
            "message" => self.message = text == LINE,
            "oversleep_us" => self.restart.oversleep_us = text.parse().ok(),
            _ => {}
        }
    }
}

impl Subscriber for Lines {
    fn register_callsite(&self, meta: &'static Metadata<'static>) -> Interest {
        if is_restart(meta) {
            Interest::always()
        } else {
            Interest::never()
        }
    }
    fn enabled(&self, meta: &Metadata<'_>) -> bool {
        is_restart(meta)
    }
    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        if fields.message {
            let thread = std::thread::current().name().unwrap_or("").to_string();
            lines().push((thread, fields.restart));
        }
    }
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

/// Sets the global subscriber that records the restart lines, once a
/// test binary; call it before the sending device starts.
pub(crate) fn listen() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        tracing::subscriber::set_global_default(Lines)
            .expect("no other global subscriber in the test binary");
    });
}

/// Every TX lane's counted restarts.
fn counted(tx: &TxShared) -> u64 {
    (0..MAX_FLOWS).map(|s| tx.lane(s).rebootstraps).sum()
}

/// A window open on a sender's TX side.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Opened {
    counted: u64,
    mark: usize,
}

/// Opens a window on `tx`, the sending device's TX side.
pub(crate) fn open(tx: &TxShared) -> Opened {
    listen();
    Opened {
        counted: counted(tx),
        mark: lines().len(),
    }
}

impl Opened {
    /// The window's restarts on `tx`, a device at `rate` Hz.
    pub(crate) fn close(&self, tx: &TxShared, rate: u32) -> Restarts {
        Restarts {
            counted: counted(tx).saturating_sub(self.counted),
            lines: lines()
                .get(self.mark..)
                .unwrap_or_default()
                .iter()
                .filter(|(thread, _)| thread.starts_with(TX_THREAD))
                .map(|&(_, r)| r)
                .collect(),
            rate,
            oversleep_max_us: tx.tx_thread().oversleep_max_us,
        }
    }
}

/// A sender's TX schedule restarts in one window.
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct Restarts {
    /// The restarts its TX lanes counted (a lag, a jump, a re-anchor).
    pub(crate) counted: u64,
    /// The restart lines logged in the window (a re-anchor logs none).
    pub(crate) lines: Vec<Restart>,
    /// The device's rate, Hz (the lines' samples).
    pub(crate) rate: u32,
    /// The TX thread's largest oversleep since its start, µs (the status
    /// `tx_thread`; for the message only).
    pub(crate) oversleep_max_us: u64,
}

impl Restarts {
    /// Whether the TX schedule restarted at all.
    pub(crate) fn any(&self) -> bool {
        self.counted > 0
    }

    /// Whether every counted restart came with a line whose sleep
    /// returned the flow's latency late or more: the runner did not run
    /// the TX thread. A restart without such a line (a pass that ran long,
    /// a re-anchor, or no restart at all) is not the runner's.
    pub(crate) fn runner(&self) -> bool {
        self.any()
            && self.lines.len() as u64 >= self.counted
            && self.lines.iter().all(|r| r.slept_past(self.rate))
    }
}

impl Debug for Restarts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let lines: Vec<String> = self
            .lines
            .iter()
            .map(|r| {
                let us = r.oversleep_us.map_or("-".into(), |us| us.to_string());
                format!("lag {} max_lag {} oversleep_us {us}", r.lag, r.max_lag)
            })
            .collect();
        write!(
            f,
            "{} TX restarts at {} Hz, lines {lines:?}, tx_thread.oversleep_max_us {}",
            self.counted, self.rate, self.oversleep_max_us
        )
    }
}

#[cfg(test)]
mod restarts_tests {
    use super::*;

    /// A restart line at 48 kHz with a 9 ms latency (432 samples).
    fn line(oversleep_us: Option<u64>) -> Restart {
        Restart {
            lag: 440,
            max_lag: 432,
            oversleep_us,
        }
    }

    fn restarts(counted: u64, lines: &[Restart]) -> Restarts {
        Restarts {
            counted,
            lines: lines.to_vec(),
            rate: 48_000,
            oversleep_max_us: 0,
        }
    }

    #[test]
    fn a_restart_is_the_runner_s_when_its_sleep_returned_the_latency_late() {
        // 432 samples at 48 kHz: 9000 us exactly, and one µs under it.
        assert!(line(Some(9_000)).slept_past(48_000));
        assert!(!line(Some(8_999)).slept_past(48_000));
        assert!(!line(None).slept_past(48_000));
        assert!(restarts(1, &[line(Some(9_282))]).runner());
        assert!(restarts(2, &[line(Some(9_282)), line(Some(12_000))]).runner());
    }

    #[test]
    fn a_restart_without_a_late_sleep_is_not_the_runner_s() {
        // No restart: nothing to blame on the TX thread.
        assert!(!restarts(0, &[]).runner());
        // A sleep on time (the pass ran long), unmeasured, or one of two.
        assert!(!restarts(1, &[line(Some(500))]).runner());
        assert!(!restarts(1, &[line(None)]).runner());
        assert!(!restarts(2, &[line(Some(9_282)), line(Some(500))]).runner());
        // A counted restart with no line (a re-anchor).
        assert!(!restarts(2, &[line(Some(9_282))]).runner());
    }

    /// The lines this thread logged since `mark`.
    fn own_since(mark: usize) -> Vec<Restart> {
        let me = std::thread::current().name().unwrap_or("").to_string();
        lines()[mark..]
            .iter()
            .filter(|(thread, _)| *thread == me)
            .map(|&(_, r)| r)
            .collect()
    }

    #[test]
    fn the_subscriber_records_the_restart_line_s_fields() {
        listen();
        let mark = lines().len();
        // The line as `Schedule::due_after` writes it, from a target under
        // the TX module; another warning and another target are not taken.
        tracing::warn!(
            target: "dstack::media::tx::schedule",
            lag = 440_i64,
            max_lag = 432_u64,
            oversleep_us = %9_282,
            last_pass_us = %"-",
            "the TX schedule starts again"
        );
        tracing::warn!(target: "dstack::media::tx::schedule", lag = 1_i64, "another line");
        tracing::warn!(target: "dstack::other", "the TX schedule starts again");
        tracing::warn!(
            target: "dstack::media::tx::schedule",
            lag = 7_i64,
            max_lag = 432_u64,
            oversleep_us = %"-",
            "the TX schedule starts again"
        );
        assert_eq!(
            own_since(mark),
            [
                Restart {
                    lag: 440,
                    max_lag: 432,
                    oversleep_us: Some(9_282)
                },
                Restart {
                    lag: 7,
                    max_lag: 432,
                    oversleep_us: None
                }
            ]
        );
    }
}
