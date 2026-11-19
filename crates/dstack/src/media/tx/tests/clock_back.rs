// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #440's residual window on the TX side: a pass that reads the new
//! time of a step back under the old epoch finds its next packet due the
//! whole step ahead. Under the old rule (a jump back only past 2 s at
//! 96 kHz) the flow sent nothing for the step; a schedule further ahead of
//! the clock than the flow's bound and a packet is a clock gone back: it
//! starts again at once (counted, the restart line), and the next pass at
//! the new epoch keeps it there.

use super::*;

/// The steps back: 100 ms and 1 s, in samples at 96 kHz.
const BACK: [u64; 2] = [9_600, 96_000];

/// A schedule a step back ahead of the clock starts again at once on the
/// fpp grid after the new time, counted, and sends from there.
#[test]
fn a_schedule_a_step_back_ahead_starts_again_at_once() {
    for back in BACK {
        let mut s = Schedule::bootstrap(1_000_000, 32, 96);
        let now = 1_000_000 - back;
        assert_eq!(s.due(now), None, "{back} back");
        assert_eq!(s.take_rebootstraps(), 1, "{back} back");
        assert_eq!(s.next, now - now % 32 + 32, "{back} back");
        assert_eq!(s.due(s.next + 32), Some(s.next - 32), "{back} back");
    }
}

/// On the transmitter: a pass reads the time a step back with its old
/// epoch, the next pass the new epoch with the step. The flow starts again
/// at once and sends from the new time, its stamps on the packet grid,
/// never waiting the step out. The re-anchor keeps the restarted schedule
/// (ROZHODNUTÉ 19.11: a one-pass mark) instead of moving it the step back
/// once more: one restart, the step costs one packet.
#[test]
fn a_time_read_back_before_its_epoch_restarts_once_and_sends_at_once() {
    for back in BACK {
        let mut rig = Rig::new(Vec::new());
        let (slot, _) = rig.flow(vec![Some(1)]);
        for us in (0..=3_000).step_by(500) {
            rig.tx.step(Some(T + us * 1_000), 1);
        }
        drain(&rig.rx);
        let back_ns = ns_of(back, RATE);
        let new = T + 3_500_000 - back_ns;
        rig.tx.step(Some(new), 1);
        assert_eq!(rig.shared.lane(slot).rebootstraps, 1, "{back} back");
        let step_ns = -i64::try_from(back_ns).unwrap();
        let mut packets = Vec::new();
        for us in (500..=3_000).step_by(500) {
            rig.tx.step(Some(new + us * 1_000), stepped(2, step_ns));
            packets.extend(drain(&rig.rx));
        }
        assert_eq!(rig.shared.lane(slot).rebootstraps, 1, "{back} back");
        // 2.5 ms of passes: 7 packets on a quiet path (the epoch pass
        // restarts nothing, so it sends what is due).
        assert!(
            packets.len() >= 7,
            "{back} back: sending on at once: {}",
            packets.len()
        );
        assert!(
            stamp_steps(&packets).iter().all(|&d| d == FPP),
            "{back} back"
        );
    }
}

/// The limit is the bound, at least the device's block, and a packet: a
/// re-anchor moves the next stamp up to our latency (at least the block)
/// and a packet past now, so a moved schedule is never taken for a clock
/// gone back.
#[test]
fn the_ahead_limit_is_the_bound_at_least_the_block_and_a_packet() {
    // fpp 32, our latency 24 samples (0.25 ms at 96 kHz).
    let mut s = Schedule::bootstrap(1_000_000, 32, 24);
    assert_eq!(s.ahead_limit(), 56);
    s.block = 64;
    assert_eq!(s.ahead_limit(), 96);
    s.receiver_lag = 200;
    assert_eq!(s.ahead_limit(), 232);
    // A block of 64: the next stamp exactly 96 ahead waits, 97 restarts.
    let fresh = || Schedule {
        block: 64,
        ..Schedule::bootstrap(1_000_000, 32, 24)
    };
    let mut s = fresh();
    assert_eq!(s.due(s.next - 96), None);
    assert_eq!(s.take_rebootstraps(), 0);
    let mut s = fresh();
    assert_eq!(s.due(s.next - 97), None);
    assert_eq!(s.take_rebootstraps(), 1);
}

