// SPDX-License-Identifier: MIT OR Apache-2.0

//! An ASIO host's decisions, pure: the [`crate::Runner`] and the Windows
//! glue (`win`) only act on them. Lifted from SongPlayer's `asio_state.rs`
//! (songplayer #233 and #229, dstack #243); times are ns on a monotonic
//! clock.
//!
//! - **Reopen backoff** 2 / 10 / 30 / 60 / 60 … s after a failed open or a
//!   close; a run of 60 s resets it, so a reset after a long run waits 2 s
//!   (some virtual drivers stay busy for seconds after a release).
//! - **A close:** a reset request (or a buffer-size change, which is answered
//!   0 so the driver then asks a reset: a host never resizes live), a rate
//!   change of 1 Hz or more (Dante Controller or SoundGrid re-clocked the
//!   card; under 1 Hz, or ASE_NoClock, is a lost clock: [`lost_clock`]), or
//!   no callback for 2 s from a driver that ticked (a vanished driver).
//! - **No clock:** a driver that opens and does not tick gives none (DVS not
//!   running, no PTP leader on its network): no close, no reset, no backoff;
//!   the host waits for it from 2 s after the open and opens the driver
//!   again every 60 s ([`clock_step`]; SongPlayer's owner ruling of
//!   8.10.2026).
//! - **Off rate:** a driver that ticks far from the rate it reports (Waves
//!   SoundGrid with its network off: 44 100 Hz reported, ~2.9 % more taken)
//!   is no usable clock either: [`RateWatch`] compares the frames the card
//!   took with the clock over windows of at least 10 s (songplayer #229).
//! - **asioMessage replies** ([`reply`]): resets (by reopening), resyncs,
//!   latency changes and time info are supported; a buffer-size change is
//!   answered 0.
//! - The driver's rate, buffer and sample type are read, never set: the rate
//!   is admitted (8–384 kHz, [`admit_rate`]).

/// The reopen backoff, s.
pub const BACKOFF_S: [i64; 4] = [2, 10, 30, 60];
/// A run this long resets the backoff (60 s), ns.
pub const STABLE_RUN_NS: i64 = 60_000_000_000;
/// No callback for this long while running: the driver is gone (2 s), ns.
pub const STALL_NS: i64 = 2_000_000_000;
/// No callback this long after an open: the driver gives no clock, and the
/// host waits for it, calmly (2 s), ns.
pub const NO_CLOCK_NS: i64 = 2_000_000_000;
/// Still no clock this long after an open: the driver is closed and opened
/// again at once, every 60 s (no backoff: a fixed, slow cadence), ns.
pub const NO_CLOCK_REOPEN_NS: i64 = 60_000_000_000;
/// A host's worker steps at least this often (the driver's messages,
/// 10 ms), ns.
pub const POLL_NS: i64 = 10_000_000;
pub const MIN_RATE: f64 = 8_000.0;
pub const MAX_RATE: f64 = 384_000.0;

/// `frames` at `rate` Hz in ns, rounded (a rate under 1 Hz reads as 1 Hz).
pub fn frames_to_ns(frames: i64, rate: f64) -> i64 {
    (frames as f64 * 1e9 / rate.max(1.0)).round() as i64
}

/// The wait before the next open after `failures` failed opens or closes in
/// a row (at least the first step), ns.
pub fn backoff_ns(failures: u32) -> i64 {
    let i = (failures.max(1) as usize - 1).min(BACKOFF_S.len() - 1);
    BACKOFF_S[i] * 1_000_000_000
}

/// The failure count after a close of a run that lasted `ran_ns`.
pub fn failures_after_close(failures: u32, ran_ns: i64) -> u32 {
    if ran_ns >= STABLE_RUN_NS {
        1
    } else {
        failures.saturating_add(1)
    }
}

