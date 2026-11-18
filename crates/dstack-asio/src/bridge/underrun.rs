// SPDX-License-Identifier: MIT OR Apache-2.0

//! The bridge's underrun line (dstack #386): when a player's underrun
//! count rose, one warning names the driver callbacks' lateness over the
//! same time, so a field underrun reads as the driver's or the bridge's.
//!
//! **Why the lateness decides it** (#382, issuecomment-6952521129): a late
//! driver callback is a late WRITE into the from-ASIO FIFO while the TX
//! thread reads on time, so the from-ASIO side loses depth one for one
//! with the lateness (at a 1.333 ms target the simulation lost audio from
//! 1.0-1.5 ms late, far under the two periods `late_callbacks` counts).
//! The to-ASIO side pulls and plays in the same switch, so a late switch
//! costs it nothing; only a gap over two periods (its catch-up switches
//! run the FIFO dry, `tests/to_asio_pull.rs`' driver-thread witness) is
//! the driver's there.
//!
//! The app's status loop calls [`UnderrunWatch::look`] with the counts it
//! already reads (the players' underruns, the runner's lateness bins and
//! late count, [`UnderrunCounts::of`]); never the driver's callback or a
//! player's thread (it formats and logs). A count that falls (a new run's
//! counters, a new player's) counts again from 0; a count reset and grown
//! past its old value between two checks reads as a smaller rise (the
//! check is the app's tick, 10 s in dasio).

use dstack::info::{FigureSlot, UnderrunCause};
use tracing::warn;

use crate::runner::Status;
use crate::state::{LATE_BIN_NS, LATE_BINS, frames_to_ns};

/// One bin's width, us.
const LATE_BIN_US: u64 = LATE_BIN_NS as u64 / 1000;

/// The counts one check reads (module doc).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UnderrunCounts {
    /// The Dante -> ASIO player's underruns (`Bridge::to_asio_stats`).
    pub to_asio_underruns: u64,
    /// The ASIO -> Dante player's underruns (`Bridge::from_asio_stats`).
    pub from_asio_underruns: u64,
    /// The runner's callbacks per lateness bin ([`Status::late_bins`]).
    pub late_bins: [u64; LATE_BINS],
    /// The runner's callbacks over two periods ([`Status::late_callbacks`]).
    pub late_callbacks: u64,
    /// The driver's buffer period, us (0: no driver opened yet): how late
    /// one gap can make the top bin's callbacks.
    pub period_us: u64,
    /// The run's peak lateness behind the driver's schedule, us
    /// ([`Status::late_max_us`], dstack #393). In a rise
    /// ([`UnderrunCounts::since`]): the peak when it rose over the check
    /// (or a new run's), else 0.
    pub late_max_us: u64,
}

impl UnderrunCounts {
    /// The counts from the runner's status and the two players' underruns.
    pub fn of(status: &Status, to_asio_underruns: u64, from_asio_underruns: u64) -> Self {
        let period_us = if status.rate == 0 {
            0
        } else {
            frames_to_ns(i64::from(status.buffer_frames), f64::from(status.rate)) as u64 / 1000
        };
        UnderrunCounts {
            to_asio_underruns,
            from_asio_underruns,
            late_bins: status.late_bins,
            late_callbacks: status.late_callbacks,
            period_us,
            late_max_us: status.late_max_us,
        }
    }

    /// What rose from `before` to `self`, each count on its own (the
    /// period is `self`'s).
    fn since(&self, before: &UnderrunCounts) -> UnderrunCounts {
        UnderrunCounts {
            to_asio_underruns: rise(self.to_asio_underruns, before.to_asio_underruns),
            from_asio_underruns: rise(self.from_asio_underruns, before.from_asio_underruns),
            late_bins: std::array::from_fn(|i| rise(self.late_bins[i], before.late_bins[i])),
            late_callbacks: rise(self.late_callbacks, before.late_callbacks),
            period_us: self.period_us,
            late_max_us: if self.late_max_us == before.late_max_us {
                0
            } else {
                self.late_max_us
            },
        }
    }
}

/// A count's rise: one that fell started again from 0 (module doc).
fn rise(now: u64, before: u64) -> u64 {
    if now < before { now } else { now - before }
}

/// Whose an underrun reads as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// A driver callback came late enough to cost that side its audio.
    Driver,
    /// No callback came late enough: the bridge's (or its clock's).
    Bridge,
    /// The from-ASIO target is not known yet (0: no player started).
    Unknown,
}

impl From<Cause> for UnderrunCause {
    /// The cause as the bridge-figures reply carries it (dstack #391).
    fn from(c: Cause) -> UnderrunCause {
        match c {
            Cause::Driver => UnderrunCause::Driver,
            Cause::Bridge => UnderrunCause::Bridge,
            Cause::Unknown => UnderrunCause::Unknown,
        }
    }
}

impl Cause {
    fn text(self) -> &'static str {
        match self {
            Cause::Driver => "cause: driver callback lateness",
            Cause::Bridge => "cause: the bridge (no callback late enough)",
            Cause::Unknown => "cause: unknown (no target yet)",
        }
    }
}

