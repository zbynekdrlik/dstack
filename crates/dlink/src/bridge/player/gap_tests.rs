// SPDX-License-Identifier: MIT OR Apache-2.0

//! The output's own gaps (dstack #307, [`Player::fill_pulled`],
//! [`output_gap`]): a fill-the-room output (WASAPI shared: the engine
//! reading a period at each tick) whose callback was held past its buffer.
//! At 48 kHz a period is 480 frames, 1 ms 48 frames. T is the held
//! event's tick; the call before it was on its tick unless said.

use super::*;

use crate::bridge::chunk_ns;

/// The calls `(pulled, call)` through [`output_gap`] at a usual call of
/// 480 and a buffer of `buffer`: the frames dropped at each and the gaps
/// ended.
fn run(buffer: usize, calls: &[(usize, usize)]) -> (Vec<usize>, u64) {
    let mut state = GapState::default();
    let mut ended = 0;
    let drops = calls
        .iter()
        .map(|&(pulled, call)| {
            let gap = output_gap(state, pulled, call, 480, buffer, 0);
            state = gap.state;
            ended += u64::from(gap.ended);
            gap.drop
        })
        .collect();
    (drops, ended)
}

/// A usual call on the grid, on time.
const ON: (usize, usize) = (480, 480);

#[test]
fn a_hold_drops_the_card_s_silence_and_keeps_the_phase() {
    // Held 25 ms on a buffer of two periods: the tick at T+20 found it
    // empty (480 of silence). The dry call reads 35 ms for 20: its excess
    // (720) holds one period of silence and 5 ms of phase, dropped at once;
    // the next, on the grid at T+30, reads 5 ms for a period.
    let (drops, ended) = run(960, &[(1680, 960), (240, 480), ON, ON, ON]);
    assert_eq!(drops, [480, 0, 0, 0, 0]);
    assert_eq!(ended, 1);
}

#[test]
fn late_wakes_after_the_hold_are_not_dropped_as_silence() {
    // The same hold, the next calls 3 ms late each (the lowest sum alone
    // dropped 624), then 6 ms late each.
    let (drops, ended) = run(960, &[(1680, 960), (384, 480), ON, ON, ON]);
    assert_eq!(drops.iter().sum::<usize>(), 480);
    assert_eq!(ended, 1);
    let (drops, _) = run(960, &[(1680, 960), (528, 480), ON, ON, ON]);
    assert_eq!(drops.iter().sum::<usize>(), 480);
}

#[test]
fn a_hold_the_buffer_covered_drops_nothing() {
    // Held 15 ms on a buffer of two periods: the tick at T+10 emptied it
    // (a dry call), the call came before the tick at T+20.
    let (drops, ended) = run(960, &[(1200, 960), (240, 480), ON, ON, ON]);
    assert_eq!(drops, [0; 5]);
    assert_eq!(ended, 0);
    // On Windows' 1056 frames 96 were left: no dry call at all.
    let (drops, ended) = run(1056, &[(1200, 960), (240, 480), ON, ON, ON]);
    assert_eq!(drops, [0; 5]);
    assert_eq!(ended, 0);
}

#[test]
fn a_hold_that_ended_right_at_a_tick_is_settled_whole() {
    // Held to 1 ms past the tick at T+20 (480 of silence), the call before
    // 2 ms late: the dry call's sum (432) is under a period.
    let (drops, ended) = run(960, &[(1392, 960), (480, 480), ON, ON, ON]);
    assert_eq!(drops, [0, 0, 0, 0, 480]);
    assert_eq!(ended, 1);
}

#[test]
fn a_buffer_of_two_periods_and_a_fraction_drops_its_partial_silence() {
    // Windows' 1056-frame buffer, held 25 ms: the tick at T+20 found 96
    // frames, 384 of silence.
    let (drops, _) = run(1056, &[(1680, 1056), (240, 480), ON, ON, ON]);
    assert_eq!(drops, [384, 0, 0, 0, 0]);
}

#[test]
fn the_field_s_46_ms_hold_drops_three_periods() {
    // Ticks at T+20, T+30, T+40 found it empty; the next call, on the grid
    // at T+50, reads 4 ms.
    let (drops, ended) = run(960, &[(2688, 960), (192, 480), ON, ON, ON]);
    assert_eq!(drops, [1440, 0, 0, 0, 0]);
    assert_eq!(ended, 1);
    // On 1056 frames: 1344.
    let (drops, _) = run(1056, &[(2688, 1056), (192, 480), ON, ON, ON]);
    assert_eq!(drops.iter().sum::<usize>(), 1344);
}