/// Why a host's driver is not running (with the `serde` feature: the same
/// shape SongPlayer's `asio_state::Reason` sends over its host pipe, less
/// its own `HostDown`).
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Reason {
    /// No registered driver has that name; the ones present.
    NotFound { present: Vec<String> },
    /// The driver refused its init (another program holds it?).
    Busy(String),
    /// The driver runs at something we do not take.
    Refused(String),
    /// A driver call failed.
    Failed(String),
    /// The driver asked for a reset (or a buffer-size change).
    Reset,
    /// The driver's rate changed to this (0: it lost its clock).
    RateChanged(u32),
    /// No callback for 2 s from a driver that ticked.
    Stalled,
    /// ASIO runs on Windows only.
    WindowsOnly,
    /// Another device of this process still holds the driver.
    Held,
    /// A driver callback never returned: the driver is parked until the
    /// process ends, no open can succeed.
    Parked,
    /// The driver opened but gives no clock (no callback since the open).
    NoClock,
    /// The driver ticks, but the card takes audio more than 1 % off the rate
    /// it reports: no usable clock.
    ClockOffRate,
}

impl Reason {
    /// A stable code (SongPlayer's dashboard maps these to Slovak).
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "not_found",
            Self::Busy(_) => "busy",
            Self::Refused(_) => "refused",
            Self::Failed(_) => "failed",
            Self::Reset => "reset",
            Self::RateChanged(0) => "clock_lost",
            Self::RateChanged(_) => "rate_changed",
            Self::Stalled => "stalled",
            Self::WindowsOnly => "windows_only",
            Self::Held => "held",
            Self::Parked => "parked",
            Self::NoClock => "no_clock",
            Self::ClockOffRate => "clock_off_rate",
        }
    }

    /// The reason in words (English, for a log or an API).
    pub fn text(&self) -> String {
        match self {
            Self::NotFound { present } if present.is_empty() => {
                "the driver is not registered (present: none)".into()
            }
            Self::NotFound { present } => format!(
                "the driver is not registered (present: {})",
                present.join(", ")
            ),
            Self::Busy(e) => {
                format!("the driver refused to start (in use by another program?): {e}")
            }
            Self::Refused(e) | Self::Failed(e) => e.clone(),
            Self::Reset => "the driver asked for a reset".into(),
            Self::RateChanged(0) => "the driver lost its clock (no rate)".into(),
            Self::RateChanged(r) => format!("the driver's rate changed to {r} Hz"),
            Self::Stalled => "no callback from the driver for 2 s".into(),
            Self::WindowsOnly => "ASIO runs on Windows only".into(),
            Self::Held => "another output of this process still holds the driver".into(),
            Self::Parked => {
                "a driver callback did not return for 1 s: the driver is parked until the \
                 process restarts"
                    .into()
            }
            Self::NoClock => {
                "the driver gives no clock (no callback since it opened): e.g. the virtual \
                 soundcard is not running, or its network has no clock leader"
                    .into()
            }
            Self::ClockOffRate => {
                "the card takes audio more than 1 % off the rate its driver reports: no usable \
                 clock (e.g. Waves SoundGrid with its network off)"
                    .into()
            }
        }
    }
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text())
    }
}

/// What the driver said since the last poll (counted by its callbacks).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DeviceEvents {
    pub reset: bool,
    pub resync: bool,
    pub buffer_size_change: bool,
    pub latencies_changed: bool,
    /// The last rate `sampleRateDidChange` reported (under 1 Hz: a lost
    /// clock, [`lost_clock`]).
    pub rate_changed: Option<f64>,
    pub overloads: u64,
    /// Buffer switches since the start (monotonic).
    pub callbacks: u64,
    /// The longest time between two buffer switches since the last poll, ns
    /// (0 when none came after another).
    pub callback_gap_max_ns: i64,
    /// Buffer switches that came [`GapKind::Late`] after the one before,
    /// since the start (monotonic).
    pub late_callbacks: u64,
    /// Buffer switches that came [`GapKind::Fast`] (a catch-up burst), since
    /// the start (monotonic).
    pub fast_callbacks: u64,
    /// Panics of the app's `Process` since the start (it plays silence from
    /// then on).
    pub faults: u64,
}

