use super::*;

/// A buffer between two clocks: our converter writes into it at our rate
/// times (1 + the correction), the consumer reads at its rate, `drift_ppm`
/// off ours. Every `step_s` the controller observes the depth.
struct Sim {
    c: Controller,
    rate: f64,
    drift_ppm: f64,
    depth_frames: f64,
    consumed: f64,
    t: f64,
    max_step_ppm: f64,
    last_ppm: f64,
    last_t: f64,
}

impl Sim {
    fn new(drift_ppm: f64, start_depth_s: f64) -> Self {
        Sim {
            c: Controller::new(0.010, 0.002, 0.050, Basis::Arrival),
            rate: 48_000.0,
            drift_ppm,
            depth_frames: start_depth_s * 48_000.0,
            consumed: 0.0,
            t: 0.0,
            max_step_ppm: 0.0,
            last_ppm: 0.0,
            last_t: 0.0,
        }
    }

    /// Runs `seconds` in steps of 10 ms; returns the re-centres seen.
    fn run(&mut self, seconds: f64) -> u32 {
        let dt = 0.010;
        let mut recentres = 0;
        let end = self.t + seconds;
        while self.t < end {
            self.t += dt;
            let ppm = self.c.ppm();
            self.depth_frames += dt * self.rate * (1.0 + ppm * 1e-6);
            let read = dt * self.rate * (1.0 + self.drift_ppm * 1e-6);
            self.depth_frames -= read;
            self.consumed += read;
            let step = self.c.observe(
                self.t,
                self.depth_frames / self.rate,
                self.consumed / self.rate,
            );
            if let Step::Recentre { by_s, .. } = step {
                self.depth_frames += by_s * self.rate;
                recentres += 1;
            }
            let now = self.c.ppm();
            if self.t - self.last_t >= 1.0 {
                let per_s = (now - self.last_ppm).abs() / (self.t - self.last_t);
                self.max_step_ppm = self.max_step_ppm.max(per_s);
                self.last_ppm = now;
                self.last_t = self.t;
            }
        }
        recentres
    }

    fn depth_ms(&self) -> f64 {
        self.depth_frames / self.rate * 1000.0
    }
}

#[test]
fn a_drifting_clock_is_followed_and_the_buffer_held_at_its_target() {
    for drift in [80.0, -150.0, 0.0] {
        let mut s = Sim::new(drift, 0.010);
        assert_eq!(s.run(900.0), 0, "{drift}: no re-centre for a steady drift");
        assert!(s.c.locked());
        assert!(
            (s.c.rate_ppm() - drift).abs() < 2.0,
            "{drift}: rate {}",
            s.c.rate_ppm()
        );
        assert!(
            (s.c.ppm() - drift).abs() < 3.0,
            "{drift}: correction {}",
            s.c.ppm()
        );
        assert!(
            (s.depth_ms() - 10.0).abs() < 1.0,
            "{drift}: depth {} ms",
            s.depth_ms()
        );
        // Inaudible: never faster than 5 ppm per second (a step of
        // measurement over a second rounds a little over it).
        assert!(
            s.max_step_ppm <= MAX_SLEW_PPM_PER_S * 1.05,
            "{drift}: {}",
            s.max_step_ppm
        );
    }
}

#[test]
fn an_offset_is_drained_by_the_ratio_not_by_a_skip() {
    let mut s = Sim::new(0.0, 0.035);
    assert_eq!(s.run(600.0), 0, "an offset inside the excess is no fault");
    assert!(
        (s.depth_ms() - 10.0).abs() < CALM_ZONE_MS + 0.2,
        "depth {} ms",
        s.depth_ms()
    );
    assert_eq!(s.c.recentres(), 0);
}

#[test]
fn a_depth_past_the_excess_or_under_the_floor_is_a_counted_recentre() {
    let mut c = Controller::new(0.010, 0.002, 0.050, Basis::Arrival);
    match c.observe(0.0, 0.070, 0.0) {
        Step::Recentre { by_s, ppm } => {
            assert!((by_s + 0.060).abs() < 1e-12, "{by_s}");
            assert_eq!(ppm, 0.0);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        c.observe(0.01, 0.001, 0.01),
        Step::Recentre { .. }
    ));
    // Exactly at the excess, and exactly at the floor: still held.
    assert!(matches!(c.observe(0.02, 0.060, 0.02), Step::Hold(_)));
    assert!(matches!(c.observe(0.03, 0.002, 0.03), Step::Hold(_)));
    assert_eq!(c.recentres(), 2);
}

