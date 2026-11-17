// SPDX-License-Identifier: MIT OR Apache-2.0

//! The output's own gaps (dstack #307, [`Player::fill_pulled`],
//! [`output_gap`]): a fill-the-room output (WASAPI shared: two periods of
//! buffer, the engine reading a period at each tick) whose callback was
//! held past its buffer. At 48 kHz a period is 480 frames.

use super::*;

use crate::bridge::chunk_ns;

/// The gap a call decides with nothing owed.
fn gap(pulled: usize, call: usize, usual: usize) -> Gap {
    output_gap(0, pulled, call, usual)
}

#[test]
fn whole_periods_the_engine_found_empty_are_the_output_s_gap() {
    // Held 25 ms: the pull read 35 ms, the call asks the whole buffer;
    // the tick at 20 ms found it empty (one period), the 5 ms past it are
    // the call's phase off the grid, owed to the next call.
    assert_eq!(
        gap(1680, 960, 480),
        Gap {
            drop: 480,
            owed: 240
        }
    );
    // Held 46 ms (the field's worst): three empty ticks.
    assert_eq!(
        gap(480 + 2208, 960, 480),
        Gap {
            drop: 1440,
            owed: 288
        }
    );
    // Held 15 ms: the call came before the tick at 20 ms, nothing lost.
    assert_eq!(gap(1200, 960, 480), Gap { drop: 0, owed: 240 });
    // A pull short of the big call's take (the call before was late) owes
    // the shortfall.
    assert_eq!(gap(900, 960, 480), Gap { drop: 0, owed: -60 });
}

#[test]
fn the_call_after_a_gap_settles_what_was_owed() {
    // Held 25 ms (owed 240 = the 5 ms phase): the next call, on the grid,
    // reads 5 ms for a period: nothing more.
    assert_eq!(output_gap(240, 240, 480, 480), Gap { drop: 0, owed: 0 });
    // Held to 1 ms past the tick at 20 ms, the call before 2 ms late after
    // its own tick: the excess (10 ms less the 1 ms the lateness took) was
    // owed whole; the next call, 1 ms late itself, reads 9 ms + 1 ms for a
    // period: the tick at 20 ms was missed, one period dropped now.
    assert_eq!(output_gap(432, 480, 480, 480), Gap { drop: 480, owed: 0 });
    // Rounded to the nearest period: just under half a period is no gap,
    // half a period is one.
    assert_eq!(output_gap(239, 480, 480, 480), Gap { drop: 0, owed: 0 });
    assert_eq!(output_gap(240, 480, 480, 480), Gap { drop: 480, owed: 0 });
    // A short read after a short pull drops nothing.
    assert_eq!(output_gap(-60, 0, 480, 480), Gap { drop: 0, owed: 0 });
    // Two periods.
    assert_eq!(output_gap(960, 480, 480, 480), Gap { drop: 960, owed: 0 });
}

