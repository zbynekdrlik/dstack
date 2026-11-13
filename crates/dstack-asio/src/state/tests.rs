// SPDX-License-Identifier: MIT OR Apache-2.0

//! SongPlayer's decision tests (songplayer `asio_state_tests.rs` and
//! `asio_state_tests_rate.rs`), carried over with the times in ns.

use super::*;

#[test]
fn the_backoff_is_2_10_30_then_every_60_s() {
    let s: Vec<i64> = (1..=6).map(|f| backoff_ns(f) / 1_000_000_000).collect();
    assert_eq!(s, vec![2, 10, 30, 60, 60, 60]);
    assert_eq!(
        backoff_ns(0),
        2_000_000_000,
        "never less than the start backoff"
    );
    assert_eq!(backoff_ns(u32::MAX), 60_000_000_000);
    assert_eq!(BACKOFF_S, [2, 10, 30, 60]);
}

#[test]
fn a_60_s_run_resets_the_backoff() {
    assert_eq!(STABLE_RUN_NS, 60_000_000_000);
    assert_eq!(failures_after_close(3, STABLE_RUN_NS), 1);
    assert_eq!(failures_after_close(3, STABLE_RUN_NS - 1), 4);
    assert_eq!(failures_after_close(0, 0), 1);
    assert_eq!(failures_after_close(u32::MAX, 0), u32::MAX, "never wraps");
}

#[test]
fn a_reset_a_size_change_or_a_new_rate_closes_the_run() {
    let base = DeviceEvents::default();
    let with = |f: fn(&mut DeviceEvents)| {
        let mut e = base;
        f(&mut e);
        close_reason(&e, 96_000.0)
    };
    assert_eq!(close_reason(&base, 96_000.0), None);
    assert_eq!(with(|e| e.reset = true), Some(Reason::Reset));
    assert_eq!(with(|e| e.buffer_size_change = true), Some(Reason::Reset));
    assert_eq!(
        with(|e| e.rate_changed = Some(48_000.0)),
        Some(Reason::RateChanged(48_000))
    );
    assert_eq!(
        with(|e| e.rate_changed = Some(96_000.5)),
        None,
        "under 1 Hz"
    );
    assert_eq!(
        with(|e| e.rate_changed = Some(96_001.0)),
        Some(Reason::RateChanged(96_001))
    );
    assert_eq!(
        with(|e| e.rate_changed = Some(95_999.0)),
        Some(Reason::RateChanged(95_999)),
        "1 Hz down"
    );
    assert_eq!(
        with(|e| {
            e.resync = true;
            e.latencies_changed = true;
            e.overloads = 3;
            e.callbacks = 9;
            e.faults = 2;
        }),
        None
    );
}

#[test]
fn a_report_under_1_hz_closes_as_a_lost_clock_and_no_clock_is_one() {
    for r in [0.0, 0.6, 0.999, -0.5] {
        let ev = DeviceEvents {
            rate_changed: Some(r),
            ..DeviceEvents::default()
        };
        assert_eq!(
            close_reason(&ev, 96_000.0),
            Some(Reason::RateChanged(0)),
            "{r}"
        );
    }
    let one = DeviceEvents {
        rate_changed: Some(1.0),
        ..DeviceEvents::default()
    };
    assert_eq!(close_reason(&one, 96_000.0), Some(Reason::RateChanged(1)));
    assert!(lost_clock(0.999) && !lost_clock(1.0) && !lost_clock(-1.0));
    assert_eq!(sample_rate_error(-995), Some(Reason::RateChanged(0)));
    assert_eq!(
        sample_rate_error(-1000),
        None,
        "ASE_NotPresent stays the glue's"
    );
    assert_eq!(
        sample_rate_error(-999),
        None,
        "ASE_HWMalfunction stays the glue's"
    );
    assert_eq!(
        Reason::RateChanged(0).text(),
        "the driver lost its clock (no rate)"
    );
}

#[test]
fn no_callback_for_2_s_is_a_stall() {
    assert_eq!(STALL_NS, 2_000_000_000);
    let mut w = StallWatch::default();
    assert!(!w.stalled(10, 0));
    assert!(!w.stalled(10, STALL_NS - 1));
    assert!(w.stalled(10, STALL_NS));
    assert!(!w.stalled(11, STALL_NS + 5), "a callback resets it");
    assert!(!w.stalled(11, 2 * STALL_NS + 4));
    assert!(w.stalled(11, 2 * STALL_NS + 5));
}