/// Whether the events close a run opened at `opened_rate`.
pub fn close_reason(ev: &DeviceEvents, opened_rate: f64) -> Option<Reason> {
    if ev.reset || ev.buffer_size_change {
        return Some(Reason::Reset);
    }
    match ev.rate_changed {
        Some(r) if (r - opened_rate).abs() >= 1.0 => Some(rate_change(r)),
        _ => None,
    }
}

/// What a running driver does about its clock, at one step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockStep {
    /// It ticks (a stall is the stall watch's).
    Ticking,
    /// It ticks after a wait for its clock: it runs (one INFO).
    ClockArrived,
    /// No callback yet, nothing to do now.
    Quiet,
    /// No callback [`NO_CLOCK_NS`] after the open: the host waits for its
    /// clock, the driver kept open (one WARN per wait).
    StartsWaiting,
    /// No callback [`NO_CLOCK_REOPEN_NS`] after the open: closed and opened
    /// again at once (no reset, no backoff).
    Reopen,
}

/// The clock decision of a run whose driver ticked `ticks` times since its
/// tick base, `primed` (the app's first data is in; an app with no priming
/// passes true), opened `since_open_ns` ago, `waiting` for its clock or not.
/// A tick before the priming while waiting (a burst at a reopen) is no
/// clock; before the priming of a first open it is a tick.
pub fn clock_step(ticks: u64, primed: bool, waiting: bool, since_open_ns: i64) -> ClockStep {
    if ticks > 0 {
        return match (waiting, primed) {
            (false, _) => ClockStep::Ticking,
            (true, true) => ClockStep::ClockArrived,
            (true, false) => ClockStep::Quiet,
        };
    }
    if since_open_ns >= NO_CLOCK_REOPEN_NS {
        ClockStep::Reopen
    } else if since_open_ns >= NO_CLOCK_NS && !waiting {
        ClockStep::StartsWaiting
    } else {
        ClockStep::Quiet
    }
}

/// A rate window lasts at least this long (10 s), ns.
pub const RATE_WINDOW_NS: i64 = 10_000_000_000;
/// ... and at least this many driver buffers: the consumed count moves in
/// whole buffers, so one buffer stays under 0.2 % of a window.
pub const RATE_WINDOW_BUFFERS: i64 = 500;
/// A card more than this far off its reported rate is no usable clock (1 %;
/// a healthy card is within ±300 ppm, SoundGrid offline ~29 000 ppm).
pub const OFF_RATE_PPM: f64 = 10_000.0;
/// Off-rate windows in a row before a playing run stops for it.
pub const OFF_RATE_STRIKES_PLAYING: u32 = 2;
/// A gap in the callbacks longer than this (or than [`RATE_GAP_PERIODS`]
/// of its buffers) starts the rate windows again, ns.
pub const RATE_GAP_MIN_NS: i64 = 100_000_000;
/// ... in buffer periods.
pub const RATE_GAP_PERIODS: i64 = 3;
/// A run's rate windows start once its card took a quarter second of audio
/// since its tick base: the open's first callbacks (a burst) never weigh in.
pub const RATE_SETTLE_PER_S: f64 = 4.0;

/// The longest gap in a card's callbacks its rate windows read through:
/// 100 ms, or three of its buffers when that is longer, ns.
pub fn rate_gap_limit_ns(buffer_frames: u32, rate: f64) -> i64 {
    frames_to_ns(RATE_GAP_PERIODS * i64::from(buffer_frames), rate).max(RATE_GAP_MIN_NS)
}

/// Whether `ticks` frames at `rate` are enough to start the rate windows.
pub fn rate_settled(ticks: u64, rate: f64) -> bool {
    ticks as f64 * RATE_SETTLE_PER_S >= rate
}

/// The length of a rate window of a driver with `buffer_frames` at `rate`,
/// ns.
pub fn rate_window_ns(buffer_frames: u32, rate: f64) -> i64 {
    frames_to_ns(RATE_WINDOW_BUFFERS * i64::from(buffer_frames), rate).max(RATE_WINDOW_NS)
}

