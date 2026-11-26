// SPDX-License-Identifier: MIT OR Apache-2.0

//! Each TX lane's oldest packet over the last 10 s (dstack #500, dasio#237):
//! a window the late watch feeds and any number of readers load.

use super::*;
use crate::media::late::{EVERY_NS, LateWatch};
use crate::media::tx::{AGE_WINDOW_LOOKS, TxAgeLane};
use std::time::Instant;

fn device() -> Rig {
    let mut cfg = cfg();
    cfg.tx_latency_ns = Some(500_000);
    let shared =
        Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, Vec::new().into(), 1, &[]).unwrap());
    Rig::with(shared)
}

/// A device without a late watch never fills the window: every lane reads
/// `None`, not a 0 that would claim no old packet.
#[test]
fn without_a_late_watch_the_view_has_no_window() {
    let rig = device();
    let view = rig.shared.tx_age_view();
    assert_eq!(view.len(), MAX_FLOWS);
    for (slot, lane) in view.iter().enumerate() {
        assert_eq!(
            *lane,
            TxAgeLane {
                slot,
                receiver: String::new(),
                receiver_latency_ns: 0,
                age_max_10s_ns: None,
                over_latency: 0,
                late_threshold_ns: None,
            }
        );
    }
    assert_eq!(rig.shared.tx_age_looks(), 0);
    // A peak nobody took stays out of the view (it is the late watch's).
    rig.shared.lanes[3]
        .age_peak_ns
        .store(7_000, Ordering::Relaxed);
    assert_eq!(rig.shared.tx_age_view()[3].age_max_10s_ns, None);
    assert_eq!(rig.shared.take_age_peak(3), 7_000);
}

/// Two readers see the same window and take nothing from the late watch;
/// a peak stays for 10 looks and leaves at the 11th; a lane with no flow
/// reads 0.
#[test]
fn readers_share_the_window_and_a_peak_leaves_after_ten_looks() {
    let mut rig = device();
    let (slot, _) = rig.flow(vec![Some(1), Some(2)]);
    rig.shared.set_receiver_latency(slot, v4(&rig.rx), 250_000);
    rig.tx.step(Some(T), 0);
    rig.tx.step(Some(first_due_ns(T) + 50_000), 0);
    let lane = rig.shared.lane(slot);
    assert!(lane.over_latency >= 1, "{lane:?}");
    assert!(lane.age_max_ns > 0, "{lane:?}");
    let peak = lane.age_max_ns;
    let mut watch = LateWatch::new();
    // Look 1 takes the peak into the window.
    watch.watch(0, &rig.shared);
    assert_eq!(rig.shared.tx_age_looks(), 1);
    let first = rig.shared.tx_age_view();
    let second = rig.shared.tx_age_view();
    assert_eq!(first, second, "two readers, one window");
    assert_eq!(
        first[slot],
        TxAgeLane {
            slot,
            receiver: "peer-device".to_string(),
            receiver_latency_ns: 250_000,
            age_max_10s_ns: Some(peak),
            over_latency: lane.over_latency,
            // A receiver at 250 us plays our flow at our 500 us at least
            // (Dante's rule), and a 32-frame packet at its floor, 633 us
            // (frames + margin + send slack): not at its stated latency.
            late_threshold_ns: Some(633_334),
        }
    );
    let other = (slot + 1) % MAX_FLOWS;
    assert_eq!(
        first[other],
        TxAgeLane {
            slot: other,
            receiver: String::new(),
            receiver_latency_ns: 0,
            age_max_10s_ns: Some(0),
            over_latency: 0,
            late_threshold_ns: None,
        },
        "a lane with no flow reads 0"
    );
    // A reader takes nothing: a peak written now is still the late watch's
    // at its next look, and a smaller one never hides the window's max.
    rig.shared.lanes[slot]
        .age_peak_ns
        .store(peak / 2, Ordering::Relaxed);
    let _ = rig.shared.tx_age_view();
    let looks = u64::try_from(AGE_WINDOW_LOOKS).unwrap();
    // Looks 2..=10: the first one's peak is still in the window.
    for k in 1..looks {
        watch.watch(k * EVERY_NS, &rig.shared);
        assert_eq!(rig.shared.tx_age_view()[slot].age_max_10s_ns, Some(peak));
    }
    assert_eq!(rig.shared.tx_age_looks(), looks);
    // Look 11 writes over look 1: the half peak of look 2 is the max now.
    watch.watch(looks * EVERY_NS, &rig.shared);
    assert_eq!(
        rig.shared.tx_age_view()[slot].age_max_10s_ns,
        Some(peak / 2)
    );
    // Look 12 writes over look 2: nothing sent since, 0.
    watch.watch((looks + 1) * EVERY_NS, &rig.shared);
    let view = rig.shared.tx_age_view();
    assert_eq!(view[slot].age_max_10s_ns, Some(0));
    assert_eq!(view[slot].over_latency, lane.over_latency, "lifetime");
    assert_eq!(view[slot].receiver, "peer-device");
}

