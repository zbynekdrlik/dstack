// SPDX-License-Identifier: MIT OR Apache-2.0

//! The app's view of the TX side (dstack #325): its own packet-age peak,
//! which the late watch's take never starves, and each receiver's lateness.

use super::*;

/// dstack #325 (dasio's bridge status): the app takes the oldest packet
/// since its last view while the late watch takes its own peak every
/// second; each receiver is named with its stated latency and the slot's
/// late count.
#[test]
fn the_app_view_keeps_its_own_age_peak_beside_the_late_watch() {
    let mut cfg = cfg();
    cfg.tx_latency_ns = Some(500_000);
    let shared =
        Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, Vec::new().into(), 1, &[]).unwrap());
    let mut rig = Rig::with(shared);
    assert_eq!(
        rig.shared.take_app_view(),
        AppView::default(),
        "nothing yet"
    );
    let (slot, _) = rig.flow(vec![Some(1), Some(2)]);
    rig.shared.set_receiver_latency(slot, v4(&rig.rx), 250_000);
    rig.tx.step(Some(T), 0);
    rig.tx.step(Some(first_due_ns(T) + 50_000), 0);
    let lane = rig.shared.lane(slot);
    assert!(lane.over_latency >= 1, "{lane:?}");
    assert!(lane.age_max_ns > 0, "{lane:?}");
    // The late watch takes its peak first: the app's stays.
    assert_eq!(rig.shared.take_age_peak(slot), lane.age_max_ns);
    let view = rig.shared.take_app_view();
    assert_eq!(
        view,
        AppView {
            age_peak_ns: lane.age_max_ns,
            receivers: vec![ReceiverLateness {
                slot,
                receiver: "peer-device".to_string(),
                latency_ns: 250_000,
                over_latency: lane.over_latency,
            }],
        }
    );
    // Taken: the next view has no packet, the receiver stays.
    let next = rig.shared.take_app_view();
    assert_eq!(next.age_peak_ns, 0);
    assert_eq!(next.receivers, view.receivers);
    // The app's take left the late watch's peak alone too.
    // The next packet, one packet's time on (a jump past the latency
    // would start the schedule again and send none).
    rig.tx
        .step(Some(first_due_ns(T) + ns_of(FPP, RATE) + 50_000), 0);
    let peak = rig.shared.take_app_view().age_peak_ns;
    assert!(peak > 0);
    assert_eq!(rig.shared.take_age_peak(slot), peak);
}
