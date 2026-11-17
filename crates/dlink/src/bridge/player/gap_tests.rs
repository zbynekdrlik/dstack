// SPDX-License-Identifier: MIT OR Apache-2.0

//! The output's own gaps (dstack #307, [`Player::fill_pulled`],
//! [`output_gap`]): a fill-the-room output whose callback was held past its
//! buffer.

use super::*;

use crate::bridge::chunk_ns;

#[test]
fn a_bigger_call_whose_pull_exceeds_its_take_is_the_output_s_gap() {
    // The field's 20 ms hold on a 2-period WASAPI buffer at 48 kHz: the
    // pull read 30 ms, the call asks the whole buffer.
    assert_eq!(output_gap(1440, 960, 480, 128), 480);
    // The bound is strict: a pull one converter call over its take is the
    // clocks' reads, one frame more is a gap.
    assert_eq!(output_gap(960 + 128, 960, 480, 128), 0);
    assert_eq!(output_gap(960 + 129, 960, 480, 128), 129);
}

#[test]
fn a_call_no_bigger_than_the_usual_one_is_no_gap() {
    // A fixed-size callback late by 10 ms: it reads 10 ms more and takes
    // the same; the next reads 10 ms less.
    assert_eq!(output_gap(960, 480, 480, 128), 0);
    assert_eq!(output_gap(960, 400, 480, 128), 0);
    // A late call the card's buffer covered: it takes what it read.
    assert_eq!(output_gap(700, 700, 480, 128), 0);
    // A pull under its take (a skipped pull before) never underflows.
    assert_eq!(output_gap(0, 960, 480, 128), 0);
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
    // The callback held 20 ms past its period: the pull read 30 ms, the
    // call asks the whole buffer (20 ms), the card played 10 ms of silence.
    call(&fifo, &mut player, 1440, 960);
    assert_eq!((stats.output_gaps(), stats.output_lost_frames()), (1, 480));
    assert_eq!((stats.underruns(), stats.recentres()), (0, 0));
    // The depth after the call is the depth after a usual one, within the
    // converter's call (it takes whole blocks of input): the latency is the
    // same (kept, the 480 frames would show).
    assert!(
        fifo.len().abs_diff(before) <= BLOCK,
        "{} {before}",
        fifo.len()
    );
    // The output's clock counts the time the card played silence.
    assert_eq!(player.skipped, skipped + 480);
    // The next usual call is no gap.
    call(&fifo, &mut player, 480, 480);
    assert_eq!(stats.output_gaps(), 1);
    assert!(
        fifo.len().abs_diff(before) <= BLOCK,
        "{} {before}",
        fifo.len()
    );
}

#[test]
fn a_gap_right_after_a_gap_is_dropped_too() {
    let (fifo, mut player) = running(200);
    let stats = player.stats();
    let before = fifo.len();
    call(&fifo, &mut player, 1440, 960);
    call(&fifo, &mut player, 1440, 960);
    assert_eq!((stats.output_gaps(), stats.output_lost_frames()), (2, 960));
    assert!(
        fifo.len().abs_diff(before) <= BLOCK,
        "{} {before}",
        fifo.len()
    );
}

#[test]
fn a_call_that_is_no_gap_becomes_the_usual_one() {
    let (fifo, mut player) = running(200);
    let stats = player.stats();
    // The output's calls grow to 600 frames (no gap: it reads what it
    // takes); a later 600-frame call that reads 1200 is no bigger than the
    // usual one, so it is no gap.
    call(&fifo, &mut player, 600, 600);
    call(&fifo, &mut player, 1200, 600);
    assert_eq!(stats.output_gaps(), 0);
}

#[test]
fn a_priming_player_drops_nothing() {
    let (fifo, mut player) = running(0);
    let stats = player.stats();
    call(&fifo, &mut player, 480, 480);
    assert!(matches!(player.state, State::Priming(_)));
    call(&fifo, &mut player, 1440, 960);
    assert_eq!(stats.output_gaps(), 0);
    assert_eq!(player.skipped, 0);
}

#[test]
fn a_call_with_no_output_channels_drops_nothing() {
    let (fifo, mut player) = running(200);
    let stats = player.stats();
    let pushed = fifo.push_with(1440, |_, _| 0.5);
    let mut out = vec![0.0f32; 960];
    player.fill_pulled(pushed, &mut out, 0, |x| x as f32);
    assert_eq!(stats.output_gaps(), 0);
}
