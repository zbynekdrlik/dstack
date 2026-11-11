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
    // Both counters unchanged in a minute: no other traffic.
    assert_eq!(w.look(240 * S, Some(7_002_000), 2_000_500), Some(0));
    // Ours above the NIC's (counters skewed): never below zero.
    assert_eq!(w.look(300 * S, Some(7_002_100), 9_000_500), Some(0));
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

#[test]
fn over_100_kb_a_second_of_other_traffic_is_too_much() {
    assert!(!too_much(0));
    assert!(!too_much(OTHER_BYTES_PER_S));
    assert!(too_much(OTHER_BYTES_PER_S + 1));
}

#[test]
fn the_nic_is_the_interface_holding_the_address() {
    let ifaces = vec![
        ("lo".to_string(), std::net::IpAddr::V4(Ipv4Addr::LOCALHOST)),
        (
            "eth0".to_string(),
            std::net::IpAddr::V4(Ipv4Addr::new(10, 0, 0, 10)),
        ),
    ];
    assert_eq!(
        name_of(Ipv4Addr::new(10, 0, 0, 10), &ifaces).as_deref(),
        Some("eth0")
    );
    assert_eq!(name_of(Ipv4Addr::LOCALHOST, &ifaces).as_deref(), Some("lo"));
    assert_eq!(name_of(Ipv4Addr::new(10, 0, 0, 11), &ifaces), None);
}

// dstack #203: our other devices on the PC are ours, not other traffic.

fn readings(of: &[(&str, u64)]) -> Readings {
    of.iter().map(|(n, b)| (n.to_string(), *b)).collect()
}

#[test]
fn our_other_devices_audio_is_taken_off_the_rest_and_named() {
    let mut w = TrafficWatch::new();
    let start = readings(&[("a", 1_000), ("b", 50_000)]);
    assert_eq!(w.look_with(0, Some(1_000_000), 500, &start), None);
    // In 60 s: the NIC 30 000 000 more; ours 6 000 000, a 12 000 000, b
    // 6 000 000: 6 000 000 left, 100 000 B/s.
    let end = readings(&[("a", 12_001_000), ("b", 6_050_000)]);
    let t = w
        .look_with(60 * S, Some(31_000_000), 6_000_500, &end)
        .expect("a minute on");
    assert_eq!(
        t,
        Traffic {
            other: 100_000,
            ours: 100_000,
            devices: vec![("a".to_string(), 200_000), ("b".to_string(), 100_000)],
        }
    );
    assert_eq!(t.our_devices(), 400_000);
    assert!(!too_much(t.other));
    // The same NIC bytes without the neighbours: theirs would be other.
    let mut alone = TrafficWatch::new();
    assert_eq!(alone.look(0, Some(1_000_000), 500), None);
    assert_eq!(
        alone.look(60 * S, Some(31_000_000), 6_000_500),
        Some(400_000)
    );
}

#[test]
fn a_neighbour_counts_only_when_read_at_both_ends_of_the_window() {
    let mut w = TrafficWatch::new();
    let first = readings(&[("gone", 10), ("reset", 9_000_000)]);
    assert_eq!(w.look_with(0, Some(0), 0, &first), None);
    // "new" appeared, "gone" went away, "reset" restarted (its counter went
    // back): all three stay in the rest for this window.
    let second = readings(&[("new", 3_000_000), ("reset", 600_000)]);
    let t = w.look_with(60 * S, Some(6_000_000), 0, &second).unwrap();
    assert_eq!(t.other, 100_000);
    assert!(t.devices.is_empty(), "{t:?}");
    // The next window has both ends of "new" and "reset".
    let third = readings(&[("new", 4_200_000), ("reset", 1_200_000)]);
    let t = w.look_with(120 * S, Some(12_000_000), 0, &third).unwrap();
    assert_eq!(
        t.devices,
        vec![("new".to_string(), 20_000), ("reset".to_string(), 10_000)]
    );
    assert_eq!(t.other, 70_000);
    // Neighbours above the NIC's bytes (counters read moments apart):
    // never below zero.
    let fourth = readings(&[("new", 9_000_000)]);
    let t = w.look_with(180 * S, Some(12_000_000), 0, &fourth).unwrap();
    assert_eq!(t.other, 0);
}