/// How far `frames` taken over `span_ns` are off `rate`, ppm (positive: the
/// card takes more than its rate).
pub fn consumed_ppm(frames: u64, span_ns: i64, rate: f64) -> f64 {
    let taken_hz = frames as f64 * 1e9 / span_ns as f64;
    (taken_hz / rate - 1.0) * 1e6
}

/// What a [`RateWatch`] step found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RateStep {
    /// A window runs (or an off window waits for the next one).
    Measuring,
    /// A window closed within [`OFF_RATE_PPM`].
    Plausible,
    /// `strikes` windows in a row closed beyond it.
    OffRate { ppm: f64 },
}

/// The card's consumed rate against its reported one, over back-to-back
/// windows of [`rate_window_ns`].
#[derive(Clone, Copy, Debug, Default)]
pub struct RateWatch {
    from: Option<(u64, i64)>,
    strikes: u32,
    /// The consumed count when it last moved, and when.
    moved: Option<(u64, i64)>,
}

impl RateWatch {
    /// Follow the consumed count at every step: a gap in the callbacks over
    /// [`rate_gap_limit_ns`] (a stall) starts the windows and the strikes
    /// again, so a stall across a window's edge never reads as two windows
    /// off.
    pub fn track(&mut self, consumed: u64, now_ns: i64, (rate, buffer_frames): (f64, u32)) {
        match self.moved {
            Some((frames, _)) if frames == consumed => {}
            Some((_, at)) if now_ns - at > rate_gap_limit_ns(buffer_frames, rate) => {
                self.from = None;
                self.strikes = 0;
                self.moved = Some((consumed, now_ns));
            }
            _ => self.moved = Some((consumed, now_ns)),
        }
    }

    /// The card took `consumed` frames (since its start) by `now_ns`, its
    /// driver at `(rate, buffer_frames)`; `strikes` off windows in a row
    /// make the verdict.
    pub fn step(
        &mut self,
        consumed: u64,
        now_ns: i64,
        (rate, buffer_frames): (f64, u32),
        strikes: u32,
    ) -> RateStep {
        let Some((from_frames, from_ns)) = self.from else {
            self.from = Some((consumed, now_ns));
            return RateStep::Measuring;
        };
        let span = now_ns - from_ns;
        if span < 0 {
            self.from = Some((consumed, now_ns));
            return RateStep::Measuring;
        }
        if span < rate_window_ns(buffer_frames, rate) {
            return RateStep::Measuring;
        }
        self.from = Some((consumed, now_ns));
        let ppm = consumed_ppm(consumed.saturating_sub(from_frames), span, rate);
        if ppm.abs() <= OFF_RATE_PPM {
            self.strikes = 0;
            return RateStep::Plausible;
        }
        self.strikes += 1;
        if self.strikes >= strikes {
            RateStep::OffRate { ppm }
        } else {
            RateStep::Measuring
        }
    }
}

/// No callback for [`STALL_NS`].
#[derive(Debug, Default)]
pub struct StallWatch {
    last: Option<(u64, i64)>,
}

impl StallWatch {
    /// `callbacks` (monotonic) read at `now_ns`: whether none came for
    /// [`STALL_NS`].
    pub fn stalled(&mut self, callbacks: u64, now_ns: i64) -> bool {
        match self.last {
            Some((count, since)) if count == callbacks => now_ns - since >= STALL_NS,
            _ => {
                self.last = Some((callbacks, now_ns));
                false
            }
        }
    }
}

/// How a buffer switch came after the one before, against the driver's
/// buffer period.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GapKind {
    /// Under a quarter of a period: a catch-up burst.
    Fast,
    Normal,
    /// Over two periods: the driver called back late.
    Late,
}

