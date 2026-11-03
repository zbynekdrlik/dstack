// SPDX-License-Identifier: MIT OR Apache-2.0

//! The ASRC's controller (dstack #90, phase 2): how far the converter's
//! ratio leaves its nominal value so a buffer between two clocks stays at
//! its target. Pure: the caller feeds it observations on its own clock and
//! sets the answer on an adjustable [`crate::Converter`]
//! ([`crate::Converter::set_ppm`]).
//!
//! The laws are SongPlayer's ASIO servo (`sp-server/src/playback/
//! asrc_servo.rs`, #233), itself camera-box's `asrc-compensator` turned
//! around, made generic: two clocks and a target buffer depth, no VBAN
//! slot. SongPlayer's additions are opt-in builders: the underrun cushion
//! ([`Controller::with_cushion_max_s`]), the calm zone from the consumer's
//! callback period ([`Controller::with_calm_zone_ms`], [`calm_zone_ms_for`])
//! and the first block's priming ([`Controller::with_priming`]).
//!
//! - **Rate:** ordinary least squares over up to [`REGRESSION_SPAN_S`] of
//!   1 s window means of `(x, y)` rate points (x on our clock, y the other
//!   clock's time less ours), used once [`REGRESSION_MIN_POINTS`] span
//!   [`REGRESSION_LOCK_SPAN_S`]. A point more than [`STEP_RESIDUAL_S`] off
//!   the fit re-bases it (the next point moves the line by the step).
//! - **Level:** P [`LEVEL_KP_PPM_PER_MS`] on a [`LEVEL_EMA_TAU_S`] EMA of
//!   the window-mean depth error, at most ±[`LEVEL_KP_MAX_PPM`]; I
//!   [`LEVEL_KI_PPM_PER_MS_S`], at most ±[`LEVEL_INTEGRAL_MAX_PPM`], frozen
//!   while the sum saturates.
//! - **Offset:** a window-mean error beyond the calm zone adds
//!   [`braking_ppm`], the fastest correction that still stops at the
//!   zone's edge at the slew limit: an offset is drained by the ratio,
//!   never by a skip or an insert.
//! - **Output:** rate + offset + P + I + the outside bias, within
//!   ±[`MAX_PPM`], moved at most [`MAX_SLEW_PPM_PER_S`] per second
//!   (inaudible).
//! - **Cushion:** silence the consumer played ([`Controller::played_silence`])
//!   is kept as an excess over the target, up to the cushion's cap: the
//!   offset is braked against the band [target, target + cushion] while the
//!   level loop keeps the target. Each window the cushion follows the mean
//!   down, except a window that folded new silence keeps it; a re-centre
//!   or the priming drops it.
//! - **Stall:** a window whose own first and last rate points are more than
//!   [`MAX_SANE_WINDOW_PPM`] apart (a consumer that stopped) flushes the rate
//!   fit and holds the correction; the slew's clock runs on across it.
//! - **Last resort:** a depth more than `hard_excess_s` off the target (or
//!   under `floor_s`) asks the caller for a re-centre (insert or skip under
//!   fades), counted as a fault; the level loop's error starts over.
//! - **Outside input** (vstack rig-core's `genlock_audio_pairing`): with a
//!   [`Basis`] other than `Arrival`, the error comes from the caller
//!   ([`Controller::outside`]): a placement error against 0; and an outside
//!   bias ([`Controller::set_bias_ppm`]) of at most ±[`MAX_BIAS_PPM`].
//!
//! Sign: a positive correction makes more output per input (the consumer
//! runs fast, or too little is buffered).

use std::collections::VecDeque;

