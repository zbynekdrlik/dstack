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
/// once and sends at once from the new time, its stamps on the packet
/// grid, never waiting the step out.
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