/// Whose a from-ASIO underrun over `rise` reads as, at a from-ASIO target
/// of `target_us`: the driver's when a callback came over two periods
/// late, or surely at least 3/4 of the target late: a bin whose lower edge
/// is at least that (at 1.333 ms from 1.0 ms late, where #382's simulation
/// first lost audio); the open top bin (2.0 ms or more behind the
/// driver's schedule, #393) by the run's peak lateness when that rose over
/// the check (a lateness summed over several switches can pass a period),
/// else by the period, as late as one gap can make a callback. Else the
/// bridge's; unknown while the target is 0.
pub fn from_asio_cause(rise: &UnderrunCounts, target_us: u64) -> Cause {
    if target_us == 0 {
        return Cause::Unknown;
    }
    if rise.late_callbacks > 0 {
        return Cause::Driver;
    }
    let reach_us = match rise.late_bins.iter().rposition(|&n| n > 0) {
        None => return Cause::Bridge,
        Some(bin) if is_top(bin) && rise.late_max_us > 0 => rise.late_max_us,
        Some(bin) if is_top(bin) => rise.period_us.max(lower_edge_us(bin)),
        Some(bin) => lower_edge_us(bin),
    };
    if 4 * reach_us >= 3 * target_us {
        Cause::Driver
    } else {
        Cause::Bridge
    }
}

/// Whose a to-ASIO underrun over `rise` reads as: the driver's only after
/// a gap over two periods (module doc), whatever the lateness under it.
pub fn to_asio_cause(rise: &UnderrunCounts) -> Cause {
    if rise.late_callbacks > 0 {
        Cause::Driver
    } else {
        Cause::Bridge
    }
}

/// A lateness bin's lower edge, us.
fn lower_edge_us(bin: usize) -> u64 {
    (bin as u64 + 1) * LATE_BIN_US
}

/// The open top bin (2.0 ms or more behind the driver's schedule).
fn is_top(bin: usize) -> bool {
    bin + 1 == LATE_BINS
}

/// The bins that rose, by their edges in us, and the count over two
/// periods; "none" when no callback came a bin's width late.
fn lateness_text(rise: &UnderrunCounts) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (bin, &n) in rise.late_bins.iter().enumerate() {
        if n == 0 {
            continue;
        }
        let low = lower_edge_us(bin);
        if is_top(bin) {
            parts.push(format!("{low} us or more: {n}"));
        } else {
            parts.push(format!("{low}-{} us: {n}", low + LATE_BIN_US));
        }
    }
    if rise.late_callbacks > 0 {
        parts.push(format!("over two periods: {}", rise.late_callbacks));
    }
    if parts.is_empty() {
        format!("none {LATE_BIN_US} us late or more")
    } else {
        parts.join(", ")
    }
}

/// The warning for underruns that rose by `rise` (module doc).
fn underrun_text(rise: &UnderrunCounts, from_asio_target_us: u64) -> String {
    let mut sides: Vec<String> = Vec::new();
    if rise.from_asio_underruns > 0 {
        sides.push(format!(
            "from-ASIO {} (target {from_asio_target_us} us), {}",
            rise.from_asio_underruns,
            from_asio_cause(rise, from_asio_target_us).text()
        ));
    }
    if rise.to_asio_underruns > 0 {
        sides.push(format!(
            "to-ASIO {}, {}",
            rise.to_asio_underruns,
            to_asio_cause(rise).text()
        ));
    }
    format!(
        "asio bridge underruns since the last check: {}; driver callbacks late meanwhile: {}",
        sides.join("; "),
        lateness_text(rise)
    )
}

/// A ms figure (`dlink::bridge::Stats::target_ms`) in whole us, rounded
/// (NaN and negatives read 0).
fn us_of_ms(ms: f64) -> u64 {
    (ms * 1000.0).round() as u64
}

/// The underrun watch the app's status loop keeps (module doc).
#[derive(Debug, Default)]
pub struct UnderrunWatch {
    last: UnderrunCounts,
    /// Each side's latest underrun's cause ([`UnderrunWatch::publish`]).
    to_cause: UnderrunCause,
    from_cause: UnderrunCause,
}

impl UnderrunWatch {
    /// A watch from all counts 0 (a bridge's start).
    pub fn new() -> Self {
        UnderrunWatch::default()
    }

    /// One check: when an underrun count rose since the last check, WARN
    /// which side, how many, whose, and the callbacks' lateness over the
    /// same time, and return that text; `None` otherwise.
    /// `from_asio_target_ms` is the from-ASIO player's target
    /// (`Stats::target_ms`).
    pub fn look(&mut self, now: UnderrunCounts, from_asio_target_ms: f64) -> Option<String> {
        let rise = now.since(&self.last);
        self.last = now;
        if rise.to_asio_underruns == 0 && rise.from_asio_underruns == 0 {
            return None;
        }
        let target_us = us_of_ms(from_asio_target_ms);
        if rise.to_asio_underruns > 0 {
            self.to_cause = to_asio_cause(&rise).into();
        }
        if rise.from_asio_underruns > 0 {
            self.from_cause = from_asio_cause(&rise, target_us).into();
        }
        let text = underrun_text(&rise, target_us);
        warn!("{text}");
        Some(text)
    }

    /// Hands the last check's underrun counts and each side's latest
    /// underrun's cause to the device's bridge figures (dstack #391,
    /// [`FigureSlot::set_underruns`]): dcontroller reads them over the
    /// network. Call it after each [`UnderrunWatch::look`] (dasio: its
    /// counts tick, `underruns.publish(media.bridge_figures())`).
    pub fn publish(&self, slot: &FigureSlot) {
        slot.set_underruns(
            self.last.to_asio_underruns,
            self.last.from_asio_underruns,
            self.to_cause,
            self.from_cause,
        );
    }
}

#[cfg(test)]
mod tests;