/// Bound on the applied correction, ppm.
pub const MAX_PPM: f64 = 300.0;
/// The fastest the correction moves, ppm per second.
pub const MAX_SLEW_PPM_PER_S: f64 = 5.0;
/// The rate fit's span, s.
pub const REGRESSION_SPAN_S: f64 = 600.0;
/// Points the rate fit needs before it is used.
pub const REGRESSION_MIN_POINTS: usize = 30;
/// The span those points must cover, s.
pub const REGRESSION_LOCK_SPAN_S: f64 = 60.0;
/// The most points the fit keeps.
pub const REGRESSION_CAP: usize = 640;
/// A rate point this far off the fit is a step, s.
pub const STEP_RESIDUAL_S: f64 = 0.010;
/// The level loop's P, ppm per ms of error.
pub const LEVEL_KP_PPM_PER_MS: f64 = 2.0;
/// The level loop's P bound, ppm.
pub const LEVEL_KP_MAX_PPM: f64 = 50.0;
/// The level error's EMA time constant, s.
pub const LEVEL_EMA_TAU_S: f64 = 10.0;
/// The level loop's I, ppm per ms of error per s.
pub const LEVEL_KI_PPM_PER_MS_S: f64 = 0.0002;
/// The level loop's I bound, ppm.
pub const LEVEL_INTEGRAL_MAX_PPM: f64 = 3.0;
/// The window the observations are averaged over, s.
pub const WINDOW_S: f64 = 1.0;
/// The calm zone: an offset inside it is left to the level loop, ms.
pub const CALM_ZONE_MS: f64 = 1.0;
/// Bound on the outside bias, ppm.
pub const MAX_BIAS_PPM: f64 = 10.0;
/// A window's rate beyond this is no clock but a stall, ppm.
pub const MAX_SANE_WINDOW_PPM: f64 = 100_000.0;

/// The calm zone for a consumer that takes `callback_s` a callback: half
/// of it when that is longer than [`CALM_ZONE_MS`], ms. A depth read once a
/// callback saws by a period, and the braking's steep gain near 0 would
/// otherwise move the ratio on that sawtooth (2.67 ms for 512 frames at
/// 96 kHz).
pub fn calm_zone_ms_for(callback_s: f64) -> f64 {
    (callback_s * 1000.0 / 2.0).max(CALM_ZONE_MS)
}

/// Where the error comes from (vstack rig-core's `AsrcBasis`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Basis {
    /// The buffer depth against its target ([`Controller::observe`]).
    Arrival,
    /// A placement error against 0 by timecode ([`Controller::outside`]).
    Timecode,
    /// A placement error against 0 by the source's stamps.
    Stamp,
}

/// What a rate point did to the fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offered {
    Inserted,
    /// A step once the fit has its points: the point stays out, and the next
    /// one moves the line onto it.
    Rebased,
    /// The point after a re-base: the line moved by its residual.
    Realigned,
    /// A step before the fit has its points: they start over.
    Restarted,
}

/// The rate: least squares over up to [`REGRESSION_SPAN_S`].
#[derive(Debug, Default)]
pub struct RateFit {
    points: VecDeque<(f64, f64)>,
    offset_s: f64,
    realign_next: bool,
}

impl RateFit {
    pub fn points(&self) -> usize {
        self.points.len()
    }

    fn span_s(&self) -> f64 {
        match (self.points.front(), self.points.back()) {
            (Some(a), Some(b)) => b.0 - a.0,
            _ => 0.0,
        }
    }

    /// (slope, intercept, x origin).
    fn fit(&self) -> Option<(f64, f64, f64)> {
        let x0 = self.points.front()?.0;
        let n = self.points.len() as f64;
        let mx = self.points.iter().map(|p| p.0 - x0).sum::<f64>() / n;
        let my = self.points.iter().map(|p| p.1).sum::<f64>() / n;
        let (sxx, sxy) = self.points.iter().fold((0.0, 0.0), |(sxx, sxy), p| {
            let dx = p.0 - x0 - mx;
            (sxx + dx * dx, sxy + dx * p.1)
        });
        if sxx == 0.0 {
            return None;
        }
        let slope = sxy / sxx;
        Some((slope, my - slope * mx, x0))
    }

    /// Whether it has enough points over enough time.
    pub fn locked(&self) -> bool {
        self.points.len() >= REGRESSION_MIN_POINTS && self.span_s() >= REGRESSION_LOCK_SPAN_S
    }

