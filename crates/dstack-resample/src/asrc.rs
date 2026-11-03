// SPDX-License-Identifier: GPL-3.0-or-later

//! The ASRC's controller (dstack #90, phase 2): how far the converter's
//! ratio leaves its nominal value so a buffer between two clocks stays at
//! its target. Pure: the caller feeds it observations on its own clock and
//! sets the answer on an adjustable [`crate::Converter`]
//! ([`crate::Converter::set_ppm`]).
//!
//! The laws are SongPlayer's ASIO servo (`sp-server/src/playback/
//! asrc_servo.rs`, #233), itself camera-box's `asrc-compensator` turned
//! around, made generic: two clocks and a target buffer depth, no VBAN slot
//! and no underrun cushion.
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

/// What the caller does after an observation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    /// Set this correction, ppm.
    Hold(f64),
    /// Insert (> 0) or skip (< 0) this much before going on, s (under
    /// fades); then set the correction, ppm. A fault, counted.
    Recentre { by_s: f64, ppm: f64 },
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
}

#[derive(Debug, Clone, Copy)]
struct Window {
    start_s: f64,
    err_ms: f64,
    x_s: f64,
    y_s: f64,
    points: f64,
    n: f64,
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
        }
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
        let w = self.window.get_or_insert(Window {
            start_s: now_s,
            err_ms: 0.0,
            x_s: 0.0,
            y_s: 0.0,
            points: 0.0,
            n: 0.0,
        });
        w.err_ms += err_ms;
        w.n += 1.0;
        if let Some(y) = y {
            w.x_s += now_s;
            w.y_s += y;
            w.points += 1.0;
        }
        if now_s - w.start_s < WINDOW_S {
            return Step::Hold(self.applied_ppm);
        }
        let w = *w;
        self.window = None;
        if w.points > 0.0 && self.rate.offer(w.x_s / w.points, w.y_s / w.points) == Offered::Rebased
        {
            self.rebases += 1;
        }
        let err_ms = w.err_ms / w.n;
        let dt_s = self
            .last_apply_s
            .map_or(now_s - w.start_s, |prev| now_s - prev)
            .max(0.0);
        self.last_apply_s = Some(now_s);
        let base = self.rate.rate_ppm() + braking_ppm(err_ms, CALM_ZONE_MS) + self.bias_ppm;
        let pi = self.level.update(err_ms, dt_s, base);
        let target = (base + pi).clamp(-MAX_PPM, MAX_PPM);
        self.applied_ppm = slew(self.applied_ppm, target, dt_s);
        Step::Hold(self.applied_ppm)
    }

    fn recentre(&mut self, err_s: f64) -> Step {
        self.window = None;
        self.level.reset_error();
        self.recentres += 1;
        Step::Recentre {
            by_s: err_s,
            ppm: self.applied_ppm,
        }
    }
}

#[cfg(test)]
mod tests;