#[test]
fn the_message_replies() {
    use selector::*;
    assert_eq!(
        [
            SELECTOR_SUPPORTED,
            ENGINE_VERSION,
            RESET_REQUEST,
            BUFFER_SIZE_CHANGE,
            RESYNC_REQUEST,
            LATENCIES_CHANGED,
            SUPPORTS_TIME_INFO,
            SUPPORTS_TIME_CODE,
            OVERLOAD
        ],
        [1, 2, 3, 4, 5, 6, 7, 8, 15],
        "asio.h kAsio… (azo-sys MessageSelector)"
    );
    for supported in [
        ENGINE_VERSION,
        RESET_REQUEST,
        BUFFER_SIZE_CHANGE,
        RESYNC_REQUEST,
        LATENCIES_CHANGED,
        SUPPORTS_TIME_INFO,
        OVERLOAD,
    ] {
        assert_eq!(reply(SELECTOR_SUPPORTED, supported), 1, "{supported}");
    }
    assert_eq!(reply(SELECTOR_SUPPORTED, SUPPORTS_TIME_CODE), 0);
    assert_eq!(reply(SELECTOR_SUPPORTED, 99), 0);
    assert_eq!(reply(ENGINE_VERSION, 0), 2);
    assert_eq!(reply(RESET_REQUEST, 0), 1);
    assert_eq!(reply(RESYNC_REQUEST, 0), 1);
    assert_eq!(reply(LATENCIES_CHANGED, 0), 1);
    assert_eq!(reply(SUPPORTS_TIME_INFO, 0), 1);
    assert_eq!(reply(BUFFER_SIZE_CHANGE, 256), 0, "never resized live");
    assert_eq!(reply(SUPPORTS_TIME_CODE, 0), 0);
    assert_eq!(reply(OVERLOAD, 0), 0);
    assert_eq!(reply(99, 0), 0);
}

#[test]
fn the_rate_is_admitted() {
    assert_eq!(admit_rate(96_000.0), Ok(96_000));
    assert_eq!(admit_rate(44_100.4), Ok(44_100));
    assert_eq!(admit_rate(8_000.0), Ok(8_000));
    assert_eq!(admit_rate(384_000.0), Ok(384_000));
    for bad in [7_999.0, 384_001.0, f64::NAN, f64::INFINITY, 1.0, -48_000.0] {
        assert_eq!(
            admit_rate(bad),
            Err(Reason::Refused(format!("the driver reports {bad} Hz"))),
            "{bad}"
        );
    }
    for lost in [0.0, -0.0, 0.999, -0.5] {
        assert_eq!(admit_rate(lost), Err(Reason::RateChanged(0)), "{lost}");
    }
}

#[test]
fn every_reason_has_its_code_and_text() {
    let table: [(Reason, &str, &str); 14] = [
        (
            Reason::NotFound { present: vec![] },
            "not_found",
            "the driver is not registered (present: none)",
        ),
        (
            Reason::NotFound {
                present: vec!["A".into(), "B".into()],
            },
            "not_found",
            "the driver is not registered (present: A, B)",
        ),
        (
            Reason::Busy("x".into()),
            "busy",
            "the driver refused to start (in use by another program?): x",
        ),
        (Reason::Refused("r".into()), "refused", "r"),
        (Reason::Failed("f".into()), "failed", "f"),
        (Reason::Reset, "reset", "the driver asked for a reset"),
        (
            Reason::RateChanged(0),
            "clock_lost",
            "the driver lost its clock (no rate)",
        ),
        (
            Reason::RateChanged(48_000),
            "rate_changed",
            "the driver's rate changed to 48000 Hz",
        ),
        (
            Reason::Stalled,
            "stalled",
            "no callback from the driver for 2 s",
        ),
        (
            Reason::WindowsOnly,
            "windows_only",
            "ASIO runs on Windows only",
        ),
        (
            Reason::Held,
            "held",
            "another output of this process still holds the driver",
        ),
        (
            Reason::Parked,
            "parked",
            "a driver callback did not return for 1 s: the driver is parked until the process \
             restarts",
        ),
        (
            Reason::NoClock,
            "no_clock",
            "the driver gives no clock (no callback since it opened): e.g. the virtual soundcard \
             is not running, or its network has no clock leader",
        ),
        (
            Reason::ClockOffRate,
            "clock_off_rate",
            "the card takes audio more than 1 % off the rate its driver reports: no usable clock \
             (e.g. Waves SoundGrid with its network off)",
        ),
    ];
    for (reason, code, text) in table {
        assert_eq!((reason.code(), reason.text().as_str()), (code, text));
        assert_eq!(reason.to_string(), text);
    }
}