    /// The other clock's rate against ours, ppm, once locked; else 0.
    pub fn rate_ppm(&self) -> f64 {
        if !self.locked() {
            return 0.0;
        }
        self.fit().map_or(0.0, |(slope, _, _)| slope * 1e6)
    }

    /// One point: `x_s` on our clock, `y_raw_s` the other's time less ours.
    pub fn offer(&mut self, x_s: f64, y_raw_s: f64) -> Offered {
        let y = y_raw_s - self.offset_s;
        if self.points.len() >= REGRESSION_MIN_POINTS
            && let Some((slope, intercept, x0)) = self.fit()
        {
            let residual = y - (intercept + slope * (x_s - x0));
            if self.realign_next {
                self.offset_s += residual;
                self.realign_next = false;
                return Offered::Realigned;
            }
            if residual.abs() > STEP_RESIDUAL_S {
                self.realign_next = true;
                return Offered::Rebased;
            }
        } else if let Some(&(_, last)) = self.points.back()
            && (y - last).abs() > STEP_RESIDUAL_S
        {
            self.flush();
            self.points.push_back((x_s, y_raw_s));
            return Offered::Restarted;
        }
        self.points.push_back((x_s, y));
        let over_cap = self.points.len().saturating_sub(REGRESSION_CAP);
        self.points.drain(..over_cap);
        while let Some(&(oldest, _)) = self.points.front()
            && x_s - oldest > REGRESSION_SPAN_S
        {
            self.points.pop_front();
        }
        Offered::Inserted
    }

    /// Forgets every point.
    pub fn flush(&mut self) {
        self.points.clear();
        self.offset_s = 0.0;
        self.realign_next = false;
    }
}

/// The level loop's P + I.
#[derive(Debug, Default)]
pub struct LevelLoop {
    ema_ms: f64,
    integral_ppm: f64,
}

impl LevelLoop {
    /// P + I after a window of `dt_s` with the error `err_ms` (positive:
    /// too little buffered, more output), `base_ppm` the rest of the sum
    /// (the anti-windup counts it).
    pub fn update(&mut self, err_ms: f64, dt_s: f64, base_ppm: f64) -> f64 {
        let alpha = dt_s / (LEVEL_EMA_TAU_S + dt_s);
        self.ema_ms += alpha * (err_ms - self.ema_ms);
        let p = (LEVEL_KP_PPM_PER_MS * self.ema_ms).clamp(-LEVEL_KP_MAX_PPM, LEVEL_KP_MAX_PPM);
        if (base_ppm + p + self.integral_ppm).abs() < MAX_PPM {
            self.integral_ppm = (self.integral_ppm + LEVEL_KI_PPM_PER_MS_S * err_ms * dt_s)
                .clamp(-LEVEL_INTEGRAL_MAX_PPM, LEVEL_INTEGRAL_MAX_PPM);
        }
        p + self.integral_ppm
    }

    /// After a re-centre: the error the EMA held is gone.
    pub fn reset_error(&mut self) {
        self.ema_ms = 0.0;
    }

    pub fn ema_ms(&self) -> f64 {
        self.ema_ms
    }

    pub fn integral_ppm(&self) -> f64 {
        self.integral_ppm
    }
}

/// `applied` moved toward `target` by at most [`MAX_SLEW_PPM_PER_S`] ×
/// `dt_s` (no time, no move).
pub fn slew(applied: f64, target: f64, dt_s: f64) -> f64 {
    let step = MAX_SLEW_PPM_PER_S * dt_s.max(0.0);
    applied + (target - applied).clamp(-step, step)
}

/// The offset correction for an error `err_ms` beyond the calm zone
/// `calm_ms`: `√(2 · 5 ppm/s · (|err| − calm))` (1 ms = 1000 ppm·s), with
/// the error's sign; 0 inside the zone.
pub fn braking_ppm(err_ms: f64, calm_ms: f64) -> f64 {
    let beyond_ms = (err_ms.abs() - calm_ms).max(0.0);
    (2.0 * MAX_SLEW_PPM_PER_S * beyond_ms * 1000.0)
        .sqrt()
        .copysign(err_ms)
}