/// The window is the late watch's takes, slot by slot: `take_age_peaks`
/// returns what it wrote and leaves every peak taken.
#[test]
fn take_age_peaks_takes_every_slot_into_the_window() {
    let rig = device();
    rig.shared.lanes[0].age_peak_ns.store(5, Ordering::Relaxed);
    rig.shared.lanes[MAX_FLOWS - 1]
        .age_peak_ns
        .store(9, Ordering::Relaxed);
    let taken = rig.shared.take_age_peaks();
    assert_eq!(taken[0], 5);
    assert_eq!(taken[MAX_FLOWS - 1], 9);
    assert!(taken[1..MAX_FLOWS - 1].iter().all(|&ns| ns == 0));
    assert_eq!(rig.shared.take_age_peak(0), 0, "taken");
    let view = rig.shared.tx_age_view();
    assert_eq!(view[0].age_max_10s_ns, Some(5));
    assert_eq!(view[MAX_FLOWS - 1].age_max_10s_ns, Some(9));
    assert_eq!(view[1].age_max_10s_ns, Some(0));
}

/// Only a flow sending now to one device names a receiver: an ended flow
/// and a multicast one read empty with latency 0, as `take_app_view` reads
/// them, even with a latency stated for their destination.
#[test]
fn an_ended_or_multicast_flow_names_no_receiver() {
    let shared = shared_with(Vec::new());
    let (ended, _) = add(&shared, &req(peer(4000)));
    let (live, _) = add(&shared, &req(peer(4001)));
    shared.set_receiver_latency(ended, peer(4000), 1_000_000);
    shared.set_receiver_latency(live, peer(4001), 2_000_000);
    assert!(shared.end_flow(ended));
    let group = SocketAddrV4::new(Ipv4Addr::new(239, 255, 0, 9), 4321);
    let multicast = {
        let mut c = shared.lock();
        let slot = (0..MAX_FLOWS).find(|&s| c.table.flow(s).is_none()).unwrap();
        c.table.add_multicast(slot, group, &[1], 32, 3).unwrap();
        shared.publish(&c);
        slot
    };
    shared.set_receiver_latency(multicast, group, 3_000_000);
    let view = shared.tx_age_view();
    for slot in [ended, multicast] {
        assert_eq!(
            (view[slot].receiver.as_str(), view[slot].receiver_latency_ns),
            ("", 0)
        );
    }
    assert_eq!(
        (view[live].receiver.as_str(), view[live].receiver_latency_ns),
        ("peer-device", 2_000_000)
    );
}

