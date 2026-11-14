// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! TEST ONLY: the system clock with a skew (dstack #305). A CI runner has no
//! grandmaster, so the TEST system clock (`host::ClockSource::System`) runs
//! at the machine's own rate: 0 ppm against an app's clock, while the field
//! has about 6 ppm. With a `test_clock_ppm` the media clock runs that many
//! ppm off the local monotonic clock from its start, so play_at, an app's
//! ASRC and dlink's TX Player see real drift, and two processes on one
//! runner drift against each other.
//!
//! PTP ns = `base + k + (m - base) × (1 + ppm × 10⁻⁶)`, `m` the monotonic
//! clock, `base` its read at the start and `k` the system clock's offset
//! (realtime − monotonic at the start, as the unskewed clock). The fields
//! never change after the start: a read takes no lock and no atomic.
//! The arithmetic is in parts per 10¹² ([`TestClockPpm::ppt`]) and `i128`.

use serde::Deserialize;

/// The largest skew a test clock takes, ppm either way: far beyond a real
/// clock's (Dante's ±100 ppm pull-in), small enough that no stamp
/// arithmetic nears an overflow.
pub const MAX_TEST_CLOCK_PPM: f64 = 1000.0;

/// Parts per 10¹² in one ppm.
const PPT_PER_PPM: f64 = 1_000_000.0;
/// 10¹².
const TERA: i128 = 1_000_000_000_000;

/// A test clock's skew, ppm (the config's `test_clock_ppm`): positive runs
/// the media clock fast against the monotonic clock. Compared by its bits
/// so configs stay `Eq`.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(transparent)]
pub struct TestClockPpm(pub f64);

impl PartialEq for TestClockPpm {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for TestClockPpm {}

impl TestClockPpm {
    /// The config's problem with it: finite and within
    /// [`MAX_TEST_CLOCK_PPM`] either way, else the rule.
    pub fn problem(self) -> Option<String> {
        (!(self.0.is_finite() && self.0.abs() <= MAX_TEST_CLOCK_PPM)).then(|| {
            format!("test_clock_ppm: a number from -{MAX_TEST_CLOCK_PPM} to {MAX_TEST_CLOCK_PPM}")
        })
    }

    /// The skew in parts per 10¹², rounded (a valid skew: [`problem`] is
    /// `None`).
    ///
    /// [`problem`]: TestClockPpm::problem
    pub fn ppt(self) -> i64 {
        // A valid skew is at most 10⁹ ppt: the cast never saturates.
        (self.0 * PPT_PER_PPM).round() as i64
    }
}

/// The skewed time at monotonic `m_ns`: `base + k + (m - base) × (1 + ppt
/// × 10⁻¹²)`, the skew's part rounded toward minus infinity. `None` when
/// it falls outside `u64` (before the epoch: a clock not ready).
pub fn skewed_ns(base_ns: i64, k_ns: i64, ppt: i64, m_ns: i64) -> Option<u64> {
    let elapsed = i128::from(m_ns) - i128::from(base_ns);
    let skew = (elapsed * i128::from(ppt)).div_euclid(TERA);
    u64::try_from(i128::from(m_ns) + i128::from(k_ns) + skew).ok()
}

/// The first monotonic time whose [`skewed_ns`] is at least `ptp_ns`
/// (the time the skewed clock's `sleep_until` sleeps to), saturating at
/// `i64::MAX`.
pub fn unskewed_ns(base_ns: i64, k_ns: i64, ppt: i64, ptp_ns: u64) -> i64 {
    // ptp = base + k + e × (T + ppt) / T, e = m - base: e = (ptp - base -
    // k) × T / (T + ppt), rounded up so the skewed time is not short of it.
    let rest = i128::from(ptp_ns) - i128::from(base_ns) - i128::from(k_ns);
    let elapsed = (rest * TERA).div_euclid(TERA + i128::from(ppt))
        + i128::from((rest * TERA).rem_euclid(TERA + i128::from(ppt)) != 0);
    i64::try_from(i128::from(base_ns) + elapsed).unwrap_or(i64::MAX)
}

/// The skewed TEST clock ([`crate::clock::PtpClock`]): ready from its
/// start, never re-anchored (epoch 0).
#[cfg(any(target_os = "linux", windows))]
#[derive(Debug, Clone, Copy)]
pub struct SkewedClock {
    base_ns: i64,
    k_ns: i64,
    ppt: i64,
}

#[cfg(any(target_os = "linux", windows))]
impl SkewedClock {
    /// The clock from monotonic `base_ns` on, at `k_ns` (PTP − monotonic
    /// there) and `ppm` off the monotonic clock.
    pub fn new(base_ns: i64, k_ns: i64, ppm: TestClockPpm) -> Self {
        SkewedClock {
            base_ns,
            k_ns,
            ppt: ppm.ppt(),
        }
    }
}

#[cfg(any(target_os = "linux", windows))]
impl super::PtpClock for SkewedClock {
    fn now_ns(&self) -> Option<u64> {
        skewed_ns(self.base_ns, self.k_ns, self.ppt, super::monotonic_ns())
    }

    fn sleep_until(&self, ptp_ns: u64) {
        super::sys::sleep_until_monotonic(unskewed_ns(self.base_ns, self.k_ns, self.ppt, ptp_ns));
    }

    fn epoch(&self) -> u32 {
        0
    }
}

#[cfg(test)]
mod tests;