#[test]
fn two_holds_back_to_back_are_one_gap() {
    // Held 25 ms, then the next event (T+30) held 25 ms too: the ticks at
    // T+20 and T+50 found it empty (960).
    let (drops, ended) = run(960, &[(1680, 960), (1440, 960), (240, 480), ON, ON, ON]);
    assert_eq!(drops, [480, 480, 0, 0, 0, 0]);
    assert_eq!(ended, 1);
}

#[test]
fn a_pull_that_wrote_nothing_is_left_out_of_the_lowest_sum() {
    // Held to 1 ms past the tick at T+20, the call before 2 ms late (as
    // above: settled whole), and the call after the hold found the pull's
    // lock held (0 written); the next reads both calls' time.
    let (drops, _) = run(960, &[(1392, 960), (0, 480), (960, 480), ON, ON]);
    assert_eq!(drops, [0, 0, 0, 0, 480]);
}

#[test]
fn calls_that_are_not_dry_open_no_gap() {
    // Late calls of the usual size (they read more, take the same), a pull
    // skipped on the call before (0, then double), a big call short of
    // the buffer.
    let (drops, ended) = run(1056, &[(960, 480), (0, 480), (1200, 480), (1200, 960), ON]);
    assert_eq!(drops, [0; 5]);
    assert_eq!(ended, 0);
    // No usual call known yet, or a buffer no bigger than the usual call.
    for (usual, buffer) in [(0, 960), (480, 480)] {
        let gap = output_gap(GapState::default(), 4800, buffer, usual, buffer, 0);
        assert_eq!((gap.drop, gap.state), (0, GapState::default()));
    }
}

#[test]
fn the_silence_snaps_to_what_the_buffer_allows() {
    // A buffer of whole periods: whole periods, from a quarter period under
    // to three quarters over.
    assert_eq!(snap(359, 1, 960, 480, 120), 0);
    assert_eq!(snap(360, 1, 960, 480, 120), 480);
    assert_eq!(snap(480 - 120, 1, 960, 480, 120), 480);
    assert_eq!(snap(480 - 121, 1, 960, 480, 120), 0);
    assert_eq!(snap(-200, 1, 960, 480, 120), 0);
    // 1056 frames: 384 at least, then whole periods; nothing more than a
    // quarter period under the least.
    assert_eq!(snap(0, 1, 1056, 480, 120), 0);
    assert_eq!(snap(384 - 121, 1, 1056, 480, 120), 0);
    assert_eq!(snap(384 - 120, 1, 1056, 480, 120), 384);
    assert_eq!(snap(384 + 359, 1, 1056, 480, 120), 384);
    assert_eq!(snap(384 + 360, 1, 1056, 480, 120), 864);
    // Two dry calls: twice the least silence.
    assert_eq!(snap(768, 2, 1056, 480, 120), 768);
    // No slack (the dry call's): rounded down.
    assert_eq!(snap(959, 1, 960, 480, 0), 480);
    assert_eq!(snap(960, 1, 960, 480, 0), 960);
}

#[test]
fn the_call_before_the_hold_is_measured_against_its_floor() {
    // Held 41 ms on two periods (1440 of silence: T+20, T+30, T+40), the
    // call before 3 ms late (144 over its floor), the calls after on time:
    // the excess (1344) is the silence less that lateness plus a phase of
    // 1 ms; without the floor it reads a period under.
    let gap = |before| {
        let dry = output_gap(GapState::default(), 2304, 960, 480, 960, before);
        let mut state = dry.state;
        let mut dropped = dry.drop;
        for (pulled, call) in [(432, 480), ON, ON, ON] {
            let next = output_gap(state, pulled, call, 480, 960, 0);
            (state, dropped) = (next.state, dropped + next.drop);
        }
        dropped
    };
    assert_eq!(gap(144), 1440);
    assert_eq!(gap(0), 960);
}