/// Whether the TX thread's count (`account`, with the clock its pass
/// builds) calls a packet `age_ns` past its stamp, of a flow of `fpp`-frame
/// packets to slot `slot`'s receiver at `dest`, over latency on `shared`
/// (the receiver's latency read as the TX thread reads it).
/// The pass's `Instant` lies ahead, so no real time adds to the age: the
/// packet's age is exactly `age_ns`.
fn counted_late(shared: &TxShared, slot: usize, dest: SocketAddrV4, fpp: u16, age_ns: u64) -> bool {
    let receiver_ns = shared.receiver_latency_ns(slot, dest);
    let rate = shared.cfg.sample_rate;
    let stamp = 1_000 * u64::from(rate);
    let clock = PassClock {
        now_ns: ns_of(stamp, rate) + age_ns,
        pass: Instant::now() + Duration::from_secs(3_600),
        rate,
        margin_ns: shared.cfg.tx_margin_ns,
        latency_ns: shared.advertised_latency_ns(),
        relayed: false,
        escaped_ns: None,
    };
    let stats = LaneStats::default();
    let sent = Sent {
        result: Ok(false),
        retries: 0,
        took: 0,
        refused: false,
    };
    let over = account(&stats, &clock, stamp, fpp, &sent, 100, receiver_ns);
    assert_eq!(stats.over_latency.load(Ordering::Relaxed), u64::from(over));
    assert_eq!(
        stats.age_max_ns.load(Ordering::Relaxed),
        age_ns,
        "exact age"
    );
    over
}

/// dasio 26.11: a lane is judged at `late_threshold(fpp, receiver)`, not at
/// its receiver's stated latency. The view's `late_threshold_ns` is that
/// edge: a packet at it is not counted in `over_latency`, one a ns past it
/// is; it follows the receiver's latency and the latency we advertise; a
/// lane with no flow, or an unknown receiver latency, reads `None`.
#[test]
fn the_view_s_late_threshold_is_the_edge_over_latency_counts_against() {
    let rig = device();
    let (slot, _) = rig.flow(vec![Some(1), Some(2)]);
    let dest = v4(&rig.rx);
    let fpp = FPP as u16;
    let other = (slot + 1) % MAX_FLOWS;
    // The receiver's latency not read yet: nothing is counted late.
    let view = rig.shared.tx_age_view();
    assert_eq!(view[slot].late_threshold_ns, None, "unknown receiver");
    assert_eq!(view[other].late_threshold_ns, None, "no flow");
    assert!(!counted_late(&rig.shared, slot, dest, fpp, 1_000 * MS));
    // Below our 500 us the receiver plays at ours, here raised to the
    // 32-frame packet's floor (633 us); above it at its own.
    for (receiver_ns, edge) in [(250_000, 633_334), (2_000_000, 2_000_000)] {
        rig.shared.set_receiver_latency(slot, dest, receiver_ns);
        let view = rig.shared.tx_age_view();
        assert_eq!(view[slot].receiver_latency_ns, receiver_ns);
        assert_eq!(view[slot].late_threshold_ns, Some(edge), "{receiver_ns}");
        assert!(
            !counted_late(&rig.shared, slot, dest, fpp, edge),
            "at the edge"
        );
        assert!(
            counted_late(&rig.shared, slot, dest, fpp, edge + 1),
            "past it"
        );
        assert_eq!(view[other].late_threshold_ns, None, "no flow");
    }
    // The flow ends: its lane sends nothing, no edge.
    assert!(rig.shared.end_flow(slot));
    assert_eq!(rig.shared.tx_age_view()[slot].late_threshold_ns, None);
}

/// The edge follows the latency we advertise: a receiver below it plays at
/// ours, and a device latency Dante Controller sets (the config gives none)
/// moves it.
#[test]
fn the_late_threshold_follows_our_advertised_latency() {
    let shared = shared_with(Vec::new());
    let (slot, _) = add(&shared, &req(peer(4000)));
    let fpp = FPP as u16;
    shared.set_receiver_latency(slot, peer(4000), 1);
    let ours = shared.advertised_latency_ns();
    assert_eq!(ours, MS, "the test device's 1 ms");
    assert_eq!(shared.tx_age_view()[slot].late_threshold_ns, Some(ours));
    assert!(!counted_late(&shared, slot, peer(4000), fpp, ours));
    assert!(counted_late(&shared, slot, peer(4000), fpp, ours + 1));
    assert_eq!(shared.cfg.tx_latency_ns, None);
    shared.set_device_latency(20 * MS);
    assert_eq!(shared.tx_age_view()[slot].late_threshold_ns, Some(20 * MS));
    assert!(!counted_late(&shared, slot, peer(4000), fpp, 20 * MS));
    assert!(counted_late(&shared, slot, peer(4000), fpp, 20 * MS + 1));
}
