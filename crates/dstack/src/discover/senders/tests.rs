// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #289: the per-source mDNS counts, the minute's lines and the
//! meter's wiring.

use super::*;

const S: u64 = 1_000_000_000;

fn a(last: u8) -> Ipv4Addr {
    Ipv4Addr::new(192, 0, 2, last)
}

fn counted(senders: &mut Senders, source: Ipv4Addr, n: u64) {
    for _ in 0..n {
        senders.count(source);
    }
}

#[test]
fn a_minute_closes_at_sixty_seconds_with_the_loudest_five_first() {
    let mut s = Senders::new(5 * S);
    for (i, n) in [3, 9, 1, 9, 4, 7, 2].into_iter().enumerate() {
        counted(&mut s, a(10 + i as u8), n);
    }
    assert_eq!(s.roll(5 * S + MINUTE_NS - 1), None);
    let m = s.roll(5 * S + MINUTE_NS).expect("closed at 60 s");
    assert_eq!(m.ns, MINUTE_NS);
    assert_eq!(m.total, 35);
    let top: Vec<(Ipv4Addr, u64)> = m.top.iter().map(|r| (r.source, r.packets)).collect();
    // Equal counts by address.
    assert_eq!(
        top,
        vec![(a(11), 9), (a(13), 9), (a(15), 7), (a(14), 4), (a(10), 3)]
    );
    assert!(m.over_cap.is_empty());
    // The next minute starts empty at the roll.
    assert_eq!(s.sources(), 0);
    assert_eq!(s.roll(5 * S + 2 * MINUTE_NS - 1), None);
    let next = s.roll(5 * S + 2 * MINUTE_NS).expect("the next minute");
    assert_eq!((next.total, next.top.len()), (0, 0));
}

#[test]
fn a_source_over_fifty_a_second_over_the_minute_is_over_the_cap() {
    let mut s = Senders::new(0);
    counted(&mut s, a(1), 3000); // 50/s exactly: not over
    counted(&mut s, a(2), 3001);
    counted(&mut s, a(3), 6000);
    let m = s.roll(MINUTE_NS).unwrap();
    assert_eq!(
        m.over_cap,
        vec![
            Rate {
                source: a(3),
                packets: 6000
            },
            Rate {
                source: a(2),
                packets: 3001
            }
        ]
    );
    // A longer minute (a late roll) averages over its own length.
    let mut s = Senders::new(0);
    counted(&mut s, a(2), 3001);
    assert!(s.roll(2 * MINUTE_NS).unwrap().over_cap.is_empty());
}

#[test]
fn a_minute_is_loud_over_ten_a_second_in_all() {
    let minute = |total| Minute {
        ns: MINUTE_NS,
        total,
        top: vec![],
        over_cap: vec![],
    };
    assert!(!minute(600).loud());
    assert!(minute(601).loud());
    assert_eq!(minute(600).per_s(600), 10.0);
    let empty = Minute { ns: 0, ..minute(5) };
    assert_eq!(empty.per_s(5), 0.0);
    assert!(!Minute { ns: 0, ..minute(0) }.loud());
}

#[test]
fn past_the_most_sources_a_new_one_takes_the_quietest_place() {
    let ten = |i: u32| Ipv4Addr::from(0x0a00_0000 + i);
    let mut s = Senders::new(0);
    for i in 0..MAX_SOURCES as u32 {
        counted(&mut s, ten(i), if i < 2 { 1 } else { 2 });
    }
    assert_eq!(s.sources(), MAX_SOURCES);
    // Two with 1: the higher address leaves first.
    counted(&mut s, a(200), 500);
    assert_eq!(s.sources(), MAX_SOURCES);
    assert!(s.counts.contains_key(&ten(0)));
    assert!(!s.counts.contains_key(&ten(1)));
    assert_eq!(s.counts[&a(200)], 500);
    // Then the other one with 1, never a louder one.
    counted(&mut s, a(201), 1);
    assert_eq!(s.sources(), MAX_SOURCES);
    assert!(!s.counts.contains_key(&ten(0)));
    assert!(s.counts.contains_key(&ten(2)));
    assert_eq!(s.counts[&a(201)], 1);
    let m = s.roll(MINUTE_NS).unwrap();
    // Every datagram stays in the total.
    assert_eq!(m.total, 2 + 2 * (MAX_SOURCES as u64 - 2) + 501);
    assert_eq!(
        m.top[0],
        Rate {
            source: a(200),
            packets: 500
        }
    );
    assert!(m.over_cap.is_empty()); // 500 in a minute: 8.3/s
}

#[test]
fn a_source_under_the_most_is_never_dropped() {
    let mut s = Senders::new(0);
    for i in 0..MAX_SOURCES - 1 {
        counted(&mut s, Ipv4Addr::from(0x0a00_0000 + i as u32), 1);
    }
    counted(&mut s, a(9), 1);
    assert_eq!(s.sources(), MAX_SOURCES);
    let m = s.roll(MINUTE_NS).unwrap();
    assert_eq!(m.total, MAX_SOURCES as u64);
}

