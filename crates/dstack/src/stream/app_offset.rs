// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The media clock's offset from the app's clock, filtered (dstack #295
//! step 2): what each block's play time ([`dstack_audio_io::Block::play_at`])
//! is stamped with.
//!
//! The pump reads the pair once a wake: the app's clock, the media clock,
//! the app's clock again, and pairs the media read with the midpoint of the
//! two app reads ([`pair_offset`]). The pump has no real-time priority on
//! the `normal` timing profile, so a preemption between the reads now and
//! then skews one wake's pair by the preempted time (strih-snv 14.11.2026:
//! paired play_at jumps of ±1.3 to 2.3 ms that cancel within 3 to 5 ms,
//! about every 15 minutes). The real offset moves only by the rate gap
//! between the media clock and the app's (about 6 ppm).
//!
//! [`OffsetFilter`]: the first pair is taken. After it, a pair more than
//! [`REJECT_NS`] off the filtered offset is rejected (a torn read: the
//! filtered offset is kept and the pump counts it, `play_at_rejected`); a
//! closer one is followed by at most [`SLEW_PPM`] of the app time since the
//! last followed pair ([`slew_max_ns`]), so a real rate gap up to that is
//! followed and the play time never jumps. A change over [`REJECT_NS`] that
//! [`STEP_WAKES`] wakes in a row read alike is a real clock step: taken at
//! once, and the pump warns ([`STEP_LINE`]), so a real clock event is never
//! hidden. A media re-anchor moves the offset by its step together with the
//! blocks' media start: the pump shifts the filter by a known step
//! ([`OffsetFilter::shift`]) and starts it afresh on an unknown one
//! ([`OffsetFilter::restart`]).

/// A pair further than this from the filtered offset is rejected, ns.
pub const REJECT_NS: u64 = 50_000;

/// The most the filtered offset follows a pair, as ppm of the app time
/// since the last followed pair. The bound is rounded down to whole ns
/// each wake ([`slew_max_ns`]) and the fraction is not carried, so the
/// rate followed is a little under it: 99 ppm at 333 µs wakes (96 kHz,
/// 32 frames), 96 ppm at 83 µs; the real rate gap (about 6 ppm) is far
/// below either.
pub const SLEW_PPM: i64 = 100;

/// Wakes in a row whose pairs read alike (within [`REJECT_NS`] of each
/// other) and over [`REJECT_NS`] off the filtered offset: a real step.
pub const STEP_WAKES: u32 = 3;

/// The pump's warning on a step taken ([`Outcome::Stepped`]): the text a
/// consumer greps.
pub const STEP_LINE: &str = "play_at: the media clock's offset from the app's clock stepped";

/// The pump watch's line once a minute in which pairs were rejected.
pub const REJECTED_LINE: &str = "play_at: torn clock pair reads rejected in the last minute";

/// What one pair did to the filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The first pair (or the first after a restart): taken.
    First,
    /// Followed, by at most the slew bound.
    Followed,
    /// Over [`REJECT_NS`] off: the filtered offset kept.
    Rejected,
    /// The [`STEP_WAKES`]th pair in a row read alike off it: taken; the
    /// filtered offset moved by `by_ns`.
    Stepped { by_ns: i64 },
}

/// The filtered offset (see the module).
#[derive(Debug, Clone, Copy, Default)]
pub struct OffsetFilter {
    /// The filtered offset, ns, and the app time of the pair it last
    /// followed or took.
    at: Option<(i64, i64)>,
    /// Pairs rejected in a row that read alike, and the last one's offset.
    run: u32,
    last_off: i64,
}

impl OffsetFilter {
    /// A filter before its first pair.
    pub const fn new() -> Self {
        OffsetFilter {
            at: None,
            run: 0,
            last_off: 0,
        }
    }

    /// The filtered offset, ns (`None` before the first pair).
    pub fn offset(&self) -> Option<i64> {
        self.at.map(|(offset, _)| offset)
    }

    /// One wake's pair: its offset `raw` (the media clock less the app's,
    /// ns) read at the app time `app_ns`.
    pub fn update(&mut self, raw: i64, app_ns: i64) -> Outcome {
        let Some((filtered, since)) = self.at else {
            self.take(raw, app_ns);
            return Outcome::First;
        };
        let err = raw.saturating_sub(filtered);
        if err.unsigned_abs() > REJECT_NS {
            let alike =
                self.run > 0 && raw.saturating_sub(self.last_off).unsigned_abs() <= REJECT_NS;
            self.run = if alike { self.run + 1 } else { 1 };
            self.last_off = raw;
            if self.run < STEP_WAKES {
                return Outcome::Rejected;
            }
            self.take(raw, app_ns);
            return Outcome::Stepped { by_ns: err };
        }
        self.run = 0;
        let max = slew_max_ns(app_ns.saturating_sub(since));
        self.at = Some((filtered + err.clamp(-max, max), app_ns));
        Outcome::Followed
    }

    /// A media re-anchor of known `step_ns`: the offset moved by it.
    pub fn shift(&mut self, step_ns: i64) {
        if let Some((offset, since)) = self.at {
            self.at = Some((offset.saturating_add(step_ns), since));
        }
        self.run = 0;
    }

    /// A media re-anchor of unknown step: the next pair is taken as the
    /// first.
    pub fn restart(&mut self) {
        *self = OffsetFilter::new();
    }

    /// `raw` taken at `app_ns`, no run of rejects.
    fn take(&mut self, raw: i64, app_ns: i64) {
        self.at = Some((raw, app_ns));
        self.run = 0;
    }
}

/// The most the filtered offset follows a pair `elapsed_ns` of app time
/// after the last one it followed: [`SLEW_PPM`] of it, rounded down; none
/// for an app time gone back.
pub fn slew_max_ns(elapsed_ns: i64) -> i64 {
    elapsed_ns.max(0) / (1_000_000 / SLEW_PPM)
}

/// One pair read app, media, app: the media clock less the midpoint of the
/// two app reads `app_before` and `app_after`, ns, and that midpoint (a
/// preemption before or after the media read skews it by half the
/// preempted time, not all of it).
pub fn pair_offset(app_before: i64, media_ns: u64, app_after: i64) -> (i64, i64) {
    let mid = app_before.saturating_add(app_after.saturating_sub(app_before) / 2);
    (crate::clock::app_offset_ns(media_ns, mid), mid)
}

#[cfg(test)]
mod tests;