#[test]
fn an_outside_error_steers_and_the_depth_only_guards() {
    let mut c = Controller::new(0.010, 0.002, 0.050, Basis::Timecode);
    // The depth far from its target but inside the bounds moves nothing.
    for i in 0..300 {
        c.observe(f64::from(i) * 0.01, 0.040, f64::from(i) * 0.01);
    }
    assert_eq!(c.ppm(), 0.0);
    // A placement error of +5 ms (our audio early): more output.
    for i in 0..3_000 {
        c.outside(3.0 + f64::from(i) * 0.01, 5.0);
    }
    assert!(c.ppm() > 10.0, "{}", c.ppm());
    let mut c = Controller::new(0.010, 0.002, 0.050, Basis::Stamp);
    for i in 0..3_000 {
        c.outside(f64::from(i) * 0.01, -5.0);
    }
    assert!(c.ppm() < -10.0, "{}", c.ppm());
}

#[test]
fn the_outside_bias_is_bounded_and_added() {
    let mut c = Controller::new(0.010, 0.002, 0.050, Basis::Arrival);
    c.set_bias_ppm(40.0);
    for i in 0..10_000 {
        let t = f64::from(i) * 0.01;
        c.observe(t, 0.010, t);
    }
    // The bias is held at +10; the level loop pushes back once the buffer
    // moves, but in this frozen buffer the correction rests at the bias.
    assert!((c.ppm() - MAX_BIAS_PPM).abs() < 0.5, "{}", c.ppm());
    c.set_bias_ppm(-40.0);
    c.set_bias_ppm(-3.0);
    assert_eq!(c.bias_ppm, -3.0);
}

#[test]
fn the_rate_fit_locks_rebases_on_a_step_and_restarts_before_locking() {
    let mut f = RateFit::default();
    assert_eq!(f.rate_ppm(), 0.0);
    for i in 0..29 {
        assert_eq!(
            f.offer(f64::from(i) * 3.0, f64::from(i) * 3.0 * 50e-6),
            Offered::Inserted
        );
    }
    assert!(!f.locked(), "29 points");
    f.offer(87.0, 87.0 * 50e-6);
    assert!(f.locked());
    assert!((f.rate_ppm() - 50.0).abs() < 1e-6, "{}", f.rate_ppm());
    // A 20 ms step: re-based once, then the line moves onto it.
    assert_eq!(f.offer(90.0, 90.0 * 50e-6 + 0.020), Offered::Rebased);
    assert_eq!(f.offer(93.0, 93.0 * 50e-6 + 0.020), Offered::Realigned);
    assert_eq!(f.offer(96.0, 96.0 * 50e-6 + 0.020), Offered::Inserted);
    assert!((f.rate_ppm() - 50.0).abs() < 1e-3, "{}", f.rate_ppm());
    // Before locking, a step starts over.
    let mut g = RateFit::default();
    g.offer(0.0, 0.0);
    g.offer(1.0, 0.0);
    assert_eq!(g.offer(2.0, 0.050), Offered::Restarted);
    assert_eq!(g.points(), 1);
    // Points older than the span leave.
    let mut h = RateFit::default();
    for i in 0..700 {
        h.offer(f64::from(i), 0.0);
    }
    assert!(
        h.points() <= REGRESSION_SPAN_S as usize + 1,
        "{}",
        h.points()
    );
    h.flush();
    assert_eq!(h.points(), 0);
}

#[test]
fn the_level_loop_s_integral_freezes_while_the_sum_saturates() {
    let mut l = LevelLoop::default();
    l.update(10.0, 1.0, 0.0);
    let i = l.integral_ppm();
    assert!(i > 0.0);
    l.update(10.0, 1.0, MAX_PPM);
    assert_eq!(l.integral_ppm(), i, "frozen at saturation");
    assert!(l.ema_ms() > 0.0);
    l.reset_error();
    assert_eq!(l.ema_ms(), 0.0);
    // P is bounded.
    let mut p = LevelLoop::default();
    let out = p.update(1_000.0, 100.0, 0.0);
    assert!(out <= LEVEL_KP_MAX_PPM + LEVEL_INTEGRAL_MAX_PPM, "{out}");
}

