// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

/// A chunk of 960 frames at 48 kHz: 20 ms.
const CHUNK: usize = 960;
const RATE: u32 = 48_000;
const P: f64 = 0.02;

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn writer() -> Writer {
    Writer::new(CHUNK, RATE)
}

#[test]
fn nothing_is_known_before_the_first_write() {
    let w = writer();
    assert_eq!(w.until_next(1.0, 0), None);
    assert_eq!(w.due_s(960), None);
    assert!(close(w.period_s(), P));
}

#[test]
fn the_first_write_sets_the_schedule() {
    let mut w = writer();
    w.wrote(1.0, 960);
    assert!(close(w.due_s(960).unwrap(), 1.0));
    assert!(close(w.due_s(1920).unwrap(), 1.0 + P));
    assert!(close(w.until_next(1.005, 960).unwrap(), P - 0.005));
    // Right at the write: a whole period to the next.
    assert!(close(w.until_next(1.0, 960).unwrap(), P));
}

#[test]
fn a_write_on_time_pulls_the_schedule_by_the_gain() {
    let mut w = writer();
    w.wrote(1.0, 960);
    // 2 ms late, inside the tolerance (5 ms).
    w.wrote(1.0 + P + 0.002, 1920);
    assert!(close(w.due_s(2880).unwrap(), 1.0 + 2.0 * P + GAIN * 0.002));
    // 2 ms early.
    let due = w.due_s(2880).unwrap();
    w.wrote(due - 0.002, 2880);
    assert!(close(w.due_s(2880).unwrap(), due - GAIN * 0.002));
}

#[test]
fn the_tolerance_is_a_quarter_period_each_side() {
    let edge = TOLERANCE * P;
    for (off, moves) in [
        (edge * 0.99, true),
        (edge * 1.01, false),
        (-edge * 0.99, true),
        (-edge * 1.01, false),
    ] {
        let mut w = writer();
        w.wrote(1.0, 960);
        w.wrote(1.0 + P + off, 1920);
        let want = if moves { 1.0 + P + GAIN * off } else { 1.0 + P };
        assert!(close(w.due_s(1920).unwrap(), want), "{off} {moves}");
    }
}

#[test]
fn a_stall_and_its_catch_up_burst_leave_the_schedule() {
    let mut w = writer();
    w.wrote(1.0, 960);
    // Stalled to 1.066: the chunks due at 1.02, 1.04 and 1.06 come at once,
    // each more than the tolerance (5 ms) late.
    w.wrote(1.066, 1920);
    w.wrote(1.066, 2880);
    w.wrote(1.066, 3840);
    assert!(
        close(w.due_s(3840).unwrap(), 1.06),
        "late writes move nothing"
    );
    // The chunks the burst brought are not expected again: the next is due
    // at 1.08, not a period after the burst.
    assert!(close(w.until_next(1.066, 3840).unwrap(), 0.014));
}

/// Seen as one jump of two chunks (the reader is called once after the
/// burst), the burst's last chunk 3 ms late moves nothing, though alone it
/// would be on time.
#[test]
fn a_count_that_jumps_more_than_a_chunk_is_a_burst() {
    let mut w = writer();
    w.wrote(1.0, 960);
    w.wrote(1.043, 2880);
    assert!(close(w.due_s(2880).unwrap(), 1.04));
    // A chunk on its own, 3 ms late: pulled.
    w.wrote(1.063, 3840);
    assert!(close(w.due_s(3840).unwrap(), 1.06 + GAIN * 0.003));
    // Exactly one chunk more than the last is no burst either.
    let due = w.due_s(4800).unwrap();
    w.wrote(due + 0.001, 4800);
    assert!(close(w.due_s(4800).unwrap(), due + GAIN * 0.001));
}

#[test]
fn an_overdue_chunk_is_due_now_and_never_more_than_a_period_away() {
    let mut w = writer();
    w.wrote(1.0, 960);
    // Overdue within the tolerance (5 ms): now.
    assert_eq!(w.until_next(1.024, 960), Some(0.0));
    assert_eq!(w.until_next(1.025 - 1e-9, 960), Some(0.0));
    // Overdue past it: the writer is off its schedule, nothing known.
    assert_eq!(w.until_next(1.025 + 1e-9, 960), None);
    assert_eq!(w.until_next(1.03, 960), None);
    // A count ahead of the schedule (an early write) waits one period.
    assert!(close(w.until_next(1.0, 2880).unwrap(), P));
}

/// One write late on its own (a short stall) moves nothing; the next on
/// time pulls as before.
#[test]
fn one_late_write_alone_leaves_the_schedule() {
    let mut w = writer();
    w.wrote(1.0, 960);
    w.wrote(1.03, 1920);
    assert!(close(w.due_s(1920).unwrap(), 1.02));
    w.wrote(1.041, 2880);
    assert!(close(w.due_s(2880).unwrap(), 1.04 + GAIN * 0.001));
}

/// A capture that stopped and started again (dsend): two writes in a row
/// 30 ms late by the same error move the line onto the second; two late
/// by errors apart do not.
#[test]
fn two_writes_off_by_the_same_error_are_a_new_schedule() {
    let mut w = writer();
    w.wrote(1.0, 960);
    w.wrote(1.05, 1920);
    w.wrote(1.0702, 2880);
    assert!(
        close(w.due_s(2880).unwrap(), 1.0702),
        "moved onto the second"
    );
    assert!(close(w.until_next(1.0702, 2880).unwrap(), P));
    let mut w = writer();
    w.wrote(1.0, 960);
    w.wrote(1.05, 1920);
    w.wrote(1.080, 2880);
    assert!(close(w.due_s(2880).unwrap(), 1.04), "40 ms after 30 ms: no");
    // A write on time between them clears the first.
    let mut w = writer();
    w.wrote(1.0, 960);
    w.wrote(1.05, 1920);
    w.wrote(1.04, 2880);
    w.wrote(1.09, 3840);
    assert!(close(w.due_s(3840).unwrap(), 1.06), "not in a row");
}

#[test]
fn a_drifting_writer_is_followed() {
    // The writer's clock 200 ppm fast: each chunk 4 us early on the line.
    let mut w = writer();
    let mut t = 1.0;
    for k in 1..=3000u64 {
        w.wrote(t, k * 960);
        t += P * (1.0 - 200e-6);
    }
    let lag = w.due_s(3000 * 960).unwrap() - (t - P * (1.0 - 200e-6));
    assert!(lag.abs() < 0.0001, "{lag}");
}
