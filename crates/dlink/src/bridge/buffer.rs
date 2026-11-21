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

/// The least target a dlink app's device takes, ns, and every
/// [`AppBuffer`]'s floor unless its user sets its own
/// ([`AppBuffer::with_floor`]: dstack-asio's per-direction bridge targets,
/// dstack #422, and its default target, dstack #482). It is a sanity
/// bound, not a working value: a target only holds when it covers half
/// the app's chunk and a block ([`AppBuffer::short`], warned at the
/// start).
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
/// the pump's block, the value the app configured, if any, and the least
/// target taken ([`AppBuffer::floor_ns`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppBuffer {
    chunk_ns: u64,
    block_ns: u64,
    set_ns: Option<u64>,
    floor_ns: u64,
}

impl AppBuffer {
    /// The default of an app whose fixed chunk is `chunk_ns` (see
    /// [`chunk_ns`]) on a device whose pump block is `block_ns`: their sum.
    pub fn new(chunk_ns: u64, block_ns: u64) -> Self {
        AppBuffer {
            chunk_ns,
            block_ns,
            set_ns: None,
            floor_ns: MIN_APP_BUFFER_NS,
        }
    }

    /// With the least target its user takes, ns (dstack #422; the default
    /// is [`MIN_APP_BUFFER_NS`], every dlink app's): a bridge whose floor was
    /// measured for its own shape (dstack-asio's per-direction targets,
    /// `MIN_BRIDGE_TARGET_NS`). [`AppBuffer::problem`] and the clamp
    /// ([`AppBuffer::clamped_ns`]) follow it; a floor over
    /// [`MAX_APP_BUFFER_NS`] counts as that.
    #[must_use]
    pub fn with_floor(self, floor_ns: u64) -> Self {
        AppBuffer {
            floor_ns: floor_ns.min(MAX_APP_BUFFER_NS),
            ..self
        }
    }

