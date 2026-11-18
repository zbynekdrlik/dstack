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
//! The bound is the latency the receiver plays the flow at: its own
//! stated latency when longer than the sender's (dstack #426, run
//! 44407677291 on windows-2025: a sleep 8.465 ms late restarted a schedule
//! bound at the sender's 8.36 ms while the receiver played at 13.67 ms, 864
//! off, read `RUNNER`). A window reads the receivers' stated latencies at
//! its open; a line whose `max_lag` is under the longest of them is the
//! stack's (the schedule gave up packets the receiver still plays), never
//! the runner's, and the verdict names it.
//!
//! [`listen`] sets one global subscriber per test binary that records
//! only that line (every other callsite is never enabled); a window
//! [`open`]s on the sender's TX side and [`Opened::close`]s into
//! [`Restarts`]: the restarts the TX lanes counted and the lines logged
//! meanwhile. A test binary runs one sending device at a time, so every
//! line in the window is its sender's.

use std::fmt::{self, Debug};
use std::sync::{Mutex, MutexGuard, Once, PoisonError};

use dstack::clock::latency_samples;
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
    /// Whether the restart was a lag past the bound (not a jump back, whose
    /// lag is negative) after a sleep that returned the flow's latency late
    /// or more, at the device's `rate`: the TX thread was not run.
    pub(crate) fn slept_past(&self, rate: u32) -> bool {
        self.lag > i64::try_from(self.max_lag).unwrap_or(i64::MAX)
            && self.oversleep_us.is_some_and(|us| {
                u128::from(us) * u128::from(rate) >= u128::from(self.max_lag) * 1_000_000
            })
    }

    /// The most samples the restart may cost the receiver: its lag and its
    /// bound again. The schedule skips its lag and up to two packets (the
    /// packet it found late, and the new first stamp is the next multiple
    /// of fpp after now); the receiver counted 61 to 83 samples more than
    /// the lag a restart at 48 kHz (dev1, 18.11.2026: lag 577, 638 and 660
    /// ring holes a restart; fpp 32), so the room is the latency, not two
    /// packets. A loss past it has another cause beside the restarts.
    pub(crate) fn lost_max(&self) -> u64 {
        u64::try_from(self.lag.max(0))
            .unwrap_or(0)
            .saturating_add(self.max_lag)
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

/// The longest latency `tx`'s receivers state now, ns (0: none known).
fn receiver_ns(tx: &TxShared) -> u64 {
    tx.receiver_flows()
        .iter()
        .map(|&(slot, dest, _)| tx.receiver_latency_ns(slot, dest))
        .max()
        .unwrap_or(0)
}

/// A window open on a sender's TX side.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Opened {
    counted: u64,
    mark: usize,
    /// The receivers' longest stated latency at the open, ns.
    receiver_ns: u64,
}

/// Opens a window on `tx`, the sending device's TX side.
pub(crate) fn open(tx: &TxShared) -> Opened {
    listen();
    Opened {
        counted: counted(tx),
        mark: lines().len(),
        receiver_ns: receiver_ns(tx),
    }
}