/// Whether a window's rate point, `dx` s on our clock and `dy` s of the
/// other's less ours after the last one, is a stall: `|dy / dx|` beyond
/// [`MAX_SANE_WINDOW_PPM`], written without the division (a window at the
/// last one's time is none).
pub fn stalled(dx: f64, dy: f64) -> bool {
    dx > 0.0 && dy.abs() * 1e6 > MAX_SANE_WINDOW_PPM * dx
}

/// The error the offset is braked by, ms: `err_ms` (the target less the
/// depth) measured from the band [target, target + `cushion_ms`]: a deficit
/// from the target, an excess from the band's top, 0 within it.
pub fn band_error_ms(err_ms: f64, cushion_ms: f64) -> f64 {
    err_ms.max(0.0) - (-err_ms - cushion_ms).max(0.0)
}

/// What the caller does after an observation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    /// Set this correction, ppm.
    Hold(f64),
    /// Insert (> 0) or skip (< 0) this much before going on, s (under
    /// fades); then set the correction, ppm. A fault, counted.
    Recentre { by_s: f64, ppm: f64 },
    /// The first observation ([`Controller::with_priming`]): insert this
    /// much to reach the target, s; then set the correction, ppm. No fault.
    Prime { by_s: f64, ppm: f64 },
}

/// The controller of one ASRC.
#[derive(Debug)]
pub struct Controller {
    target_s: f64,
    floor_s: f64,
    hard_excess_s: f64,
    basis: Basis,
    rate: RateFit,
    level: LevelLoop,
    applied_ppm: f64,
    bias_ppm: f64,
    /// The open window: its start, the sums of the error and of the rate
    /// point, and the count.
    window: Option<Window>,
    last_apply_s: Option<f64>,
    /// The sums of [`Controller::advance`]: our clock's time and the
    /// other's, s.
    advanced: (f64, f64),
    recentres: u64,
    rebases: u64,
    calm_ms: f64,
    cushion_max_s: f64,
    cushion_s: f64,
    /// The highest count of the consumer's silence seen, s (`None`: no
    /// count read yet).
    silence_s: Option<f64>,
    /// New silence was folded since the open window started.
    folded: bool,
    /// [`Controller::with_priming`] was asked for.
    priming: bool,
    /// The next observation primes.
    prime_next: bool,
    /// The last window's mean error outside the band, ms (positive: later).
    offset_ms: f64,
    stalls: u64,
}

#[derive(Debug, Clone, Copy)]
struct Window {
    start_s: f64,
    err_ms: f64,
    x_s: f64,
    y_s: f64,
    points: f64,
    n: f64,
    /// The window's first and last rate points (the stall check).
    first: Option<(f64, f64)>,
    last: (f64, f64),
}

impl Controller {
    /// A controller keeping a buffer at `target_s`, re-centring under
    /// `floor_s` or more than `hard_excess_s` off the target.
    pub fn new(target_s: f64, floor_s: f64, hard_excess_s: f64, basis: Basis) -> Self {
        Controller {
            target_s,
            floor_s,
            hard_excess_s,
            basis,
            rate: RateFit::default(),
            level: LevelLoop::default(),
            applied_ppm: 0.0,
            bias_ppm: 0.0,
            window: None,
            last_apply_s: None,
            advanced: (0.0, 0.0),
            recentres: 0,
            rebases: 0,
            calm_ms: CALM_ZONE_MS,
            cushion_max_s: 0.0,
            cushion_s: 0.0,
            silence_s: None,
            folded: false,
            priming: false,
            prime_next: false,
            offset_ms: 0.0,
            stalls: 0,
        }
    }

    /// Keeps up to `max_s` of the excess the consumer's silence leaves
    /// ([`Controller::played_silence`]); 0 (the default) keeps none.
    pub fn with_cushion_max_s(mut self, max_s: f64) -> Self {
        self.cushion_max_s = max_s.max(0.0);
        self
    }

