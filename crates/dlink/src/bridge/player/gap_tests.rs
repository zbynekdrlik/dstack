// SPDX-License-Identifier: MIT OR Apache-2.0

//! The output's own gaps (dstack #307, [`Player::fill_pulled`],
//! [`output_gap`]): a fill-the-room output (WASAPI shared: the engine
//! reading a period at each tick) whose callback was held past its buffer.
//! At 48 kHz a period is 480 frames, 1 ms 48 frames.

use super::*;

use crate::bridge::chunk_ns;

/// The calls `(pulled, call)` through [`output_gap`] at a usual call of
/// 480: the frames dropped at each and the gaps ended.
fn run(calls: &[(usize, usize)]) -> (Vec<usize>, u64) {
    let mut state = GapState::default();
    let mut ended = 0;
    let drops = calls
        .iter()
        .map(|&(pulled, call)| {
            let gap = output_gap(state, pulled, call, 480);
            state = gap.state;
            ended += u64::from(gap.ended);
            gap.drop
        })
        .collect();
    (drops, ended)
}

#[test]
fn a_hold_drops_the_card_s_silence_and_keeps_the_phase() {
    // Held 25 ms past its tick on a buffer of two periods, every wake on
    // time: the tick at 20 ms found it empty (480 of silence). The big
    // call reads 35 ms for 20 and drops what surely was silence (the
    // excess past a period); the next, on the grid at 30 ms, reads 5 ms
    // for a period; the lowest sum settles the rest.
    let (drops, ended) = run(&[(1680, 960), (240, 480), (480, 480), (480, 480), (480, 480)]);
    assert_eq!(drops, [240, 0, 0, 0, 240]);
    assert_eq!(ended, 1);
}

#[test]
fn a_late_wake_after_the_hold_drops_no_more_than_the_silence() {
    // The same hold, the call before 0.5 ms late (24) and the call after
    // the hold 6 ms late (288): rounding each call's excess to periods
    // dropped 960 here; the lowest sum drops 456 of the 480.
    let (drops, ended) = run(&[(1656, 960), (528, 480), (192, 480), (480, 480), (480, 480)]);
    assert_eq!(drops.iter().sum::<usize>(), 456);
    assert_eq!(ended, 1);
}

#[test]
fn a_hold_the_buffer_covered_drops_nothing() {
    // Held 15 ms: the call came before the tick at 20 ms.
    let (drops, ended) = run(&[(1200, 960), (240, 480), (480, 480), (480, 480), (480, 480)]);
    assert_eq!(drops, [0; 5]);
    assert_eq!(ended, 0);
}

#[test]
fn a_hold_that_ended_right_at_a_tick_is_settled_by_the_next_calls() {
    // Held to 1 ms past the tick at 20 ms (480 of silence), the call
    // before 2 ms late: the big call's excess (432) is under a period.
    let (drops, ended) = run(&[(1392, 960), (480, 480), (480, 480), (480, 480), (480, 480)]);
    assert_eq!(drops, [0, 0, 0, 0, 432]);
    assert_eq!(ended, 1);
}

#[test]
fn a_buffer_of_two_periods_and_a_fraction_drops_its_partial_silence() {
    // Windows' 1056-frame buffer for a 480-frame period, held 25 ms: the
    // tick at 20 ms found 96 frames, 384 of silence.
    let (drops, _) = run(&[(1680, 1056), (240, 480), (480, 480), (480, 480), (480, 480)]);
    assert_eq!(drops.iter().sum::<usize>(), 384);
}

#[test]
fn the_field_s_46_ms_hold_drops_three_periods() {
    // Held 46 ms: ticks at 20, 30, 40 ms found it empty (1440). The next
    // call, on the grid at 50 ms, reads 4 ms.
    let (drops, ended) = run(&[(2688, 960), (192, 480), (480, 480), (480, 480), (480, 480)]);
    assert_eq!(drops.iter().sum::<usize>(), 1440);
    assert_eq!(ended, 1);
}

