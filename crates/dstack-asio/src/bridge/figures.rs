// SPDX-License-Identifier: MIT OR Apache-2.0

//! The bridge's own figures for the network read (dstack #391,
//! `dstack::info::bridge`): its two players' targets and converter delays
//! and the runner's callback lateness, read at each request from the
//! players' [`Stats`] and the runner's [`Status`]. The app hands the
//! underruns and their causes itself ([`super::UnderrunWatch::publish`]).

use std::sync::{Arc, Mutex, PoisonError};

use dlink::bridge::Stats;
use dstack::info::{FigureSlot, OwnFigures};

use super::Bridge;
use crate::runner::Status;
use crate::state::LATE_BINS;

const _: () = assert!(LATE_BINS == dstack::info::bridge::LATE_BINS);

/// A ms figure ([`Stats::target_ms`]) in whole us, rounded and saturated
/// (NaN and negatives read 0).
fn us_of_ms(ms: f64) -> u32 {
    (ms * 1000.0).round() as u32
}

/// A ns figure in whole us, rounded, saturated.
fn us_of_ns(ns: u64) -> u32 {
    u32::try_from(ns.saturating_add(500) / 1000).unwrap_or(u32::MAX)
}

/// The figures the bridge owns now: each player's target and converter
/// delay, the runner's lateness bins and late callbacks (its current run).
pub fn own_figures(to_asio: &Stats, from_asio: &Stats, status: &Status) -> OwnFigures {
    OwnFigures {
        to_asio_target_us: us_of_ms(to_asio.target_ms()),
        from_asio_target_us: us_of_ms(from_asio.target_ms()),
        to_asio_converter_delay_us: us_of_ns(to_asio.converter_delay_ns()),
        from_asio_converter_delay_us: us_of_ns(from_asio.converter_delay_ns()),
        late_callbacks: status.late_callbacks,
        late_bins: status.late_bins,
    }
}

impl Bridge {
    /// Publishes this bridge's own figures to the device's `slot`
    /// (`Media::bridge_figures`, dstack #391): from now on the device
    /// answers dcontroller's bridge-figures request with the players'
    /// targets and converter delays and `runner`'s lateness
    /// (`Runner::status`), each read at the request, and the underruns the
    /// app hands with [`super::UnderrunWatch::publish`]. Call it once, when
    /// the device and the runner exist.
    pub fn publish_figures(&self, slot: &FigureSlot, runner: Arc<Mutex<Status>>) {
        let (to, from) = (self.to_asio_stats(), self.from_asio_stats());
        slot.set_source(move || {
            let status = runner.lock().unwrap_or_else(PoisonError::into_inner);
            own_figures(&to, &from, &status)
        });
    }
}

#[cfg(test)]
mod tests;