#[test]
fn the_slew_and_the_braking_follow_their_laws() {
    assert_eq!(slew(0.0, 100.0, 1.0), 5.0);
    assert_eq!(slew(0.0, -100.0, 2.0), -10.0);
    assert_eq!(slew(0.0, 3.0, 1.0), 3.0);
    assert_eq!(slew(7.0, 100.0, -1.0), 7.0);
    assert_eq!(braking_ppm(0.5, 1.0), 0.0);
    // 10 ms beyond a 1 ms zone: sqrt(2 * 5 * 9000) = 300.
    assert!((braking_ppm(10.0, 1.0) - 300.0).abs() < 1e-9);
    assert!((braking_ppm(-10.0, 1.0) + 300.0).abs() < 1e-9);
}

#[test]
fn the_level_loop_gives_its_exact_p_and_i() {
    let mut l = LevelLoop::default();
    // alpha = 1 / (10 + 1); EMA 10/11 ms; P = 2 * 10/11; I = 0.0002 * 10 * 1.
    let out = l.update(10.0, 1.0, 0.0);
    let ema = 10.0 / 11.0;
    assert!((l.ema_ms() - ema).abs() < 1e-12, "{}", l.ema_ms());
    assert!(
        (l.integral_ppm() - 0.002).abs() < 1e-15,
        "{}",
        l.integral_ppm()
    );
    assert!((out - (2.0 * ema + 0.002)).abs() < 1e-12, "{out}");
    // A second window moves the EMA by alpha again.
    let out = l.update(-10.0, 1.0, 0.0);
    let ema2 = ema + (-10.0 - ema) / 11.0;
    assert!((l.ema_ms() - ema2).abs() < 1e-12);
    assert!(
        (l.integral_ppm() - 0.0).abs() < 1e-15,
        "{}",
        l.integral_ppm()
    );
    assert!((out - 2.0 * ema2).abs() < 1e-12, "{out}");
}

#[test]
fn the_integral_freezes_exactly_at_the_bound_and_moves_just_inside() {
    let mut l = LevelLoop::default();
    // P at its 50 ppm bound, I at its 3 ppm bound.
    for _ in 0..200 {
        l.update(10_000.0, 10.0, 0.0);
    }
    assert_eq!(l.integral_ppm(), LEVEL_INTEGRAL_MAX_PPM);
    // base 247 + P 50 + I 3 = 300: saturated, the integral stays.
    l.update(-1.0, 0.001, 247.0);
    assert_eq!(l.integral_ppm(), LEVEL_INTEGRAL_MAX_PPM);
    // base 246: inside, it moves down by 0.0002 * 1 * 0.001.
    l.update(-1.0, 0.001, 246.0);
    assert!(
        (l.integral_ppm() - (3.0 - 2e-7)).abs() < 1e-12,
        "{}",
        l.integral_ppm()
    );
}

#[test]
fn the_rate_fit_reads_away_from_zero_and_keeps_its_boundaries() {
    // x from 1000 s: a fit centred wrongly would read another slope.
    let mut f = RateFit::default();
    for i in 0..40 {
        let x = 1_000.0 + f64::from(i) * 2.0;
        f.offer(x, 0.25 + (x - 1_000.0) * 20e-6);
    }
    assert!(f.locked());
    assert!((f.rate_ppm() - 20.0).abs() < 1e-6, "{}", f.rate_ppm());
    // A residual of exactly the step bound is a point, not a step.
    let mut g = RateFit::default();
    for i in 0..30 {
        g.offer(f64::from(i) * 3.0, 0.0);
    }
    assert_eq!(g.offer(90.0, STEP_RESIDUAL_S), Offered::Inserted);
    // Before locking: equal non-zero values are no step; the bound itself
    // is no step either.
    let mut h = RateFit::default();
    h.offer(0.0, 0.5);
    assert_eq!(h.offer(1.0, 0.5), Offered::Inserted);
    assert_eq!(h.offer(2.0, 0.5 + 0.0078125), Offered::Inserted);
    let mut z = RateFit::default();
    z.offer(0.0, 0.0);
    assert_eq!(z.offer(1.0, STEP_RESIDUAL_S), Offered::Inserted);
    // A point exactly the span old stays; one past it leaves.
    let mut k = RateFit::default();
    k.offer(0.0, 0.0);
    k.offer(REGRESSION_SPAN_S, 0.0);
    assert_eq!(k.points(), 2);
    k.offer(REGRESSION_SPAN_S + 0.5, 0.0);
    assert_eq!(k.points(), 2);
    // Locking needs the span too: 30 points within 58 s are not locked
    // (from 100 s, so the span is a difference, not a sum).
    let mut m = RateFit::default();
    for i in 0..30 {
        m.offer(100.0 + f64::from(i) * 2.0, 0.0);
    }
    assert!(!m.locked());
    m.offer(160.0, 0.0);
    assert!(m.locked());
}