#[test]
fn two_holds_back_to_back_are_one_gap() {
    // Held 25 ms, then the next event (at 30 ms) held 25 ms too: the
    // ticks at 20 and 50 ms found it empty (960).
    let (drops, ended) = run(&[
        (1680, 960),
        (1440, 960),
        (240, 480),
        (480, 480),
        (480, 480),
        (480, 480),
    ]);
    assert_eq!(drops.iter().sum::<usize>(), 960);
    assert_eq!(ended, 1);
}

#[test]
fn a_pull_that_wrote_nothing_is_left_out_of_the_lowest_sum() {
    // The call after the hold found the pull's lock held (0 written); the
    // next reads both calls' time.
    let (drops, _) = run(&[(1680, 960), (0, 480), (720, 480), (480, 480), (480, 480)]);
    assert_eq!(drops.iter().sum::<usize>(), 480);
}

#[test]
fn calls_of_the_usual_size_open_no_gap() {
    // A late call of the usual size (it reads more, takes the same), a
    // pull skipped on the call before (0, then double).
    let (drops, ended) = run(&[(960, 480), (0, 480), (960, 480), (480, 480)]);
    assert_eq!(drops, [0; 4]);
    assert_eq!(ended, 0);
    // No usual call known yet.
    let gap = output_gap(GapState::default(), 4800, 960, 0);
    assert_eq!((gap.drop, gap.state), (0, GapState::default()));
}

#[test]
fn a_remainder_under_a_quarter_period_is_the_latenesses_noise() {
    // Held 15 ms (no silence), the next call 2.5 ms late: the lowest sum
    // ends at 119, under 120, nothing dropped; at 120 it is.
    let (drops, ended) = run(&[(1200, 960), (359, 480), (480, 480), (480, 480), (480, 480)]);
    assert_eq!(drops, [0; 5]);
    assert_eq!(ended, 0);
    let (drops, ended) = run(&[(1200, 960), (360, 480), (480, 480), (480, 480), (480, 480)]);
    assert_eq!(drops, [0, 0, 0, 0, 120]);
    assert_eq!(ended, 1);
}

#[test]
fn the_settling_runs_its_calls_then_closes() {
    let gap = output_gap(GapState::default(), 1680, 960, 480);
    assert_eq!(
        gap.state,
        GapState {
            sum: 480,
            least: 480,
            left: SETTLE_CALLS,
            dropped: 240
        }
    );
    let gap = output_gap(gap.state, 240, 480, 480);
    assert_eq!(
        gap.state,
        GapState {
            sum: 240,
            least: 240,
            left: SETTLE_CALLS - 1,
            dropped: 240
        }
    );
    // A big call while owed carries the sum and what it dropped.
    let gap = output_gap(gap.state, 1440, 960, 480);
    assert_eq!(
        gap.state,
        GapState {
            sum: 480,
            least: 480,
            left: SETTLE_CALLS,
            dropped: 480
        }
    );
    assert_eq!((gap.drop, gap.ended), (240, false));
}

#[test]
fn the_usual_call_is_the_smallest_that_came_twice() {
    // The first call (Windows' whole 1056-frame buffer) until a smaller
    // one repeats.
    assert_eq!(usual_call(0, 0, 1056), 1056);
    assert_eq!(usual_call(1056, 1056, 480), 1056);
    assert_eq!(usual_call(1056, 480, 480), 480);
    // A bigger call never.
    assert_eq!(usual_call(480, 960, 960), 480);
    // A stray small call once, never.
    assert_eq!(usual_call(480, 480, 16), 480);
    assert_eq!(usual_call(480, 16, 480), 480);
}

#[test]
fn a_stray_small_call_does_not_make_every_period_big() {
    let (fifo, mut player) = running(200);
    let stats = player.stats();
    call(&fifo, &mut player, 16, 16);
    for _ in 0..200 {
        // A pull a frame over its take now and then (the two clocks).
        call(&fifo, &mut player, 481, 480);
        call(&fifo, &mut player, 479, 480);
    }
    assert_eq!((stats.output_gaps(), stats.output_lost_frames()), (0, 0));
}

