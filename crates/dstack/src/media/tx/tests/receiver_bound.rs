// SPDX-License-Identifier: MIT OR Apache-2.0

//! A flow's lag bound is the latency its receiver plays it at (dstack
//! #426): its own stated latency, at least our advertised one (Dante's
//! rule, as [`PassClock::late_threshold`] counts late packets).

use super::*;

/// dstack #426 (`dlink/tests/tx_pull_counts.rs` on windows-2025, run
/// 44407677291): a TX thread whose sleep returned 8.5 ms late, on a sender
/// that advertises 8.36 ms toward a receiver that plays the flow at 13.67
/// ms, started its schedule again and threw away 27 packets the receiver
/// would still have played. A lag past our latency but within the
/// receiver's sends every packet (late, in order, each with its own
/// samples); a lag past the receiver's latency still starts it again.
#[test]
fn a_lag_past_our_latency_within_the_receiver_s_sends_every_packet() {
    let mut rig = Rig::new(Vec::new());
    assert!(rig.shared.set_source(0, Source::Counter));
    let (slot, _) = rig.flow(vec![Some(1)]);
    rig.tx.step(Some(T), 0);
    // The receiver's latency is read once the flow runs (`flows::receivers`):
    // 10 ms, ten times our 1 ms.
    rig.shared.set_receiver_latency(slot, v4(&rig.rx), 10 * MS);
    let due = first_due_ns(T);
    rig.tx.step(Some(due + 5 * MS), 0);
    let lane = rig.shared.lane(slot);
    assert_eq!(
        lane.rebootstraps, 0,
        "within the receiver's latency: {lane:?}"
    );
    // The first packet, due at `due`, and every one due in the 5 ms since
    // (333 us apart).
    let packets = drain(&rig.rx);
    assert!((15..=17).contains(&packets.len()), "{}", packets.len());
    let first = stamp_of(&packets[0]);
    for (k, p) in packets.iter().enumerate() {
        let t = stamp_of(p);
        assert_eq!(t, first + k as u64 * FPP, "every stamp, in turn");
        assert_eq!(slot_samples(p, 1, 0), counter(t, 32), "its own samples");
    }
    assert_eq!(lane.packets, packets.len() as u64);
    // Past the receiver's latency: started again, no late packet sent.
    let last = stamp_of(packets.last().expect("a packet"));
    let next_due = ns_of(last + 2 * FPP, RATE) + TX_MARGIN_NS;
    rig.tx.step(Some(next_due + 11 * MS), 0);
    assert!(drain(&rig.rx).is_empty(), "no burst past its latency");
    assert_eq!(rig.shared.lane(slot).rebootstraps, 1);
}