/// A schedule moved by a re-anchor to the edge of its room (block 64 over a
/// latency of 24, as `a_re_anchor_s_room_is_at_least_the_block`) waits its
/// packet instead of starting again.
#[test]
fn a_schedule_moved_to_the_edge_of_its_room_waits() {
    let now = 1_000_000;
    let mut s = Schedule {
        block: 64,
        ..Schedule::bootstrap(now - 64, 32, 24)
    };
    assert_eq!(s.re_anchor(Some(128), now), Shift::Moved);
    assert_eq!(s.next, now + 96);
    assert_eq!(s.due(now), None);
    assert_eq!(s.take_rebootstraps(), 0);
    assert_eq!(s.due(now + 128), Some(now + 96));
}

/// The TX thread gives each lane's schedule the device's block.
#[test]
fn a_lane_s_schedule_carries_the_device_s_block() {
    let mut rig = Rig::new(Vec::new());
    let (slot, _) = rig.flow(vec![Some(1)]);
    rig.tx.step(Some(T), 1);
    let block = rig.shared.cfg.block;
    assert!(block > 0);
    let schedule = rig.tx.lanes[slot].schedule.expect("a schedule");
    assert_eq!(schedule.block, block);
}

/// A clock-back restart marks the schedule: the next re-anchor's step back
/// keeps it at the new time instead of moving it the step behind
/// (ROZHODNUTÉ 19.11); that re-anchor spends the mark, the one after moves
/// as it always did.
#[test]
fn a_re_anchor_back_after_a_clock_back_restart_keeps_the_schedule() {
    for back in BACK {
        let mut s = Schedule::bootstrap(1_000_000, 32, 96);
        let now = 1_000_000 - back;
        assert_eq!(s.due(now), None, "{back} back");
        assert_eq!(s.take_rebootstraps(), 1, "{back} back");
        let restarted = s.next;
        let by = -i64::try_from(back).unwrap();
        assert_eq!(s.re_anchor(Some(by), now), Shift::Kept, "{back} back");
        assert_eq!(
            (s.next, s.take_rebootstraps()),
            (restarted, 0),
            "{back} back"
        );
        assert_eq!(s.re_anchor(Some(by), now), Shift::Moved, "{back} back");
        assert_eq!(s.next, restarted - back, "{back} back");
    }
}

/// A restart for a lag past the bound marks nothing: a re-anchor back after
/// it moves the schedule, as before #440's mark.
#[test]
fn a_re_anchor_back_after_a_lag_restart_moves_the_schedule() {
    let mut s = Schedule::bootstrap(1_000_000, 32, 96);
    let late = s.next + 32 + 96 + 1;
    assert_eq!(s.due(late), None);
    assert_eq!(s.take_rebootstraps(), 1);
    let restarted = s.next;
    assert_eq!(s.re_anchor(Some(-320), late), Shift::Moved);
    assert_eq!(s.next, restarted - 320);
}

/// The mark keeps a step BACK only: a forward step after a clock-back
/// restart moves the schedule as before.
#[test]
fn a_re_anchor_forward_after_a_clock_back_restart_moves_the_schedule() {
    let mut s = Schedule::bootstrap(1_000_000, 32, 96);
    let now = 1_000_000 - 9_600;
    assert_eq!(s.due(now), None);
    assert_eq!(s.take_rebootstraps(), 1);
    let restarted = s.next;
    assert_eq!(s.re_anchor(Some(32), now), Shift::Moved);
    assert_eq!(s.next, restarted + 32);
}

/// On the transmitter the mark lasts one pass: a pass with no re-anchor
/// after the clock-back restart spends it, and an epoch with the step that
/// comes only after it moves the schedule the step back once more, a
/// second restart, as before the mark.
#[test]
fn a_clock_back_mark_lasts_one_pass() {
    for back in BACK {
        let mut rig = Rig::new(Vec::new());
        let (slot, _) = rig.flow(vec![Some(1)]);
        for us in (0..=3_000).step_by(500) {
            rig.tx.step(Some(T + us * 1_000), 1);
        }
        let back_ns = ns_of(back, RATE);
        let new = T + 3_500_000 - back_ns;
        rig.tx.step(Some(new), 1);
        rig.tx.step(Some(new + 500_000), 1);
        let step_ns = -i64::try_from(back_ns).unwrap();
        rig.tx.step(Some(new + 1_000_000), stepped(2, step_ns));
        assert_eq!(rig.shared.lane(slot).rebootstraps, 2, "{back} back");
    }
}