#[test]
fn the_lateness_floor_is_the_least_of_the_recent_calls() {
    // A first call that wrote more than it took is the floor itself.
    let mut late = Lateness::default();
    late.add(2000, 1056);
    assert_eq!(late.over_floor(), 0);
    let mut late = Lateness::default();
    for _ in 0..RECENT_CALLS {
        late.add(480, 480);
    }
    assert_eq!(late.over_floor(), 0);
    // A call 3 ms late reads 3 ms more.
    late.add(624, 480);
    assert_eq!(late.over_floor(), 144);
    // A pull that wrote nothing is no lateness, and reads a call under the
    // floor (the next pull pays it); the next reads its time.
    late.add(0, 480);
    assert_eq!(late.over_floor(), -336);
    late.add(960, 480);
    assert_eq!(late.over_floor(), 144);
    // The floor moves with the recent calls only: four on time after.
    for _ in 0..RECENT_CALLS {
        late.add(336, 480);
        late.add(624, 480);
    }
    assert_eq!(late.over_floor(), 144);
    for _ in 0..RECENT_CALLS {
        late.add(480, 480);
    }
    assert_eq!(late.over_floor(), 0);
    // Forgotten (after a gap): the floor is the running sum now.
    late.add(624, 480);
    late.forget();
    assert_eq!(late.over_floor(), 0);
}

#[test]
fn a_pull_that_wrote_nothing_before_the_hold_is_paid_by_the_dry_call() {
    // The call before the hold found the pull's lock held: the dry call's
    // pull (45 ms) also reads that call's time. Held 25 ms on two periods:
    // 480 of silence.
    let mut late = Lateness::default();
    for _ in 0..RECENT_CALLS {
        late.add(480, 480);
    }
    late.add(0, 480);
    let before = late.over_floor();
    let dry = output_gap(GapState::default(), 2160, 960, 480, 960, before);
    let mut state = dry.state;
    let mut dropped = dry.drop;
    for (pulled, call) in [(240, 480), ON, ON, ON] {
        let next = output_gap(state, pulled, call, 480, 960, 0);
        (state, dropped) = (next.state, dropped + next.drop);
    }
    assert_eq!((dry.drop, dropped), (480, 480));
}

