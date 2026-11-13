// SPDX-License-Identifier: MIT OR Apache-2.0

//! The bridge FIFO's target, `app_buffer_ns` (dstack #247, the owner's
//! ROZHODNUTÉ of 13.11.2026): a fixed latency term of the device, never
//! measured and never moved while it runs.
//!
//! - **Default:** a constant of the app, from its known fixed chunk (the
//!   frames it hands or takes at once) plus one pump block at the device's
//!   rate: OBS 1024 frames at 48 kHz (21.333 ms) + 1.333 ms (block 128 at
//!   96 kHz) = 22.667 ms; an ASIO bridge its ASIO buffer + the block; dsend
//!   and dlisten their WASAPI period + the block.
//! - **Configured:** a value the app sets (`--app-buffer-ms` in dsend and
//!   dlisten) wins, used as given, and never changes by itself.
//!
//! The [`Player`](super::Player) keeps the FIFO's depth at it; a dropout is
//! counted in its [`Stats`](super::Stats), never absorbed by a larger
//! target. The device logs it at its start beside dstack #244's terms
//! ([`AppBuffer::line`], `dlink::Config::app_buffer`).

use crate::latency::ms;

/// The least configured target, ns: under a millisecond the FIFO holds
/// less than the converter's own call at any rate.
pub const MIN_APP_BUFFER_NS: u64 = 1_000_000;
/// The largest target, ns: the player re-centres an excess of twice the
/// target ([`super::player::hard_excess_s`]), so three targets must fit in
/// the FIFO's [`super::player::FIFO_S`] with room for a burst.
pub const MAX_APP_BUFFER_NS: u64 = 150_000_000;

/// `frames` at `rate` in ns, rounded to the nearest (a rate of 0 counts as
/// 1 Hz): an app's chunk ([`AppBuffer::new`]). 1024 frames at 48 kHz is
/// 21333333 ns, 128 at 96 kHz 1333333 ns.
pub fn chunk_ns(frames: u64, rate: u32) -> u64 {
    let rate = u128::from(rate.max(1));
    let ns = (u128::from(frames) * 1_000_000_000 + rate / 2) / rate;
    u64::try_from(ns).unwrap_or(u64::MAX)
}

/// The bridge FIFO's target of one device (module doc): the app's chunk,
/// the pump's block and the value the app configured, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppBuffer {
    chunk_ns: u64,
    block_ns: u64,
    set_ns: Option<u64>,
}

impl AppBuffer {
    /// The default of an app whose fixed chunk is `chunk_ns` (see
    /// [`chunk_ns`]) on a device whose pump block is `block_ns`: their sum.
    pub fn new(chunk_ns: u64, block_ns: u64) -> Self {
        AppBuffer {
            chunk_ns,
            block_ns,
            set_ns: None,
        }
    }

    /// With the value the app configured (`None`: the default). It is the
    /// target as given; [`AppBuffer::problem`] says when it is out of
    /// range.
    #[must_use]
    pub fn set(self, set_ns: Option<u64>) -> Self {
        AppBuffer { set_ns, ..self }
    }

    /// The app's chunk, ns.
    pub fn chunk_ns(&self) -> u64 {
        self.chunk_ns
    }

    /// The pump's block, ns.
    pub fn block_ns(&self) -> u64 {
        self.block_ns
    }

    /// The configured value, ns, if any.
    pub fn set_ns(&self) -> Option<u64> {
        self.set_ns
    }

    /// The default: the chunk and one block, ns.
    pub fn default_ns(&self) -> u64 {
        self.chunk_ns.saturating_add(self.block_ns)
    }

    /// The target, ns: the configured value, else [`AppBuffer::default_ns`].
    pub fn target_ns(&self) -> u64 {
        self.set_ns.unwrap_or_else(|| self.default_ns())
    }

    /// The target, s, within [`MIN_APP_BUFFER_NS`] to [`MAX_APP_BUFFER_NS`]
    /// (a target the device refused never reaches a player; this only
    /// keeps a direct caller's out-of-range value from breaking the FIFO).
    pub fn target_s(&self) -> f64 {
        self.target_ns().clamp(MIN_APP_BUFFER_NS, MAX_APP_BUFFER_NS) as f64 / 1e9
    }