    /// The least target taken, ns ([`AppBuffer::with_floor`]).
    pub fn floor_ns(&self) -> u64 {
        self.floor_ns
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

    /// The target, s, within [`AppBuffer::floor_ns`] to
    /// [`MAX_APP_BUFFER_NS`] (a target the device refused never reaches a
    /// player; this only keeps a direct caller's out-of-range value from
    /// breaking the FIFO).
    pub fn target_s(&self) -> f64 {
        self.clamped_ns() as f64 / 1e9
    }

    /// The target, ns, within [`AppBuffer::floor_ns`] to
    /// [`MAX_APP_BUFFER_NS`] (what a player keeps, [`AppBuffer::target_s`]).
    pub fn clamped_ns(&self) -> u64 {
        self.target_ns().clamp(self.floor_ns, MAX_APP_BUFFER_NS)
    }

    /// The chunk, s.
    pub fn chunk_s(&self) -> f64 {
        self.chunk_ns as f64 / 1e9
    }

    /// Why a configured target is too small to hold, if it is: a chunked
    /// producer's depth swings half a chunk under the target, and the
    /// converter takes a block's input, so a target under half the chunk
    /// plus one block drops out every chunk. The device warns at its start
    /// (the FIFO runs dry before the next chunk). A default target
    /// (a whole chunk and a block) never is.
    pub fn short(&self) -> Option<String> {
        let least = self.chunk_ns / 2 + self.block_ns;
        let set = self.set_ns?;
        (set < least).then(|| {
            format!(
                "app buffer {} ms is under half the app chunk + one pump block ({} ms): \
                 expect a dropout every chunk",
                ms(set),
                ms(least)
            )
        })
    }

    /// Why the target cannot be used, if it cannot: outside
    /// [`AppBuffer::floor_ns`] to [`MAX_APP_BUFFER_NS`]
    /// (`app_buffer_ns: 999999 ns (0.999999 ms); 1 to 150 ms`).
    pub fn problem(&self) -> Option<String> {
        self.problem_for("app_buffer_ns")
    }

    /// [`AppBuffer::problem`] named for the setting `key` that gave the
    /// target (dstack-asio's `to_asio_app_buffer_ns`, dstack #422).
    pub fn problem_for(&self, key: &str) -> Option<String> {
        let t = self.target_ns();
        if (self.floor_ns..=MAX_APP_BUFFER_NS).contains(&t) {
            return None;
        }
        Some(format!(
            "{key}: {t} ns ({} ms); {} to {} ms",
            ms(t),
            ms(self.floor_ns),
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

    /// dstack #422: a floor its user sets moves the range, the clamp and
    /// the text; every buffer made without one keeps dlink's 1 ms, and a
    /// floor past the largest target counts as that.
    #[test]
    fn a_floor_its_user_sets_moves_the_range_and_the_clamp() {
        let b = AppBuffer::new(10_000_000, 1_333_333);
        assert_eq!(b.floor_ns(), MIN_APP_BUFFER_NS);
        assert_eq!(b.with_floor(MIN_APP_BUFFER_NS), b);
        let low = b.with_floor(400_000);
        assert_eq!(low.floor_ns(), 400_000);
        assert_eq!(low.set(Some(400_000)).problem(), None);
        assert_eq!(low.set(Some(MAX_APP_BUFFER_NS)).problem(), None);
        assert_eq!(
            low.set(Some(399_999)).problem().as_deref(),
            Some("app_buffer_ns: 399999 ns (0.399999 ms); 0.4 to 150 ms")
        );
        assert_eq!(
            low.set(Some(MAX_APP_BUFFER_NS + 1))
                .problem_for("to_asio_app_buffer_ns")
                .as_deref(),
            Some("to_asio_app_buffer_ns: 150000001 ns (150.000001 ms); 0.4 to 150 ms")
        );
        assert_eq!(low.set(Some(500_000)).clamped_ns(), 500_000);
        assert!((low.set(Some(500_000)).target_s() - 0.0005).abs() < 1e-15);
        assert_eq!(low.set(Some(0)).clamped_ns(), 400_000);
        // The same target without the floor is clamped up to dlink's.
        assert_eq!(b.set(Some(500_000)).clamped_ns(), MIN_APP_BUFFER_NS);
        assert!(b.set(Some(500_000)).problem().is_some());
        let over = b.with_floor(u64::MAX);
        assert_eq!(over.floor_ns(), MAX_APP_BUFFER_NS);
        assert_eq!(over.set(Some(0)).clamped_ns(), MAX_APP_BUFFER_NS);
        assert_eq!(over.set(Some(MAX_APP_BUFFER_NS)).problem(), None);
        assert_eq!(b.with_floor(0).set(Some(0)).problem(), None);
    }

    /// A set target under half the chunk plus a block is warned at the
    /// start; at it, or the default, is not.
    #[test]
    fn a_target_under_half_the_chunk_and_a_block_is_short() {
        let b = AppBuffer::new(10_000_000, 1_333_333);
        assert_eq!(b.short(), None, "the default");
        assert_eq!(b.set(Some(6_333_333)).short(), None, "at the bound");
        assert_eq!(
            b.set(Some(6_333_332)).short().as_deref(),
            Some(
                "app buffer 6.333332 ms is under half the app chunk + one pump block \
                 (6.333333 ms): expect a dropout every chunk"
            )
        );
        assert_eq!(b.set(Some(5_000_001)).clamped_ns(), 5_000_001);
        assert_eq!(b.set(Some(0)).clamped_ns(), MIN_APP_BUFFER_NS);
        assert_eq!(b.set(Some(u64::MAX)).clamped_ns(), MAX_APP_BUFFER_NS);
        assert_eq!(b.chunk_s(), 0.010);
    }

    #[test]
    fn a_chunk_rounds_to_the_nearest_ns_and_a_rate_of_zero_counts_as_one_hz() {
        assert_eq!(chunk_ns(480, 48_000), 10_000_000);
        assert_eq!(chunk_ns(1, 96_000), 10_417);
        assert_eq!(chunk_ns(2, 0), 2_000_000_000);
    }
}