#[test]
fn the_controller_counts_its_rebases_and_starts_unlocked() {
    let mut c = Controller::new(0.5, 0.0, 0.25, Basis::Arrival);
    assert!(!c.locked());
    assert_eq!((c.rebases(), c.recentres()), (0, 0));
    // One observation a second at the target; the other clock 10 ppm
    // fast, with two 50 ms steps.
    let mut step = 0.0;
    for i in 0..200 {
        let t = f64::from(i);
        if i == 100 || i == 150 {
            step += 0.050;
        }
        c.observe(t, 0.5, t * (1.0 + 10e-6) + step);
    }
    assert!(c.locked());
    assert_eq!(c.rebases(), 2);
    assert!((c.rate_ppm() - 10.0).abs() < 0.5, "{}", c.rate_ppm());
}

#[test]
fn the_excess_bound_itself_is_held() {
    let mut c = Controller::new(0.5, 0.0, 0.25, Basis::Arrival);
    assert!(matches!(c.observe(0.0, 0.75, 0.0), Step::Hold(_)));
    assert!(matches!(c.observe(0.1, 0.25, 0.1), Step::Hold(_)));
    assert!(matches!(
        c.observe(0.2, 0.75 + 1e-9, 0.2),
        Step::Recentre { .. }
    ));
}

#[test]
fn a_window_closes_at_exactly_one_second_and_moves_by_its_own_time() {
    let mut c = Controller::new(0.5, 0.0, 0.25, Basis::Arrival);
    // 20 ms short of the target: the window's mean error is +20 ms. From
    // 10 s, so a window's time is a difference, not a sum.
    assert_eq!(c.observe(10.0, 0.48, 10.0), Step::Hold(0.0));
    assert_eq!(c.observe(10.5, 0.48, 10.5), Step::Hold(0.0));
    // At 11.0 s the window closes: the first move is bounded by 5 ppm/s
    // over its 1 s.
    let Step::Hold(first) = c.observe(11.0, 0.48, 11.0) else {
        panic!("held");
    };
    assert_eq!(first, MAX_SLEW_PPM_PER_S);
    // The next window opens at its first observation (11.5 s) and closes
    // 1 s later; its move counts the 1.5 s since the last close.
    c.observe(11.5, 0.48, 11.5);
    assert_eq!(c.observe(12.0, 0.48, 12.0), Step::Hold(first));
    let Step::Hold(second) = c.observe(12.5, 0.48, 12.5) else {
        panic!("held");
    };
    assert_eq!(second, first + 1.5 * MAX_SLEW_PPM_PER_S);
}

#[test]
fn outside_errors_alone_never_lock_the_rate_fit() {
    let mut c = Controller::new(0.5, 0.0, 0.25, Basis::Stamp);
    for i in 0..20_000 {
        c.outside(f64::from(i) * 0.01, 0.0);
    }
    assert!(!c.locked());
    assert_eq!(c.rate_ppm(), 0.0);
}

#[test]
fn windows_of_outside_errors_alone_leave_the_rate_fit_as_it_was() {
    // A timecode ASRC: depth observations bring rate points, outside
    // errors do not; a window of only outside errors offers no point.
    let mut c = Controller::new(0.5, 0.0, 0.25, Basis::Timecode);
    for i in 0..120 {
        let t = f64::from(i);
        c.observe(t, 0.5, t * (1.0 + 30e-6));
    }
    assert!(c.locked());
    for i in 0..6_000 {
        c.outside(120.0 + f64::from(i) * 0.01, 0.0);
    }
    let rate = c.rate_ppm();
    assert!(rate.is_finite() && (rate - 30.0).abs() < 0.5, "{rate}");
}