    /// Why the target cannot be used, if it cannot: outside
    /// [`MIN_APP_BUFFER_NS`] to [`MAX_APP_BUFFER_NS`].
    pub fn problem(&self) -> Option<String> {
        let t = self.target_ns();
        if (MIN_APP_BUFFER_NS..=MAX_APP_BUFFER_NS).contains(&t) {
            return None;
        }
        Some(format!(
            "app_buffer_ns: {t} ns ({} ms); {} to {} ms",
            ms(t),
            ms(MIN_APP_BUFFER_NS),
            ms(MAX_APP_BUFFER_NS)
        ))
    }

    /// The line the device logs at its start, beside dstack #244's terms:
    /// `app buffer (bridge FIFO target) 22.666666 ms = app chunk 21.333333
    /// ms + pump block 1.333333 ms (default)`, or with a configured value
    /// `app buffer (bridge FIFO target) 15 ms (set; default 22.666666 ms =
    /// app chunk 21.333333 ms + pump block 1.333333 ms)`.
    pub fn line(&self) -> String {
        let default = format!(
            "app chunk {} ms + pump block {} ms",
            ms(self.chunk_ns),
            ms(self.block_ns)
        );
        match self.set_ns {
            None => format!(
                "app buffer (bridge FIFO target) {} ms = {default} (default)",
                ms(self.default_ns())
            ),
            Some(set) => format!(
                "app buffer (bridge FIFO target) {} ms (set; default {} ms = {default})",
                ms(set),
                ms(self.default_ns())
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// OBS's default (dstack #247): its fixed 1024-frame tick at 48 kHz,
    /// 21.333 ms, and one pump block at the device's rate.
    #[test]
    fn obs_default_is_its_1024_frame_chunk_and_one_pump_block() {
        let chunk = chunk_ns(1024, 48_000);
        assert_eq!(chunk, 21_333_333);
        let at_96k = AppBuffer::new(chunk, chunk_ns(128, 96_000));
        assert_eq!(at_96k.block_ns(), 1_333_333);
        assert_eq!(at_96k.target_ns(), 22_666_666);
        let at_48k = AppBuffer::new(chunk, chunk_ns(128, 48_000));
        assert_eq!(at_48k.target_ns(), 21_333_333 + 2_666_667);
        assert!((at_96k.target_s() - 0.022_666_666).abs() < 1e-12);
        assert_eq!(at_96k.problem(), None);
    }

    /// A configured value is the target as given, whatever the default.
    #[test]
    fn a_configured_value_is_used_verbatim() {
        let b = AppBuffer::new(21_333_333, 1_333_333).set(Some(15_000_123));
        assert_eq!(b.target_ns(), 15_000_123);
        assert_eq!(b.default_ns(), 22_666_666);
        assert_eq!(b.set_ns(), Some(15_000_123));
        assert!((b.target_s() - 0.015_000_123).abs() < 1e-15);
        assert_eq!(b.set(None).target_ns(), 22_666_666);
    }

    #[test]
    fn the_start_line_names_the_terms_and_where_they_came_from() {
        let b = AppBuffer::new(10_000_000, 1_333_333);
        assert_eq!(
            b.line(),
            "app buffer (bridge FIFO target) 11.333333 ms = app chunk 10 ms + \
             pump block 1.333333 ms (default)"
        );
        assert_eq!(
            b.set(Some(40_000_000)).line(),
            "app buffer (bridge FIFO target) 40 ms (set; default 11.333333 ms = \
             app chunk 10 ms + pump block 1.333333 ms)"
        );
    }

    #[test]
    fn a_target_out_of_range_is_a_problem_and_never_reaches_the_fifo() {
        let b = AppBuffer::new(10_000_000, 1_333_333);
        assert_eq!(b.set(Some(MIN_APP_BUFFER_NS)).problem(), None);
        assert_eq!(b.set(Some(MAX_APP_BUFFER_NS)).problem(), None);
        assert_eq!(
            b.set(Some(999_999)).problem().as_deref(),
            Some("app_buffer_ns: 999999 ns (0.999999 ms); 1 to 150 ms")
        );
        let big = b.set(Some(MAX_APP_BUFFER_NS + 1));
        assert!(big.problem().is_some());
        assert!((big.target_s() - 0.150).abs() < 1e-12);
        assert!((b.set(Some(0)).target_s() - 0.001).abs() < 1e-12);
    }

    #[test]
    fn a_chunk_rounds_to_the_nearest_ns_and_a_rate_of_zero_counts_as_one_hz() {
        assert_eq!(chunk_ns(480, 48_000), 10_000_000);
        assert_eq!(chunk_ns(1, 96_000), 10_417);
        assert_eq!(chunk_ns(2, 0), 2_000_000_000);
    }
}