impl Opened {
    /// The window's restarts on `tx`, a device at `rate` Hz.
    pub(crate) fn close(&self, tx: &TxShared, rate: u32) -> Restarts {
        Restarts {
            counted: counted(tx).saturating_sub(self.counted),
            lines: tx_lines(lines().get(self.mark..).unwrap_or_default()),
            rate,
            oversleep_max_us: tx.tx_thread().oversleep_max_us,
            receiver_lag: latency_samples(self.receiver_ns, rate),
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
    /// The receivers' longest stated latency at the window's open, samples
    /// (0: none known): every restart's bound must be at least this
    /// (dstack #426).
    pub(crate) receiver_lag: u64,
}

impl Restarts {
    /// Whether the TX schedule restarted at all.
    pub(crate) fn any(&self) -> bool {
        self.counted > 0
    }

    /// Whether every counted restart came with a line whose sleep
    /// returned the flow's latency late or more: the runner did not run
    /// the TX thread. A restart without such a line (a pass that ran long,
    /// a re-anchor, or no restart at all), or one bound under the
    /// receiver's latency ([`Restarts::under_receiver`]), is not the
    /// runner's.
    pub(crate) fn runner(&self) -> bool {
        self.any()
            && self.lines.len() as u64 >= self.counted
            && self.under_receiver() == 0
            && self.lines.iter().all(|r| r.slept_past(self.rate))
    }

    /// The restart lines bound under the receiver's latency (dstack #426):
    /// the schedule gave up packets the receiver still plays, the stack's
    /// loss whatever the sleep.
    pub(crate) fn under_receiver(&self) -> usize {
        self.lines
            .iter()
            .filter(|r| r.max_lag < self.receiver_lag)
            .count()
    }

    /// What the verdict names when the restarts are the stack's own
    /// (`OURS`): restarts bound under the receiver's latency, else nothing.
    pub(crate) fn ours(&self) -> &'static str {
        if self.under_receiver() > 0 {
            " (a TX restart bound under the receiver's latency)"
        } else {
            ""
        }
    }

    /// Whether the restarts are the runner's ([`Restarts::runner`]) and
    /// left at least `lost` samples unsent: each at most
    /// [`Restart::lost_max`]. A larger loss has another cause beside them
    /// (a dropped packet in the same window).
    pub(crate) fn explain(&self, lost: u64) -> bool {
        self.runner() && lost <= self.lines.iter().map(Restart::lost_max).sum::<u64>()
    }
}

/// The lines of `logged` the TX threads wrote: every other thread's (a
/// unit test's) never enters a window.
fn tx_lines(logged: &[(String, Restart)]) -> Vec<Restart> {
    logged
        .iter()
        .filter(|(thread, _)| thread.starts_with(TX_THREAD))
        .map(|&(_, r)| r)
        .collect()
}

/// `counted` TX restarts at 48 kHz with lines of these oversleeps (µs),
/// each a lag of 440 samples on a 9 ms latency (432 samples): the
/// witnesses' unit tests' fixture.
#[cfg(test)]
pub(crate) fn fixture(counted: u64, oversleeps: &[Option<u64>]) -> Restarts {
    Restarts {
        counted,
        lines: oversleeps
            .iter()
            .map(|&oversleep_us| Restart {
                lag: 440,
                max_lag: 432,
                oversleep_us,
            })
            .collect(),
        rate: 48_000,
        oversleep_max_us: 0,
        receiver_lag: 0,
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
            "{} TX restarts at {} Hz, lines {lines:?}, tx_thread.oversleep_max_us {}, \
             the receiver's latency {} samples ({} lines bound under it)",
            self.counted,
            self.rate,
            self.oversleep_max_us,
            self.receiver_lag,
            self.under_receiver()
        )
    }
}

#[cfg(test)]
mod restarts_tests {
    use super::*;
    use dstack::media::tx::Schedule;
    use dstack::media::tx::split::Before;

    /// A restart line at 48 kHz with a 9 ms latency (432 samples).
    fn line(oversleep_us: Option<u64>) -> Restart {
        Restart {
            lag: 440,
            max_lag: 432,
            oversleep_us,
        }
    }

    #[test]
    fn a_restart_is_the_runner_s_when_its_sleep_returned_the_latency_late() {
        // 432 samples at 48 kHz: 9000 us exactly, and one µs under it.
        assert!(line(Some(9_000)).slept_past(48_000));
        assert!(!line(Some(8_999)).slept_past(48_000));
        assert!(!line(None).slept_past(48_000));
        assert!(fixture(1, &[Some(9_282)]).runner());
        assert!(fixture(2, &[Some(9_282), Some(12_000)]).runner());
    }

    #[test]
    fn a_restart_without_a_late_sleep_is_not_the_runner_s() {
        // No restart: nothing to blame on the TX thread.
        assert!(!fixture(0, &[]).runner());
        // A sleep on time (the pass ran long), unmeasured, or one of two.
        assert!(!fixture(1, &[Some(500)]).runner());
        assert!(!fixture(1, &[None]).runner());
        assert!(!fixture(2, &[Some(9_282), Some(500)]).runner());
        // A counted restart with no line (a re-anchor).
        assert!(!fixture(2, &[Some(9_282)]).runner());
        // A jump back (a negative lag) or a lag within the bound, whatever
        // the sleep.
        let jump = Restart {
            lag: -5_000,
            ..line(Some(20_000))
        };
        assert!(!jump.slept_past(48_000));
        let within = Restart {
            lag: 432,
            ..line(Some(20_000))
        };
        assert!(!within.slept_past(48_000));
    }

