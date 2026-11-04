use super::*;

fn clock() -> PassClock {
    PassClock {
        now_ns: 5_000_000_000,
        pass: Instant::now(),
        rate: 96_000,
        margin_ns: 250_000,
        latency_ns: 1_000_000,
    }
}

#[test]
fn a_pass_s_floor_is_the_flow_s_latency_floor() {
    let c = clock();
    // A short packet: the advertised latency.
    assert_eq!(c.floor(16), 1_000_000);
    // A packet longer than the latency allows: its own frames + margin.
    let long = flow_latency_ns(1_000_000, 256, 96_000, 250_000);
    assert!(long > 1_000_000, "{long}");
    assert_eq!(c.floor(256), long);
}

#[test]
fn a_pass_s_now_is_its_time_plus_the_time_since() {
    let c = clock();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let now = c.now();
    assert!(now >= c.now_ns + 2_000_000, "{now}");
    assert!(now < c.now_ns + 2_000_000_000, "{now}");
}

/// dstack #1 (FOH, 4.11.2026): a flow to DVS (fpp 60 at 96 kHz, floor
/// about 1 ms) logged packets aged 1.0 to 1.15 ms as late, while DVS plays
/// at 10 ms ("High"). A packet is late against the latency its receiver
/// plays at: its own, at least our advertised one and the packet's floor;
/// unknown, never.
#[test]
fn a_packet_is_late_only_against_its_receiver_s_own_latency() {
    let c = clock();
    let floor = c.floor(60);
    assert!((800_000..1_150_000).contains(&floor), "{floor}");
    // DVS at 10 ms: on time.
    assert_eq!(c.late_threshold(60, 10_000_000), Some(10_000_000));
    assert!(!over_latency(1_150_000, 10_000_000));
    // A receiver at 1 ms: late.
    let at_1ms = c.late_threshold(60, 1_000_000).unwrap();
    assert_eq!(at_1ms, floor.max(1_000_000));
    assert!(over_latency(1_150_000, at_1ms));
    // Unknown: never late.
    assert_eq!(c.late_threshold(60, 0), None);
    // A receiver below our advertised latency plays at ours (Dante's
    // rule), and below the packet's floor at the floor.
    assert_eq!(c.late_threshold(16, 250_000), Some(1_000_000));
    let long = c.floor(256);
    assert_eq!(c.late_threshold(256, 1), Some(long));
    assert_eq!(c.late_threshold(256, long + 1), Some(long + 1));
}

fn sent() -> Sent {
    Sent {
        result: Ok(false),
        retries: 0,
        took: 0,
    }
}

#[test]
fn an_old_packet_counts_over_latency_only_with_a_known_receiver_latency() {
    // A packet stamped 3 ms before the pass (96 samples a ms).
    let c = clock();
    let stamp = (c.now_ns - 3_000_000) / 1_000_000 * 96;
    let stats = LaneStats::default();
    account(&stats, &c, stamp, 16, &sent(), 100, 0);
    assert_eq!(stats.over_latency.load(Ordering::Relaxed), 0, "unknown");
    assert!(stats.age_max_ns.load(Ordering::Relaxed) >= 3_000_000);
    account(&stats, &c, stamp, 16, &sent(), 100, 10_000_000);
    assert_eq!(stats.over_latency.load(Ordering::Relaxed), 0, "at 10 ms");
    account(&stats, &c, stamp, 16, &sent(), 100, 1_000_000);
    assert_eq!(stats.over_latency.load(Ordering::Relaxed), 1, "at 1 ms");
}