/// A driver that does not tick waits for its clock from 2 s after the open
/// (once per wait) and is opened again every 60 s; the first tick ends a
/// wait. Every edge, both sides.
#[test]
fn a_driver_that_does_not_tick_waits_for_its_clock_and_reopens_every_60_s() {
    use ClockStep::*;
    assert_eq!(
        (NO_CLOCK_NS, NO_CLOCK_REOPEN_NS),
        (2_000_000_000, 60_000_000_000)
    );
    let table = [
        (1, true, false, 0, Ticking),
        (1, false, false, 0, Ticking),
        (1, true, false, 70_000_000_000, Ticking),
        (1, true, true, 0, ClockArrived),
        (u64::MAX, true, true, 70_000_000_000, ClockArrived),
        (1, false, true, 0, Quiet),
        (u64::MAX, false, true, 70_000_000_000, Quiet),
        (0, true, false, 0, Quiet),
        (0, false, false, 1_999_999_999, Quiet),
        (0, true, false, 2_000_000_000, StartsWaiting),
        (0, false, false, 59_999_999_999, StartsWaiting),
        (0, true, true, 2_000_000_000, Quiet),
        (0, false, true, 59_999_999_999, Quiet),
        (0, true, true, 60_000_000_000, Reopen),
        (0, false, false, 60_000_000_000, Reopen),
        (0, true, true, i64::MAX, Reopen),
    ];
    for (ticks, primed, waiting, since, want) in table {
        assert_eq!(
            clock_step(ticks, primed, waiting, since),
            want,
            "{ticks} {primed} {waiting} {since}"
        );
    }
}

#[test]
fn only_an_absent_registry_key_is_an_empty_driver_list() {
    assert_eq!(HRESULT_FILE_NOT_FOUND, -2_147_024_894);
    assert_eq!(HRESULT_PATH_NOT_FOUND, -2_147_024_893);
    assert!(registry_key_absent(HRESULT_FILE_NOT_FOUND));
    assert!(registry_key_absent(HRESULT_PATH_NOT_FOUND));
    for other in [
        0,
        0x8007_0005_u32 as i32,
        0x8007_0001_u32 as i32,
        0x8007_0004_u32 as i32,
    ] {
        assert!(!registry_key_absent(other), "{other:#x}");
    }
}

#[test]
fn a_callback_gap_is_late_over_two_periods_and_fast_under_a_quarter() {
    let p = 1_000_000;
    assert_eq!(callback_gap_kind(2_000_001, p), GapKind::Late);
    assert_eq!(callback_gap_kind(2_000_000, p), GapKind::Normal);
    assert_eq!(callback_gap_kind(1_000_000, p), GapKind::Normal);
    assert_eq!(callback_gap_kind(250_000, p), GapKind::Normal);
    assert_eq!(callback_gap_kind(249_999, p), GapKind::Fast);
    assert_eq!(callback_gap_kind(0, p), GapKind::Fast);
    // 128 frames at 96 kHz: a period of 1.3333 ms.
    let dvs = frames_to_ns(128, 96_000.0);
    assert_eq!(dvs, 1_333_333);
    assert_eq!(callback_gap_kind(2_666_667, dvs), GapKind::Late);
    assert_eq!(callback_gap_kind(2_666_666, dvs), GapKind::Normal);
    assert_eq!(callback_gap_kind(333_334, dvs), GapKind::Normal);
    assert_eq!(callback_gap_kind(333_333, dvs), GapKind::Fast);
}

#[test]
fn frames_are_rounded_to_ns_and_a_rate_under_1_hz_counts_as_1() {
    assert_eq!(frames_to_ns(48_000, 48_000.0), 1_000_000_000);
    assert_eq!(frames_to_ns(1, 96_000.0), 10_417);
    assert_eq!(frames_to_ns(1, 0.0), 1_000_000_000);
    assert_eq!(frames_to_ns(-48, 48_000.0), -1_000_000);
}

// The rate watch (songplayer #229).

const T: i64 = 1_790_000_000_000_000_000;
/// 96 kHz, 128-frame buffers (a 10 s window).
const DVS: (f64, u32) = (96_000.0, 128);

#[test]
fn the_rate_watch_constants() {
    assert_eq!(
        (
            RATE_WINDOW_NS,
            RATE_WINDOW_BUFFERS,
            OFF_RATE_PPM,
            OFF_RATE_STRIKES_PLAYING
        ),
        (10_000_000_000, 500, 10_000.0, 2)
    );
}