    /// dstack #426 (run 44407677291): a restart whose bound is under the
    /// receiver's latency is the stack's, however late its sleep; at the
    /// receiver's latency or with none known, the sleep decides.
    #[test]
    fn a_restart_bound_under_the_receiver_s_latency_is_never_the_runner_s() {
        let mut r = fixture(1, &[Some(9_282)]);
        assert!(r.runner(), "no receiver latency known");
        assert_eq!((r.under_receiver(), r.ours()), (0, ""));
        r.receiver_lag = 432;
        assert!(r.runner(), "bound at the receiver's latency");
        r.receiver_lag = 433;
        assert!(!r.runner(), "bound one sample under it");
        assert!(!r.explain(1));
        assert_eq!(r.under_receiver(), 1);
        assert_eq!(
            r.ours(),
            " (a TX restart bound under the receiver's latency)"
        );
        // The run's own line: lag 812, bound 803 at 96 kHz, a sleep 8465 us
        // late, the receiver at 13666 us (1312 samples).
        let run = Restarts {
            counted: 1,
            lines: vec![Restart {
                lag: 812,
                max_lag: 803,
                oversleep_us: Some(8_465),
            }],
            rate: 96_000,
            oversleep_max_us: 8_465,
            receiver_lag: latency_samples(13_666_000, 96_000),
        };
        assert!(run.lines[0].slept_past(96_000), "the sleep alone");
        assert!(!run.runner(), "{run:?}");
        assert!(
            format!("{run:?}")
                .contains("the receiver's latency 1312 samples (1 lines bound under it)"),
            "{run:?}"
        );
    }

    #[test]
    fn restarts_explain_at_most_their_lags_and_bounds_each() {
        // 440 + 432 samples a restart.
        assert_eq!(line(Some(9_282)).lost_max(), 872);
        let two = fixture(2, &[Some(9_282), Some(9_282)]);
        assert!(two.explain(1_744));
        assert!(!two.explain(1_745));
        // Never past a restart without a late sleep.
        assert!(!fixture(1, &[Some(500)]).explain(1));
    }

    #[test]
    fn a_window_takes_only_the_tx_threads_lines() {
        let logged = [
            ("dstack-tx".to_string(), line(Some(1))),
            ("dstack-tx-relay".to_string(), line(Some(2))),
            ("restarts_tests".to_string(), line(Some(3))),
            (String::new(), line(Some(4))),
        ];
        assert_eq!(tx_lines(&logged), [line(Some(1)), line(Some(2))]);
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
    fn the_subscriber_records_the_stack_s_own_restart_line() {
        listen();
        let mark = lines().len();
        // The stack's own line (`Schedule::due_after`): a schedule at 0
        // with fpp 32 sends its first packet (stamp 32) at 64, so a pass at
        // 504 is 440 samples late, past a 432-sample bound, after a sleep
        // 9.282 ms late.
        let mut schedule = Schedule::bootstrap(0, 32, 432);
        let before = Before {
            oversleep_ns: Some(9_282_000),
            ..Before::default()
        };
        assert_eq!(schedule.due_after(504, &before), None);
        // Unmeasured: `-`.
        let mut schedule = Schedule::bootstrap(0, 32, 432);
        assert_eq!(schedule.due_after(600, &Before::default()), None);
        // Another warning and another target are not taken.
        tracing::warn!(target: "dstack::media::tx::schedule", lag = 1_i64, "another line");
        tracing::warn!(target: "dstack::other", "the TX schedule starts again");
        assert_eq!(
            own_since(mark),
            [
                Restart {
                    lag: 440,
                    max_lag: 432,
                    oversleep_us: Some(9_282)
                },
                Restart {
                    lag: 536,
                    max_lag: 432,
                    oversleep_us: None
                }
            ]
        );
    }
}