#[test]
fn a_call_no_bigger_than_the_usual_one_with_nothing_owed_is_no_gap() {
    // A fixed-size callback late by 10 ms: it reads 10 ms more and takes
    // the same; the next reads 10 ms less.
    assert_eq!(gap(960, 480, 480), Gap { drop: 0, owed: 0 });
    assert_eq!(gap(960, 400, 480), Gap { drop: 0, owed: 0 });
    // A pull under its take never drops.
    assert_eq!(
        gap(0, 960, 480),
        Gap {
            drop: 0,
            owed: -960
        }
    );
    // No usual call known yet (a new stream's first call), owed or not.
    assert_eq!(output_gap(240, 4800, 960, 0), Gap { drop: 0, owed: 0 });
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
fn the_output_s_gap_is_dropped_once_and_counted_as_its_own() {
    let (fifo, mut player) = running(200);
    assert_eq!(player.state, State::Running);
    let stats = player.stats();
    let before = fifo.len();
    let skipped = player.skipped;
    // Held 25 ms: one period of the card's silence.
    call(&fifo, &mut player, 1680, 960);
    assert_eq!((stats.output_gaps(), stats.output_lost_frames()), (1, 480));
    assert_eq!((stats.underruns(), stats.recentres()), (0, 0));
    // The output's clock counts the time the card played silence.
    assert_eq!(player.skipped, skipped + 480);
    // The next call, on the engine's grid again 5 ms later, reads 5 ms
    // and takes a period: the depth is the depth before, within the
    // converter's call (it takes whole blocks of input), so the latency is
    // the same (kept, the 480 frames would show).
    call(&fifo, &mut player, 240, 480);
    assert_eq!(stats.output_gaps(), 1);
    assert!(
        fifo.len().abs_diff(before) <= BLOCK,
        "{} {before}",
        fifo.len()
    );
}

#[test]
fn a_hold_the_buffer_covered_drops_nothing() {
    let (fifo, mut player) = running(200);
    let stats = player.stats();
    let before = fifo.len();
    // Held 15 ms: the call (the whole buffer) came before the tick at
    // 20 ms; the next, on the grid at 20 ms, reads 5 ms and takes a period.
    call(&fifo, &mut player, 1200, 960);
    call(&fifo, &mut player, 240, 480);
    assert_eq!((stats.output_gaps(), stats.recentres()), (0, 0));
    assert!(
        fifo.len().abs_diff(before) <= BLOCK,
        "{} {before}",
        fifo.len()
    );
    // That big call is no usual one: a gap after it is still seen.
    call(&fifo, &mut player, 1440, 960);
    assert_eq!((stats.output_gaps(), stats.output_lost_frames()), (1, 480));
}

#[test]
fn a_gap_right_after_a_gap_is_dropped_too() {
    let (fifo, mut player) = running(200);
    let stats = player.stats();
    // Held 25 ms, then the next event (at 30 ms) held 25 ms too: the
    // ticks at 30 and 40 ms read the first call's buffer, the one at 50 ms
    // found it empty.
    call(&fifo, &mut player, 1680, 960);
    call(&fifo, &mut player, 1440, 960);
    assert_eq!((stats.output_gaps(), stats.output_lost_frames()), (2, 960));
}

#[test]
fn a_smaller_call_that_is_no_gap_becomes_the_usual_one() {
    let (fifo, mut player) = running(200);
    let stats = player.stats();
    call(&fifo, &mut player, 400, 400);
    // Bigger than the 400-frame usual call, a period and more over it.
    call(&fifo, &mut player, 1200, 480);
    assert_eq!((stats.output_gaps(), stats.output_lost_frames()), (1, 400));
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
    assert_eq!((stats.output_gaps(), stats.output_lost_frames()), (1, 480));
    // The output's clock counts it in output frames.
    assert_eq!(player.skipped, skipped + 441);
}

#[test]
fn a_priming_player_drops_nothing() {
    let (fifo, mut player) = running(0);
    let stats = player.stats();
    call(&fifo, &mut player, 480, 480);
    assert!(matches!(player.state, State::Priming(_)));
    call(&fifo, &mut player, 1680, 960);
    assert_eq!(stats.output_gaps(), 0);
    assert_eq!(player.skipped, 0);
}

#[test]
fn a_call_with_no_output_channels_drops_nothing() {
    let (fifo, mut player) = running(200);
    let stats = player.stats();
    let pushed = fifo.push_with(1680, |_, _| 0.5);
    let mut out = vec![0.0f32; 960];
    player.fill_pulled(pushed, &mut out, 0, |x| x as f32);
    assert_eq!(stats.output_gaps(), 0);
}

#[test]
fn a_gap_that_ended_right_at_a_tick_is_dropped_by_the_next_call() {
    let (fifo, mut player) = running(200);
    let stats = player.stats();
    let before = fifo.len();
    // Held to 1 ms past the tick at 20 ms, the call before 2 ms late
    // after its own tick: the excess (432) is under a period, owed; the
    // next call, 1 ms late itself, reads 11 ms for a period and settles
    // the missed tick.
    call(&fifo, &mut player, 1392, 960);
    assert_eq!(stats.output_gaps(), 0);
    call(&fifo, &mut player, 528, 480);
    assert_eq!((stats.output_gaps(), stats.output_lost_frames()), (1, 480));
    assert_eq!((stats.underruns(), stats.recentres()), (0, 0));
    assert!(
        fifo.len().abs_diff(before) <= BLOCK,
        "{} {before}",
        fifo.len()
    );
}

#[test]
fn a_priming_player_owes_nothing() {
    let (fifo, mut player) = running(0);
    // The usual call known, then a big call short of the priming's level:
    // running, it would owe its shortfall.
    call(&fifo, &mut player, 100, 480);
    call(&fifo, &mut player, 700, 960);
    assert!(matches!(player.state, State::Priming(_)));
    assert_eq!(player.owed_in, 0);
}