#[test]
fn a_window_is_10_s_or_500_buffers_whichever_is_longer() {
    assert_eq!(rate_window_ns(128, 96_000.0), 10_000_000_000);
    assert_eq!(rate_window_ns(1024, 44_100.0), 11_609_977_324);
    assert_eq!(rate_window_ns(1024, 96_000.0), 10_000_000_000, "5.3 s");
    assert_eq!(rate_window_ns(2048, 96_000.0), 10_666_666_667);
}

#[test]
fn the_consumed_rate_is_measured_in_ppm_of_the_drivers_rate() {
    let close = |got: f64, want: f64| assert!((got - want).abs() < 1e-6, "{got} vs {want}");
    close(consumed_ppm(960_000, 10_000_000_000, 96_000.0), 0.0);
    close(consumed_ppm(987_840, 10_000_000_000, 96_000.0), 29_000.0);
    close(consumed_ppm(932_160, 10_000_000_000, 96_000.0), -29_000.0);
    close(consumed_ppm(1_920_000, 20_000_000_000, 96_000.0), 0.0);
    close(consumed_ppm(453_789, 10_000_000_000, 44_100.0), 29_000.0);
}

#[test]
fn a_window_closes_at_its_length_and_the_next_runs_from_there() {
    let mut w = RateWatch::default();
    let window = rate_window_ns(DVS.1, DVS.0);
    assert_eq!(w.step(1_000, T, DVS, 1), RateStep::Measuring, "the start");
    assert_eq!(
        w.step(1_000 + 2_000_000, T + window - 1, DVS, 1),
        RateStep::Measuring,
        "a step before the window's end"
    );
    assert_eq!(
        w.step(1_000 + 960_000, T + window, DVS, 1),
        RateStep::Plausible
    );
    assert!(matches!(
        w.step(1_000 + 960_000 + 969_601, T + 2 * window, DVS, 1),
        RateStep::OffRate { .. }
    ));
}

#[test]
fn one_percent_is_the_bound_on_either_side() {
    let window = rate_window_ns(DVS.1, DVS.0);
    let verdict = |frames: u64| {
        let mut w = RateWatch::default();
        w.step(0, T, DVS, 1);
        w.step(frames, T + window, DVS, 1)
    };
    assert_eq!(verdict(969_599), RateStep::Plausible);
    assert_eq!(verdict(950_401), RateStep::Plausible);
    let RateStep::OffRate { ppm } = verdict(969_601) else {
        panic!("1 % + 1 frame more")
    };
    assert!((ppm - 10_001.041_666).abs() < 1e-3, "{ppm}");
    let RateStep::OffRate { ppm } = verdict(950_399) else {
        panic!("1 % + 1 frame less")
    };
    assert!((ppm + 10_001.041_666).abs() < 1e-3, "{ppm}");
}

#[test]
fn a_playing_run_needs_two_off_windows_in_a_row() {
    let window = rate_window_ns(DVS.1, DVS.0);
    let (on, off) = (960_000, 987_840);
    let mut w = RateWatch::default();
    let mut frames = 0;
    let mut at = T;
    let mut next = |w: &mut RateWatch, taken: u64| {
        frames += taken;
        at += window;
        w.step(frames, at, DVS, OFF_RATE_STRIKES_PLAYING)
    };
    assert_eq!(w.step(0, T, DVS, 2), RateStep::Measuring);
    assert_eq!(next(&mut w, off), RateStep::Measuring, "one off window");
    assert_eq!(
        next(&mut w, on),
        RateStep::Plausible,
        "the count starts again"
    );
    assert_eq!(
        next(&mut w, off),
        RateStep::Measuring,
        "one off window again"
    );
    let RateStep::OffRate { ppm } = next(&mut w, off) else {
        panic!("two off windows in a row")
    };
    assert!((ppm - 29_000.0).abs() < 1e-6, "{ppm}");
    let RateStep::OffRate { .. } = next(&mut w, off) else {
        panic!("a third")
    };
}

#[test]
fn a_clock_that_goes_back_starts_the_window_again() {
    let window = rate_window_ns(DVS.1, DVS.0);
    let mut w = RateWatch::default();
    w.step(0, T, DVS, 1);
    assert_eq!(w.step(5_000, T - 1, DVS, 1), RateStep::Measuring);
    assert_eq!(
        w.step(5_000 + 960_000, T - 1 + window - 1, DVS, 1),
        RateStep::Measuring,
        "the window runs from the read that went back"
    );
    assert_eq!(
        w.step(5_000 + 960_000, T - 1 + window, DVS, 1),
        RateStep::Plausible
    );
}

