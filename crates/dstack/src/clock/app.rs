// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The app's own clock (dstack #295): the clock an application such as OBS
//! stamps its audio with, and the media clock's offset from it, so a
//! block's play time ([`dstack_audio_io::Block::play_at`]) is in it.

/// The app's monotonic clock, ns: Linux's `CLOCK_MONOTONIC`, Windows' raw
/// QPC (ticks as ns, rounded down, with no steer model, unlike the media
/// clock's own monotonic there): the clock OBS's `os_gettime_ns` reads.
#[cfg(any(target_os = "linux", windows))]
pub use super::sys::app_monotonic_ns;

/// QPC's `ticks` at `frequency` ticks a second as ns, rounded down (a
/// frequency of 0 or less reads as 1): the conversion OBS's
/// `os_gettime_ns` makes on Windows (`util_mul_div64(ticks, 10^9, freq)`),
/// with no steer model, so [`app_monotonic_ns`] is the app's clock there.
/// `i128`, so a counter's whole range never overflows. Windows reads it;
/// the tests pin it on every OS.
#[cfg(any(windows, test))]
pub(crate) fn qpc_ns_of(ticks: i64, frequency: i64) -> i64 {
    (i128::from(ticks) * 1_000_000_000 / i128::from(frequency.max(1))) as i64
}

/// The media clock less the app's, ns, from a pair read back to back:
/// `media_ns` (the media clock) and `app_ns` ([`app_monotonic_ns`]). A
/// media time minus it is that time in the app's clock. Saturating at
/// both ends.
pub(crate) fn app_offset_ns(media_ns: u64, app_ns: i64) -> i64 {
    i64::try_from(media_ns)
        .unwrap_or(i64::MAX)
        .saturating_sub(app_ns)
}