#[test]
fn neighbours_are_our_other_devices_on_our_address() {
    use std::collections::BTreeMap;
    use std::net::SocketAddrV4;
    // Documentation addresses (RFC 5737) only.
    let ours = Ipv4Addr::new(192, 0, 2, 10);
    let other = Ipv4Addr::new(192, 0, 2, 99);
    let device = |name: &str, ip: Ipv4Addr, ctl: Option<&str>| {
        let mut txt = BTreeMap::new();
        if let Some(v) = ctl {
            txt.insert(crate::announce::STATUS_PORT_KEY.to_string(), v.to_string());
        }
        crate::discover::Device {
            name: name.to_string(),
            arc: SocketAddrV4::new(ip, 4440),
            dbc: None,
            cmc: None,
            device_id: None,
            txt,
        }
    };
    let found = [
        device("zeta", ours, Some("5002")),
        // This device itself.
        device("me", ours, Some("5000")),
        // A foreign Dante device on this address (no status port).
        device("foreign", ours, None),
        // Ours on another address (another NIC or PC).
        device("elsewhere", other, Some("5003")),
        device("alpha", ours, Some("5001")),
        device("zeta", ours, Some("5002")),
    ];
    let peer = |n: &str, p: u16| (n.to_string(), p);
    assert_eq!(
        neighbours(&found, ours, Some(5000)),
        vec![peer("alpha", 5001), peer("zeta", 5002)]
    );
    assert_eq!(
        neighbours(&found, ours, None),
        vec![peer("alpha", 5001), peer("me", 5000), peer("zeta", 5002)]
    );
    assert_eq!(
        neighbours(&found, other, Some(5000)),
        vec![peer("elsewhere", 5003)]
    );
    assert!(neighbours(&[], ours, None).is_empty());
}

#[test]
fn deltas_keep_the_neighbours_read_twice_whose_counter_did_not_go_back() {
    let before = readings(&[("a", 10), ("b", 20), ("c", 30), ("gone", 1)]);
    let now = readings(&[("a", 10), ("b", 25), ("c", 29), ("new", 7)]);
    assert_eq!(
        deltas(&before, &now),
        vec![("a".to_string(), 0), ("b".to_string(), 5)]
    );
    assert!(deltas(&Readings::new(), &now).is_empty());
}

#[test]
fn a_look_is_due_first_and_then_once_a_minute() {
    let mut w = TrafficWatch::new();
    assert!(w.due(5 * S));
    // An unread NIC learns nothing, so the next look is still due.
    assert_eq!(w.look(5 * S, None, 0), None);
    assert!(w.due(5 * S));
    assert_eq!(w.look(5 * S, Some(0), 0), None);
    assert!(!w.due(5 * S));
    assert!(!w.due(5 * S + EVERY_NS - 1));
    assert!(w.due(5 * S + EVERY_NS));
}

#[test]
fn our_devices_part_names_this_device_then_each_neighbour() {
    let t = Traffic {
        other: 9,
        ours: 1_200,
        devices: vec![("a".to_string(), 300), ("b".to_string(), 0)],
    };
    assert_eq!(our_devices(&t), "this device 1200, a 300, b 0");
    assert_eq!(our_devices(&Traffic::default()), "this device 0");
}