#[test]
fn a_read_at_the_windows_start_instant_keeps_the_window() {
    let window = rate_window_ns(DVS.1, DVS.0);
    let mut w = RateWatch::default();
    w.step(0, T, DVS, 1);
    assert_eq!(w.step(100_000, T, DVS, 1), RateStep::Measuring);
    assert_eq!(w.step(969_000, T + window, DVS, 1), RateStep::Plausible);
}

#[test]
fn the_windows_start_after_a_quarter_second_of_ticks() {
    assert_eq!(RATE_SETTLE_PER_S, 4.0);
    assert!(rate_settled(12_000, 48_000.0));
    assert!(!rate_settled(11_999, 48_000.0));
    assert!(rate_settled(11_025, 44_100.0));
    assert!(!rate_settled(11_024, 44_100.0));
}

#[test]
fn a_callback_gap_over_100_ms_or_3_buffers_is_a_stall() {
    assert_eq!((RATE_GAP_MIN_NS, RATE_GAP_PERIODS), (100_000_000, 3));
    assert_eq!(rate_gap_limit_ns(128, 96_000.0), 100_000_000);
    assert_eq!(rate_gap_limit_ns(1024, 44_100.0), 100_000_000, "69.7 ms");
    assert_eq!(rate_gap_limit_ns(2048, 44_100.0), 139_319_728);
}

/// A card read the way a worker steps it: its count tracked and the watch
/// stepped every 50 ms.
struct Feed {
    w: RateWatch,
    frames: u64,
    at: i64,
    strikes: u32,
}

const READ: i64 = 50_000_000;
const ON: u64 = 4_800;
const OFF: u64 = 4_939;

impl Feed {
    fn new(strikes: u32) -> Self {
        let mut f = Self {
            w: RateWatch::default(),
            frames: 0,
            at: T,
            strikes,
        };
        f.read();
        f
    }

    fn read(&mut self) -> RateStep {
        self.w.track(self.frames, self.at, DVS);
        self.w.step(self.frames, self.at, DVS, self.strikes)
    }

    fn run(&mut self, reads: u32, per_read: u64) -> Option<(i64, RateStep)> {
        let mut first = None;
        for _ in 0..reads {
            self.at += READ;
            self.frames += per_read;
            let step = self.read();
            if first.is_none() && step != RateStep::Measuring {
                first = Some((self.at, step));
            }
        }
        first
    }

    fn gap(&mut self, gap: i64) {
        self.at += gap;
        self.frames += (gap as u64) * 96_000 / 1_000_000_000;
        self.read();
    }
}

#[test]
fn a_callback_gap_over_the_limit_starts_the_window_again() {
    let window = rate_window_ns(DVS.1, DVS.0);
    let mut f = Feed::new(1);
    f.run(20, ON);
    f.gap(100_000_000);
    assert_eq!(f.run(400, ON).unwrap(), (T + window, RateStep::Plausible));
    let mut f = Feed::new(1);
    f.run(20, ON);
    f.gap(100_000_001);
    let resumed = T + 1_000_000_000 + 100_000_001;
    assert_eq!(
        f.run(400, ON).unwrap(),
        (resumed + window, RateStep::Plausible)
    );
}

#[test]
fn a_callback_gap_ends_the_strikes_so_far() {
    let window = rate_window_ns(DVS.1, DVS.0);
    let mut f = Feed::new(OFF_RATE_STRIKES_PLAYING);
    assert_eq!(f.run(200, OFF), None, "one strike at T + 10 s");
    f.gap(2_000_000_000);
    let resumed = T + window + 2_000_000_000;
    let (at, step) = f.run(500, OFF).unwrap();
    assert_eq!(at, resumed + 2 * window, "two windows after the gap");
    assert!(matches!(step, RateStep::OffRate { .. }), "{step:?}");
}

#[test]
fn a_window_that_closes_inside_a_stall_is_judged() {
    let window = rate_window_ns(DVS.1, DVS.0);
    let mut f = Feed::new(1);
    assert_eq!(f.run(190, ON), None, "9.5 s on its rate");
    let (at, step) = f.run(40, 0).unwrap();
    assert_eq!(at, T + window);
    assert!(matches!(step, RateStep::OffRate { .. }), "{step:?}");
}