#[test]
fn a_clock_that_went_back_starts_a_new_minute() {
    let mut s = Senders::new(10 * S);
    counted(&mut s, a(1), 4);
    // The same time keeps the counts.
    assert_eq!(s.roll(10 * S), None);
    assert_eq!(s.sources(), 1);
    assert_eq!(s.roll(10 * S - 1), None);
    assert_eq!(s.sources(), 0);
    counted(&mut s, a(2), 1);
    let m = s
        .roll(10 * S - 1 + MINUTE_NS)
        .expect("a minute from the step back");
    assert_eq!(m.total, 1);
}

#[test]
fn a_loud_minute_logs_its_top_line_and_a_flood_source_a_warning() {
    crate::test_log::undecided();
    // A synthetic flood: one source at 120/s, three quiet ones.
    let mut s = Senders::new(0);
    counted(&mut s, a(66), 7200);
    counted(&mut s, a(1), 60);
    counted(&mut s, a(2), 30);
    counted(&mut s, a(3), 6);
    let m = s.roll(MINUTE_NS).unwrap();
    let text = crate::test_log::capture_at(tracing::Level::INFO, || report(&m));
    assert!(text.contains("mDNS senders in the last minute"), "{text}");
    assert!(
        text.contains("top=192.0.2.66 120.0/s, 192.0.2.1 1.0/s, 192.0.2.2 0.5/s, 192.0.2.3 0.1/s"),
        "{text}"
    );
    assert!(text.contains("per_s=121.6"), "{text}");
    let warns = crate::test_log::capture_warnings(|| report(&m));
    assert_eq!(
        warns.matches("an mDNS sender is over the cap").count(),
        1,
        "{warns}"
    );
    assert!(warns.contains("source=192.0.2.66"), "{warns}");
    assert!(warns.contains("per_s=120.0"), "{warns}");
    assert!(warns.contains("cap_per_s=50"), "{warns}");
    assert!(
        !warns.contains("mDNS senders in the last minute"),
        "{warns}"
    );
}

#[test]
fn a_quiet_minute_logs_nothing() {
    crate::test_log::undecided();
    let mut s = Senders::new(0);
    counted(&mut s, a(1), 600);
    let m = s.roll(MINUTE_NS).unwrap();
    let text = crate::test_log::capture_at(tracing::Level::DEBUG, || report(&m));
    assert_eq!(text, "");
}

fn loopback() -> UdpSocket {
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    sock
}

/// The wiring: a meter on a loopback socket names a flooding source in
/// its last minute (minutes of 300 ms here).
#[test]
fn the_meter_names_a_flooding_source_in_its_last_minute() {
    let sock = loopback();
    let to = sock.local_addr().unwrap();
    let meter = Meter::start(sock, None, Ipv4Addr::LOCALHOST, 300_000_000).unwrap();
    assert_eq!(meter.last(), None);
    let flood = UdpSocket::bind("127.0.0.2:0").unwrap();
    let quiet = UdpSocket::bind("127.0.0.3:0").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let named = loop {
        for _ in 0..20 {
            flood.send_to(b"x", to).unwrap();
        }
        quiet.send_to(b"x", to).unwrap();
        std::thread::sleep(Duration::from_millis(10));
        if let Some(m) = meter.last()
            && m.top.len() == 2
        {
            break m;
        }
        assert!(Instant::now() < deadline, "no minute with both sources");
    };
    assert_eq!(named.top[0].source, Ipv4Addr::new(127, 0, 0, 2));
    assert_eq!(named.top[1].source, Ipv4Addr::new(127, 0, 0, 3));
    assert_eq!(named.over_cap[0].source, Ipv4Addr::new(127, 0, 0, 2));
    assert!(named.ns >= 300_000_000);
    // Its drop ends the thread, which closes the socket: the port is free.
    drop(meter);
    UdpSocket::bind(to).expect("the meter's socket closed with its thread");
}

#[test]
fn one_meter_per_address_in_a_process_ending_with_its_last_holder() {
    // An address of this test's own: the registry is the process's.
    let ip = Ipv4Addr::new(192, 0, 2, 189);
    let mut opened = 0;
    let first = Meter::shared_with(ip, || {
        opened += 1;
        Ok(Meter::holding(None))
    })
    .unwrap();
    let second = Meter::shared_with(ip, || {
        opened += 1;
        Ok(Meter::holding(None))
    })
    .unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(opened, 1);
    let other = Meter::shared_with(Ipv4Addr::new(192, 0, 2, 190), || {
        opened += 1;
        Ok(Meter::holding(None))
    })
    .unwrap();
    assert!(!Arc::ptr_eq(&first, &other));
    assert_eq!(opened, 2);
    drop((first, second));
    let again = Meter::shared_with(ip, || {
        opened += 1;
        Ok(Meter::holding(None))
    })
    .unwrap();
    assert_eq!(opened, 3);
    // The new one is found again (the ended one's entry is not).
    let found = Meter::shared_with(ip, || {
        opened += 1;
        Ok(Meter::holding(None))
    })
    .unwrap();
    assert!(Arc::ptr_eq(&again, &found));
    assert_eq!(opened, 3);
    drop((again, found, other));
    // An open that fails is the caller's error, and nothing is kept.
    let refused = Meter::shared_with(ip, || Err(io::Error::other("no socket")));
    assert!(refused.is_err());
    let after = Meter::shared_with(ip, || {
        opened += 1;
        Ok(Meter::holding(None))
    })
    .unwrap();
    assert_eq!(opened, 4);
    drop(after);
}