/// A player at 48 kHz on both sides (dlisten's default buffer: 10 ms +
/// 128 frames) with its FIFO, run `calls` calls of 480 frames, each
/// pulled just before (`push` then `fill_pulled`), so it plays on a level.
fn running(calls: usize) -> (Arc<Fifo>, Player) {
    let fifo = Arc::new(fifo_for(1, 48_000));
    let buffer = AppBuffer::new(10_000_000, chunk_ns(128, 48_000));
    let mut player = Player::new(
        Arc::clone(&fifo),
        48_000,
        48_000,
        buffer,
        buffer.chunk_ns(),
        Arc::default(),
    )
    .unwrap();
    for _ in 0..calls {
        call(&fifo, &mut player, 480, 480);
    }
    (fifo, player)
}

/// One call of `take` frames after a pull of `pulled` frames (a constant).
fn call(fifo: &Fifo, player: &mut Player, pulled: usize, take: usize) {
    let pushed = fifo.push_with(pulled, |_, _| 0.5);
    let mut out = vec![0.0f32; take];
    player.fill_pulled(pushed, &mut out, 1, |x| x as f32);
}

#[test]
fn the_output_s_gap_is_dropped_and_counted_as_its_own() {
    let (fifo, mut player) = running(200);
    assert_eq!(player.state, State::Running);
    let stats = player.stats();
    let before = fifo.len();
    let skipped = player.skipped;
    // Held 25 ms: one period of the card's silence.
    call(&fifo, &mut player, 1680, 960);
    call(&fifo, &mut player, 240, 480);
    for _ in 0..SETTLE_CALLS {
        call(&fifo, &mut player, 480, 480);
    }
    assert_eq!((stats.output_gaps(), stats.output_lost_frames()), (1, 480));
    assert_eq!((stats.underruns(), stats.recentres()), (0, 0));
    // The output's clock counts the time the card played silence.
    assert_eq!(player.skipped, skipped + 480);
    // The depth is the depth before, within the converter's call (it takes
    // whole blocks of input): the latency is the same (kept, the 480
    // frames would show).
    assert!(
        fifo.len().abs_diff(before) <= BLOCK,
        "{} {before}",
        fifo.len()
    );
}

#[test]
fn a_gap_on_a_44_1_khz_output_is_dropped_in_network_frames() {
    let fifo = Arc::new(fifo_for(1, 48_000));
    let buffer = AppBuffer::new(10_000_000, chunk_ns(128, 48_000));
    let mut player = Player::new(
        Arc::clone(&fifo),
        48_000,
        44_100,
        buffer,
        buffer.chunk_ns(),
        Arc::default(),
    )
    .unwrap();
    for _ in 0..200 {
        call(&fifo, &mut player, 480, 441);
    }
    let stats = player.stats();
    let skipped = player.skipped;
    // Held 25 ms: the call asks the whole buffer, 882 output frames.
    call(&fifo, &mut player, 1680, 882);
    call(&fifo, &mut player, 240, 441);
    for _ in 0..SETTLE_CALLS {
        call(&fifo, &mut player, 480, 441);
    }
    assert_eq!((stats.output_gaps(), stats.output_lost_frames()), (1, 480));
    // The output's clock counts it in output frames.
    assert_eq!(player.skipped, skipped + 441);
}

#[test]
fn a_priming_player_drops_nothing_and_owes_nothing() {
    let (fifo, mut player) = running(0);
    let stats = player.stats();
    // The usual call known, then a big call short of the priming's level.
    call(&fifo, &mut player, 100, 480);
    call(&fifo, &mut player, 700, 960);
    assert!(matches!(player.state, State::Priming(_)));
    assert_eq!(player.gap, GapState::default());
    assert_eq!((stats.output_gaps(), player.skipped), (0, 0));
}

#[test]
fn a_call_with_no_output_channels_drops_nothing() {
    let (fifo, mut player) = running(200);
    let stats = player.stats();
    let pushed = fifo.push_with(1680, |_, _| 0.5);
    let mut out = vec![0.0f32; 960];
    player.fill_pulled(pushed, &mut out, 0, |x| x as f32);
    assert_eq!((stats.output_gaps(), player.gap), (0, GapState::default()));
}