#[test]
fn a_neighbour_s_status_gives_its_tx_bytes() {
    let status =
        r#"{"clock":{"ready":1},"tx":[{"flow":1,"packets":3}],"tx_bytes":123456789012,"rx":[]}"#;
    assert_eq!(tx_bytes_of(status), Some(123_456_789_012));
    assert_eq!(tx_bytes_of(r#"{"clock":{"ready":1},"tx":[]}"#), None);
    assert_eq!(tx_bytes_of(r#"{"tx_bytes":-1}"#), None);
    assert_eq!(tx_bytes_of(r#"{"tx_bytes":"1"}"#), None);
    assert_eq!(tx_bytes_of("not json"), None);
    assert_eq!(tx_bytes_of("[1]"), None);
    assert_eq!(reply_bytes(Ok(r#"{"tx_bytes":7}"#.to_string())), Ok(7));
    assert_eq!(
        reply_bytes(Ok("{}".to_string())),
        Err(NO_TX_BYTES.to_string())
    );
    assert_eq!(
        reply_bytes(Err(ProbeError::Empty)),
        Err("the answer is empty".to_string())
    );
}

#[test]
fn an_unreadable_neighbour_is_left_out_and_warned_about_once() {
    crate::test_log::undecided();
    let mut w = TrafficWatch::new();
    let logs = crate::test_log::capture_warnings(|| {
        for _ in 0..3 {
            assert_eq!(w.take("a", Err("no answer".to_string())), None);
        }
        assert_eq!(w.take("b", Err("slow".to_string())), None);
    });
    assert_eq!(logs.matches("could not be read").count(), 2, "{logs}");
    assert_eq!(logs.matches("no answer").count(), 1, "{logs}");
    // Read again, then failing again: told again.
    let logs = crate::test_log::capture_warnings(|| {
        assert_eq!(w.take("a", Ok(5)), Some(5));
        assert_eq!(w.take("a", Err("gone".to_string())), None);
        assert_eq!(w.take("b", Err("slow".to_string())), None);
    });
    assert_eq!(logs.matches("could not be read").count(), 1, "{logs}");
    assert!(logs.contains("gone"), "{logs}");
}

/// An endpoint on 127.0.0.1 answering one connection with `line`, or
/// holding it unanswered for 2 s when `line` is `None`.
fn neighbour(line: Option<&'static str>) -> u16 {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let mut req = [0u8; 7];
        let _ = s.read_exact(&mut req);
        match line {
            Some(line) => {
                let _ = s.write_all(format!("{line}\n").as_bytes());
            }
            None => std::thread::sleep(std::time::Duration::from_secs(2)),
        }
    });
    port
}

/// A port on 127.0.0.1 nothing listens on.
fn closed_port() -> u16 {
    let l = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    l.local_addr().unwrap().port()
}

#[test]
fn neighbours_are_read_from_their_local_endpoints_within_one_wait() {
    let peers = vec![
        (
            "a".to_string(),
            neighbour(Some(r#"{"tx":[],"tx_bytes":4242}"#)),
        ),
        ("old".to_string(), neighbour(Some(r#"{"tx":[]}"#))),
        ("closed".to_string(), closed_port()),
    ];
    let got = read_peers(&peers, std::time::Duration::from_secs(5));
    assert_eq!(got[0], ("a".to_string(), Ok(4242)));
    assert_eq!(got[1], ("old".to_string(), Err(NO_TX_BYTES.to_string())));
    assert_eq!(got[2].0, "closed");
    let reason = got[2].1.as_ref().unwrap_err();
    assert!(reason.starts_with("no answer"), "{got:?}");
}

#[test]
fn a_stuck_neighbour_spends_the_wait_and_the_rest_are_not_read() {
    let peers = vec![
        ("stuck".to_string(), neighbour(None)),
        ("a".to_string(), neighbour(Some(r#"{"tx_bytes":1}"#))),
    ];
    let wait = std::time::Duration::from_millis(300);
    let start = std::time::Instant::now();
    let got = read_peers(&peers, wait);
    let took = start.elapsed();
    assert!(took >= wait, "{took:?}");
    assert!(took < std::time::Duration::from_millis(1500), "{took:?}");
    assert!(got[0].1.is_err(), "{got:?}");
    let reason = got[1].1.as_ref().unwrap_err();
    assert!(reason.starts_with("no answer"), "{got:?}");
    // Nothing to read: nothing waited.
    assert!(read_peers(&[], wait).is_empty());
}
