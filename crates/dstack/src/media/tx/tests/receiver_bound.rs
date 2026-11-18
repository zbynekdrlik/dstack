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

/// A receiver's stated latency as a lag bound: samples at the rate
/// (rounded as every latency is, `clock::latency_samples`), 0 while
/// unknown, never past the engine ring's reach.
#[test]
fn a_receiver_s_latency_is_a_lag_of_its_samples_within_the_ring_s_reach() {
    assert_eq!(receiver_lag(0, RATE), 0, "unknown");
    // 13666666 ns at 96 kHz (dlink's RX-only P at 96 kHz): 1312 samples.
    assert_eq!(receiver_lag(13_666_666, RATE), 1_312);
    assert_eq!(receiver_lag(10 * MS, 48_000), 480);
    let max = crate::device::MAX_LATENCY_SAMPLES;
    let at_max = max * 1_000_000_000 / u64::from(RATE);
    assert_eq!(receiver_lag(at_max, RATE), max);
    assert_eq!(receiver_lag(at_max + 1_000_000, RATE), max, "capped");
    assert_eq!(receiver_lag(u64::MAX / RATE as u64, RATE), max, "capped");
}

/// The schedule's bound is ours, or the receiver's when it is longer; a
/// receiver that states less plays at ours (Dante's rule), so ours stays.
/// The restart line names the bound it judged by: a lag at it still sends,
/// one past it restarts.
#[test]
fn the_schedule_holds_on_for_the_longer_of_the_two_latencies() {
    let mut s = Schedule::bootstrap(1_000, 32, 96);
    assert_eq!(s.bound(), 96, "unknown receiver: ours");
    s.receiver_lag = 48;
    assert_eq!(s.bound(), 96, "a shorter receiver: ours");
    s.receiver_lag = 480;
    assert_eq!(s.bound(), 480);
    // The first packet (stamp 1024) is due at 1056: 480 past it still goes.
    assert_eq!(s.due(1_056 + 480), Some(1_024));
    crate::test_log::undecided();
    let warnings = crate::test_log::capture_warnings(|| {
        // The next (stamp 1056) is due at 1088: 481 past it restarts.
        assert_eq!(s.due(1_088 + 481), None);
    });
    assert_eq!(s.take_rebootstraps(), 1);
    assert!(warnings.contains("lag=481 max_lag=480 "), "{warnings}");
}

/// The TX pull's reach takes the largest bound of the flows that send: ours
/// with no flow or an unknown receiver, a receiver's when longer.
#[test]
fn the_reach_is_the_longest_receiver_latency_of_the_flows_that_send() {
    let mut rig = Rig::new(Vec::new());
    assert_eq!(rig.tx.reach_lag(), 96, "no flow: ours");
    let (a, _) = rig.flow(vec![Some(1)]);
    let (b, _) = add(
        &rig.shared,
        &FlowRequest {
            channels: vec![Some(2)],
            ..req(v4(&rig.stranger))
        },
    );
    rig.tx.step(Some(T), 0);
    assert_eq!(rig.tx.reach_lag(), 96, "unknown receivers: ours");
    rig.shared.set_receiver_latency(a, v4(&rig.rx), 2 * MS);
    rig.shared
        .set_receiver_latency(b, v4(&rig.stranger), 5 * MS);
    assert_eq!(rig.tx.reach_lag(), 480);
    rig.shared
        .set_receiver_latency(b, v4(&rig.stranger), MS / 2);
    assert_eq!(rig.tx.reach_lag(), 192);
}