    /// The calm zone, ms ([`CALM_ZONE_MS`] by default; [`calm_zone_ms_for`]
    /// gives it from the consumer's callback period).
    pub fn with_calm_zone_ms(mut self, ms: f64) -> Self {
        self.calm_ms = ms.max(0.0);
        self
    }

    /// The first observation inserts up to the target ([`Step::Prime`]),
    /// not counted as a re-centre; [`Controller::restart`] primes again.
    pub fn with_priming(mut self) -> Self {
        self.priming = true;
        self.prime_next = true;
        self
    }

    /// After the consumer reopened: the next observation primes (with
    /// [`Controller::with_priming`]), the window, the level loop's error
    /// and the cushion start over; the rate fit is kept.
    pub fn restart(&mut self) {
        self.prime_next = self.priming;
        self.window = None;
        self.level.reset_error();
        self.cushion_s = 0.0;
        self.silence_s = None;
        self.folded = false;
    }

    /// The consumer's silence so far, `total_s` (cumulative): what is new
    /// since the last count joins the cushion, up to its cap. The first
    /// count is the start; only what passes the highest count seen is new
    /// (a count that went back folds nothing until it passes it again).
    pub fn played_silence(&mut self, total_s: f64) {
        let new = self.silence_s.map_or(0.0, |high| (total_s - high).max(0.0));
        self.silence_s = Some(self.silence_s.map_or(total_s, |high| high.max(total_s)));
        if new > 0.0 {
            self.cushion_s = (self.cushion_s + new).min(self.cushion_max_s);
            self.folded = true;
        }
    }

    /// The excess kept over the target, s.
    pub fn cushion_s(&self) -> f64 {
        self.cushion_s
    }

    /// The last window's mean depth outside [target, target + cushion], ms:
    /// positive over it (later), negative under the target, 0 within.
    pub fn offset_ms(&self) -> f64 {
        self.offset_ms
    }

    /// Windows read as a stall (the rate fit flushed).
    pub fn stalls(&self) -> u64 {
        self.stalls
    }

    /// The correction it last gave, ppm.
    pub fn ppm(&self) -> f64 {
        self.applied_ppm
    }

    /// The other clock's rate against ours, ppm (0 until locked).
    pub fn rate_ppm(&self) -> f64 {
        self.rate.rate_ppm()
    }

    /// Whether the rate fit is locked.
    pub fn locked(&self) -> bool {
        self.rate.locked()
    }

    /// Re-centres so far (faults).
    pub fn recentres(&self) -> u64 {
        self.recentres
    }

    /// Steps the rate fit re-based on.
    pub fn rebases(&self) -> u64 {
        self.rebases
    }

    /// Whether the correction sits at its bound (±[`MAX_PPM`]): the drift is
    /// more than the ASRC can follow (vstack's `asrc_saturated`).
    pub fn saturated(&self) -> bool {
        self.applied_ppm.abs() >= MAX_PPM
    }

    /// A rate point as two advances over the same stretch (vstack's
    /// `audio_asrc_rate_point`): `ours_s` on our clock (the master's stamp
    /// advance) and `other_s` on the other (the raw duration covered). The
    /// sums give the point `(Σours, Σother − Σours)` of the rate fit.
    pub fn advance(&mut self, ours_s: f64, other_s: f64) -> Offered {
        self.advanced.0 += ours_s;
        self.advanced.1 += other_s;
        let (x, y) = self.advanced;
        let offered = self.rate.offer(x, y - x);
        if offered == Offered::Rebased {
            self.rebases += 1;
        }
        offered
    }

    /// The outside bias, ppm, at most ±[`MAX_BIAS_PPM`].
    pub fn set_bias_ppm(&mut self, ppm: f64) {
        self.bias_ppm = ppm.clamp(-MAX_BIAS_PPM, MAX_BIAS_PPM);
    }