#[test]
fn the_settling_runs_its_calls_then_closes() {
    let gap = output_gap(GapState::default(), 1680, 960, 480, 960, 0);
    let open = GapState {
        sum: 720,
        least: 720,
        before: 0,
        left: SETTLE_CALLS,
        dry: 1,
        dropped: 480,
    };
    assert_eq!(gap.state, open);
    let gap = output_gap(gap.state, 240, 480, 480, 960, 0);
    assert_eq!(
        gap.state,
        GapState {
            sum: 480,
            least: 480,
            left: SETTLE_CALLS - 1,
            ..open
        }
    );
    // A dry call while settling counts its silence in and starts again.
    let gap = output_gap(gap.state, 1440, 960, 480, 960, 0);
    assert_eq!(
        gap.state,
        GapState {
            sum: 960,
            least: 960,
            before: 0,
            left: SETTLE_CALLS,
            dry: 2,
            dropped: 960
        }
    );
    assert_eq!((gap.drop, gap.ended), (480, false));
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
    for _ in 1..SETTLE_CALLS {
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
fn gaps_on_a_44_1_khz_output_are_dropped_in_network_frames() {
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
    // Two holds of 25 ms, apart: each call asks the whole buffer, 882
    // output frames.
    for _ in 0..2 {
        call(&fifo, &mut player, 1680, 882);
        call(&fifo, &mut player, 240, 441);
        for _ in 1..SETTLE_CALLS {
            call(&fifo, &mut player, 480, 441);
        }
    }
    assert_eq!((stats.output_gaps(), stats.output_lost_frames()), (2, 960));
    // The output's clock counts them in output frames, 441 each.
    assert_eq!(player.skipped, skipped + 882);
}

#[test]
fn the_player_measures_the_call_before_the_hold() {
    let (fifo, mut player) = running(200);
    let stats = player.stats();
    // The call before 3 ms late, then held 41 ms on two periods: ticks
    // at T+20, T+30, T+40 found it empty (1440).
    call(&fifo, &mut player, 624, 480);
    call(&fifo, &mut player, 2304, 960);
    call(&fifo, &mut player, 432, 480);
    for _ in 1..SETTLE_CALLS {
        call(&fifo, &mut player, 480, 480);
    }
    assert_eq!((stats.output_gaps(), stats.output_lost_frames()), (1, 1440));
    assert_eq!((stats.underruns(), stats.recentres()), (0, 0));
}

/// dstack #433 (v0.6.187 tag CI on windows-2025: 1 underrun, 1 re-centre,
/// 5088 frames dropped for the card's 5568 of silence): every wake there
/// is about 1 ms late (the runner's timer), so the floor's calls are 1 ms
/// past their ticks, and a hold that ended 0.3 ms past a tick reads its
/// excess 0.7 ms under the silence. The dry call drops a period under it
/// (no slack: never over), and the FIFO holds that period until the
/// settling drops it. Meanwhile the controller must not take that period
/// for an excess: its re-centre mid-call ran the rest of the big call dry.
#[test]
fn a_hold_that_ended_under_the_floor_s_lateness_is_settled_whole() {
    let (fifo, mut player) = running(200);
    let stats = player.stats();
    // Held past the tick at T+30 on Windows' 1056 frames: the ticks at T+20
    // and T+30 found 96 and nothing (864 of silence). The call before was
    // 1 ms past its tick (every call is), the dry call 0.3 ms past T+30:
    // 40 ms less 0.7 ms read for the whole buffer; the next, 1 ms past
    // T+40, reads the 0.7 ms back.
    call(&fifo, &mut player, 1920 - 34, 1056);
    call(&fifo, &mut player, 480 + 34, 480);
    for _ in 1..SETTLE_CALLS {
        call(&fifo, &mut player, 480, 480);
    }
    assert_eq!((stats.underruns(), stats.recentres()), (0, 0));
    assert_eq!((stats.output_gaps(), stats.output_lost_frames()), (1, 864));
}

/// A callback held on every call (each 35 ms after the last, a dry call
/// each time) keeps its gap open, so the controller makes no observation
/// (dstack #433): the gap's running sum still drops the card's silence, so
/// the depth neither runs dry nor grows, and the settling after it closes
/// the gap with the FIFO where it was.
#[test]
fn a_callback_held_on_every_call_keeps_the_depth() {
    let (fifo, mut player) = running(200);
    let stats = player.stats();
    let before = fifo.len();
    for _ in 0..40 {
        call(&fifo, &mut player, 1680, 1056);
    }
    call(&fifo, &mut player, 480, 480);
    for _ in 1..SETTLE_CALLS {
        call(&fifo, &mut player, 480, 480);
    }
    assert_eq!((stats.underruns(), stats.recentres()), (0, 0));
    assert_eq!(stats.output_gaps(), 1);
    assert!(
        fifo.len().abs_diff(before) <= 480,
        "{} {before}",
        fifo.len()
    );
}

#[test]
fn a_hold_soon_after_a_gap_is_measured_on_a_fresh_floor() {
    let (fifo, mut player) = running(200);
    let stats = player.stats();
    // The call before the first hold 3 ms late, held 41 ms (1440).
    call(&fifo, &mut player, 624, 480);
    call(&fifo, &mut player, 2304, 960);
    call(&fifo, &mut player, 432, 480);
    for _ in 1..SETTLE_CALLS {
        call(&fifo, &mut player, 480, 480);
    }
    // Two calls on time, then held 28 ms (480: the tick at T+20), the
    // call before on time: the floor from before the first gap would read
    // it 3 ms late and drop a period too many.
    call(&fifo, &mut player, 480, 480);
    call(&fifo, &mut player, 480, 480);
    call(&fifo, &mut player, 1824, 960);
    call(&fifo, &mut player, 96, 480);
    for _ in 1..SETTLE_CALLS {
        call(&fifo, &mut player, 480, 480);
    }
    assert_eq!((stats.output_gaps(), stats.output_lost_frames()), (2, 1920));
    assert_eq!((stats.underruns(), stats.recentres()), (0, 0));
}

#[test]
fn a_priming_player_drops_nothing_and_owes_nothing() {
    let (fifo, mut player) = running(0);
    let stats = player.stats();
    // The usual call known, then a dry call short of the priming's level.
    call(&fifo, &mut player, 100, 480);
    call(&fifo, &mut player, 100, 480);
    call(&fifo, &mut player, 700, 960);
    assert!(matches!(player.state, State::Priming(_)));
    assert_eq!(player.gap, GapState::default());
    assert_eq!((stats.output_gaps(), player.skipped), (0, 0));
}

#[test]
fn a_gap_cut_short_by_an_underrun_still_counts() {
    let (fifo, mut player) = running(200);
    let stats = player.stats();
    call(&fifo, &mut player, 1680, 960);
    // The FIFO emptied under the settling call: an underrun, priming.
    fifo.skip(fifo.len());
    call(&fifo, &mut player, 0, 480);
    call(&fifo, &mut player, 480, 480);
    assert!(matches!(player.state, State::Priming(_)));
    assert_eq!((stats.output_gaps(), stats.output_lost_frames()), (1, 480));
    assert_eq!(player.gap, GapState::default());
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