/// The kind of a gap of `gap_ns` between two buffer switches of a driver
/// whose buffer lasts `period_ns`: over two periods late, under a quarter
/// fast, both bounds themselves normal.
pub fn callback_gap_kind(gap_ns: i64, period_ns: i64) -> GapKind {
    if gap_ns > 2 * period_ns {
        GapKind::Late
    } else if 4 * gap_ns < period_ns {
        GapKind::Fast
    } else {
        GapKind::Normal
    }
}

/// The ASIO driver-to-host message selectors (asio.h `kAsio…`).
pub mod selector {
    pub const SELECTOR_SUPPORTED: i32 = 1;
    pub const ENGINE_VERSION: i32 = 2;
    pub const RESET_REQUEST: i32 = 3;
    pub const BUFFER_SIZE_CHANGE: i32 = 4;
    pub const RESYNC_REQUEST: i32 = 5;
    pub const LATENCIES_CHANGED: i32 = 6;
    pub const SUPPORTS_TIME_INFO: i32 = 7;
    pub const SUPPORTS_TIME_CODE: i32 = 8;
    pub const OVERLOAD: i32 = 15;
}

/// The host's answer to `asioMessage`: it supports resets (by reopening),
/// resyncs and time info; it never resizes buffers live, so a size change
/// is answered 0.
pub fn reply(sel: i32, value: i32) -> i32 {
    use selector::*;
    match sel {
        SELECTOR_SUPPORTED => i32::from(matches!(
            value,
            ENGINE_VERSION
                | RESET_REQUEST
                | BUFFER_SIZE_CHANGE
                | RESYNC_REQUEST
                | LATENCIES_CHANGED
                | SUPPORTS_TIME_INFO
                | OVERLOAD
        )),
        ENGINE_VERSION => 2,
        RESET_REQUEST | RESYNC_REQUEST | LATENCIES_CHANGED | SUPPORTS_TIME_INFO => 1,
        _ => 0,
    }
}

/// What a reported rate stands for: a lost clock under 1 Hz, else the new
/// rate (rounded).
fn rate_change(rate: f64) -> Reason {
    if lost_clock(rate) {
        Reason::RateChanged(0)
    } else {
        Reason::RateChanged(rate.round() as u32)
    }
}

/// HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND): a registry key that does not
/// exist.
pub const HRESULT_FILE_NOT_FOUND: i32 = 0x8007_0002_u32 as i32;
/// HRESULT_FROM_WIN32(ERROR_PATH_NOT_FOUND).
pub const HRESULT_PATH_NOT_FOUND: i32 = 0x8007_0003_u32 as i32;

/// Whether a failed read of `HKLM\SOFTWARE\ASIO` only says the key is
/// absent (no ASIO driver was ever installed): an empty driver list. Any
/// other failure (access denied, a broken hive) is an error, never an empty
/// list a caller would read as "no driver on this PC".
pub fn registry_key_absent(hresult: i32) -> bool {
    hresult == HRESULT_FILE_NOT_FOUND || hresult == HRESULT_PATH_NOT_FOUND
}

/// ASIO's ASE_NoClock (asio.h): the driver has no clock.
pub const ASE_NO_CLOCK: i32 = -995;

/// Under 1 Hz a driver has no clock (0 Hz, a fraction).
pub fn lost_clock(rate: f64) -> bool {
    rate.abs() < 1.0
}

/// What a failed `getSampleRate` stands for when it is one of ASIO's own
/// codes: ASE_NoClock is a lost clock; `None` leaves any other error to the
/// glue.
pub fn sample_rate_error(code: i32) -> Option<Reason> {
    (code == ASE_NO_CLOCK).then_some(Reason::RateChanged(0))
}

/// The driver's rate, as the host follows it (never set). Under 1 Hz the
/// driver has no clock.
pub fn admit_rate(rate: f64) -> Result<u32, Reason> {
    if lost_clock(rate) {
        return Err(Reason::RateChanged(0));
    }
    if rate.is_finite() && (MIN_RATE..=MAX_RATE).contains(&rate) {
        Ok(rate.round() as u32)
    } else {
        Err(Reason::Refused(format!("the driver reports {rate} Hz")))
    }
}

#[cfg(test)]
mod tests;