    /// One observation at `now_s` on our clock: the buffer's depth
    /// `depth_s`, and the rate point `(now_s, other_s - now_s)`, `other_s`
    /// the other clock's time (its frames consumed / its rate).
    pub fn observe(&mut self, now_s: f64, depth_s: f64, other_s: f64) -> Step {
        let err_s = self.target_s - depth_s;
        if self.prime_next {
            // The first block goes to the target (an insert under it, a
            // skip over it) and is no part of the first window.
            self.prime_next = false;
            self.cushion_s = 0.0;
            return Step::Prime {
                by_s: err_s,
                ppm: self.applied_ppm,
            };
        }
        if depth_s < self.floor_s || err_s.abs() > self.hard_excess_s {
            return self.recentre(err_s);
        }
        let err_ms = match self.basis {
            Basis::Arrival => err_s * 1000.0,
            // The placement error comes through `outside`; the depth only
            // guards the floor and the excess.
            Basis::Timecode | Basis::Stamp => 0.0,
        };
        self.add(now_s, err_ms, Some(other_s - now_s))
    }

    /// An outside placement error at `now_s` (positive: our audio is early,
    /// more output), for a [`Basis`] other than `Arrival`.
    pub fn outside(&mut self, now_s: f64, error_ms: f64) -> Step {
        self.add(now_s, error_ms, None)
    }

    fn add(&mut self, now_s: f64, err_ms: f64, y: Option<f64>) -> Step {
        // A clock that went back before the window's start: it starts over
        // (it would stay open for the step and a second more).
        if self.window.is_some_and(|w| now_s < w.start_s) {
            self.window = None;
        }
        let w = self.window.get_or_insert(Window {
            start_s: now_s,
            err_ms: 0.0,
            x_s: 0.0,
            y_s: 0.0,
            points: 0.0,
            n: 0.0,
            first: None,
            last: (0.0, 0.0),
        });
        w.err_ms += err_ms;
        w.n += 1.0;
        if let Some(y) = y {
            w.x_s += now_s;
            w.y_s += y;
            w.points += 1.0;
            w.first.get_or_insert((now_s, y));
            w.last = (now_s, y);
        }
        if now_s - w.start_s < WINDOW_S {
            return Step::Hold(self.applied_ppm);
        }
        let w = *w;
        self.window = None;
        let folded = std::mem::take(&mut self.folded);
        // A window whose own first and last points are too far apart is a
        // stall: the fit is flushed, the correction held, and the slew's
        // clock kept, so the next window moves by the whole time since.
        if let Some(first) = w.first
            && stalled(w.last.0 - first.0, w.last.1 - first.1)
        {
            self.rate.flush();
            self.stalls += 1;
            return Step::Hold(self.applied_ppm);
        }
        let dt_s = self
            .last_apply_s
            .map_or(now_s - w.start_s, |prev| now_s - prev)
            .max(0.0);
        self.last_apply_s = Some(now_s);
        if w.points > 0.0 && self.rate.offer(w.x_s / w.points, w.y_s / w.points) == Offered::Rebased
        {
            self.rebases += 1;
        }
        let err_ms = w.err_ms / w.n;
        // The excess over the target the window held: the cushion follows
        // it down, unless the window folded new silence.
        if !folded {
            self.cushion_s = self.cushion_s.min((-err_ms / 1000.0).max(0.0));
        }
        let band_err_ms = band_error_ms(err_ms, self.cushion_s * 1000.0);
        self.offset_ms = -band_err_ms;
        let base = self.rate.rate_ppm() + braking_ppm(band_err_ms, self.calm_ms) + self.bias_ppm;
        let pi = self.level.update(err_ms, dt_s, base);
        let target = (base + pi).clamp(-MAX_PPM, MAX_PPM);
        self.applied_ppm = slew(self.applied_ppm, target, dt_s);
        Step::Hold(self.applied_ppm)
    }

    fn recentre(&mut self, err_s: f64) -> Step {
        self.window = None;
        self.level.reset_error();
        self.cushion_s = 0.0;
        self.recentres += 1;
        Step::Recentre {
            by_s: err_s,
            ppm: self.applied_ppm,
        }
    }
}

#[cfg(test)]
mod tests;
