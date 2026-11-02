use super::*;

const S: u64 = 1_000_000_000;

#[test]
fn the_other_traffic_is_the_nic_s_bytes_less_ours_once_a_minute() {
    let mut w = TrafficWatch::new();
    // The first look only learns.
    assert_eq!(w.look(0, Some(1_000), 500), None);
    // Within the minute: nothing.
    assert_eq!(w.look(59 * S, Some(9_000_000), 500), None);
    // A minute on: the NIC sent 6 001 000 more, ours 1 000 000: 5 001 000 in
    // 60 s is 83 350 B/s.
    assert_eq!(w.look(60 * S, Some(6_002_000), 1_000_500), Some(83_350));
    // Ours alone: nothing other.
    assert_eq!(w.look(120 * S, Some(7_002_000), 2_000_500), Some(0));
    // An unread NIC: nothing, and the window holds.
    assert_eq!(w.look(180 * S, None, 3_000_500), None);
    // Ours above the NIC's (counters skewed): never below zero.
    assert_eq!(w.look(180 * S, Some(7_002_100), 9_000_500), Some(0));
}

#[test]
fn a_counter_that_went_back_starts_the_window_again() {
    let mut w = TrafficWatch::new();
    assert_eq!(w.look(0, Some(10_000_000), 0), None);
    assert_eq!(w.look(60 * S, Some(5), 0), None, "the NIC was reset");
    assert_eq!(w.look(120 * S, Some(6_000_005), 0), Some(100_000));
}

/// The routed interface's own counter is read and moves when a datagram
/// leaves by it (Linux and Windows).
#[cfg(any(target_os = "linux", windows))]
#[test]
fn the_routed_nic_s_sent_bytes_are_read_and_grow() {
    let probe = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    probe.connect("192.0.2.1:9").unwrap();
    let std::net::SocketAddr::V4(local) = probe.local_addr().unwrap() else {
        panic!("no routed IPv4 address");
    };
    let before = nic_sent_bytes(*local.ip()).expect("the routed NIC's counter");
    for _ in 0..50 {
        let _ = probe.send(&[0u8; 1000]);
    }
    let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut after = before;
    while after < before + 10_000 && std::time::Instant::now() < end {
        std::thread::sleep(std::time::Duration::from_millis(50));
        after = nic_sent_bytes(*local.ip()).unwrap();
    }
    assert!(after >= before + 10_000, "{before} -> {after}");
}
