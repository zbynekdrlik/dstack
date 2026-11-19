// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #289: the per-source mDNS counts, the minute's lines and the
//! meter's wiring.

use super::*;
use crate::announce::mdns::{Heard, Taps};
use std::net::{SocketAddr, SocketAddrV4};

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
        browsers: vec![],
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

/// A meter fed by taps of its own, no socket: what the daemon's socket
/// hands it (dstack #442).
fn fed(minute_ns: u64, browsers: Browsers) -> (Arc<Taps>, Meter) {
    let taps = Arc::new(Taps::default());
    let meter = Meter::start(
        taps.listen(Heard::All),
        Ipv4Addr::LOCALHOST,
        minute_ns,
        browsers,
    )
    .unwrap();
    (taps, meter)
}

/// The source `127.0.0.<last>:<port>`.
fn from(last: u8, port: u16) -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, last), port))
}

/// The wiring: a meter names a flooding source in its last minute
/// (minutes of 300 ms here) and counts every datagram it was handed.
#[test]
fn the_meter_names_a_flooding_source_in_its_last_minute() {
    let (taps, meter) = fed(300_000_000, Browsers::default());
    assert_eq!(meter.last(), None);
    assert_eq!(meter.heard(), 0);
    let mut handed = 0;
    let deadline = Instant::now() + Duration::from_secs(10);
    let named = loop {
        for _ in 0..20 {
            taps.feed(b"x", from(2, 5353));
        }
        taps.feed(b"x", from(3, 5353));
        handed += 21;
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
    while meter.heard() < handed {
        assert!(Instant::now() < deadline, "{} of {handed}", meter.heard());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(meter.heard(), handed);
    assert_eq!(taps.dropped(), 0);
    // Its drop ends the thread, and its listener leaves the taps.
    assert_eq!(taps.listeners(), 1);
    drop(meter);
    assert_eq!(taps.listeners(), 0);
}

/// dstack #319: a host refreshing Dante records every 96 s gets one WARN
/// naming it while it is marked to warn; a listed host not marked, none.
#[test]
fn a_browsing_host_is_warned_about_by_name_when_marked() {
    crate::test_log::undecided();
    let mut m = Senders::new(0).roll(MINUTE_NS).unwrap();
    m.browsers = vec![
        Browser {
            source: a(7),
            period_ns: 96_200_000_000,
            warn: true,
        },
        Browser {
            source: a(9),
            period_ns: 96_000_000_000,
            warn: false,
        },
    ];
    let warns = crate::test_log::capture_warnings(|| report(&m));
    assert_eq!(
        warns
            .matches("an mDNS host refreshes Dante records every 96 s")
            .count(),
        1,
        "{warns}"
    );
    assert!(warns.contains("source=192.0.2.7"), "{warns}");
    assert!(warns.contains("period_s=96.2"), "{warns}");
    assert!(!warns.contains("192.0.2.9"), "{warns}");
    m.browsers[0].warn = false;
    let none = crate::test_log::capture_warnings(|| report(&m));
    assert_eq!(none, "");
}

/// A query with a question under `_netaudio-arc` (dstack #319).
fn arc_query() -> Vec<u8> {
    let mut q = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in ["_netaudio-arc", "_udp", "local"] {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.extend_from_slice(&[0, 0, 12, 0, 1]);
    q
}

/// The wiring (dstack #319): the meter names a host whose Dante queries
/// from port 5353 come on the cycle (here 250 ms ± 80 ms, minutes of
/// 200 ms), and not one sending the same from another port.
#[test]
fn the_meter_names_a_host_refreshing_dante_records_on_the_cycle() {
    let cycle = Browsers::with_cycle(250_000_000, 80_000_000, 20_000_000);
    let (taps, meter) = fed(200_000_000, cycle);
    let q = arc_query();
    let deadline = Instant::now() + Duration::from_secs(10);
    // Every minute seen that named a host (a later minute lists it again,
    // not warned: look between the sends).
    let mut seen: Vec<Minute> = Vec::new();
    while seen.is_empty() || !seen.iter().any(|m| m.browsers.iter().any(|b| b.warn)) {
        taps.feed(&q, from(4, 5353));
        taps.feed(&q, from(5, 40000));
        for _ in 0..25 {
            std::thread::sleep(Duration::from_millis(10));
            if let Some(m) = meter.last()
                && !m.browsers.is_empty()
                && seen.last() != Some(&m)
            {
                seen.push(m);
            }
        }
        assert!(Instant::now() < deadline, "no host named: {seen:?}");
    }
    for m in &seen {
        assert_eq!(m.browsers.len(), 1, "{:?}", m.browsers);
        assert_eq!(m.browsers[0].source, Ipv4Addr::new(127, 0, 0, 4));
        let p = m.browsers[0].period_ns;
        assert!((170_000_000..=330_000_000).contains(&p), "{p}");
    }
}

/// dstack #319, field 16.11.2026: a host whose own answers carry an ARC
/// record without `dstack_vers` (Dante hardware) is not named for the same
/// cycle; a host with no records beside it is.
#[test]
fn the_meter_names_no_dante_hardware_on_the_cycle() {
    use simple_dns::rdata::{RData, TXT};
    use simple_dns::{CLASS, Name, Packet, ResourceRecord};
    let cycle = Browsers::with_cycle(250_000_000, 80_000_000, 20_000_000);
    let (taps, meter) = fed(200_000_000, cycle);
    let (pc, hardware) = (from(4, 5353), from(6, 5353));
    let card_answer = {
        let mut p = Packet::new_reply(0);
        // The keys of the field case's ARC record (16.11.2026, an Audinate
        // module), values made up.
        let mut txt = TXT::new();
        for entry in [
            "arcp_vers=2.8.9",
            "arcp_min=0.2.4",
            "router_vers=4.0.0",
            "router_info=x",
            "mf=x",
            "model=x",
        ] {
            txt.add_string(entry).unwrap();
        }
        p.answers = vec![ResourceRecord::new(
            Name::new_unchecked("meter-card._netaudio-arc._udp.local"),
            CLASS::IN,
            120,
            RData::TXT(txt),
        )];
        p.build_bytes_vec().unwrap()
    };
    taps.feed(&card_answer, hardware);
    let q = arc_query();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen: Vec<Minute> = Vec::new();
    while seen.len() < 3 {
        taps.feed(&q, pc);
        taps.feed(&q, hardware);
        for _ in 0..25 {
            std::thread::sleep(Duration::from_millis(10));
            if let Some(m) = meter.last()
                && !m.browsers.is_empty()
                && seen.last() != Some(&m)
            {
                seen.push(m);
            }
        }
        assert!(Instant::now() < deadline, "no host named: {seen:?}");
    }
    for m in &seen {
        let named: Vec<Ipv4Addr> = m.browsers.iter().map(|b| b.source).collect();
        assert_eq!(named, vec![Ipv4Addr::new(127, 0, 0, 4)], "{m:?}");
    }
}

/// dstack #321: the meter reads the dstack version of each ARC record it
/// hears and keeps the peers below the network floor, with the address
/// the answer came from; a current one is not kept.
#[test]
fn the_meter_keeps_the_peers_it_hears_below_the_network_floor() {
    use simple_dns::rdata::{RData, TXT};
    use simple_dns::{CLASS, Name, Packet, ResourceRecord};
    let answer = |name: &'static str, vers: &'static str| {
        let mut p = Packet::new_reply(0);
        let txt = TXT::new().with_string(vers).unwrap();
        p.answers = vec![ResourceRecord::new(
            Name::new_unchecked(name),
            CLASS::IN,
            120,
            RData::TXT(txt),
        )];
        p.build_bytes_vec().unwrap()
    };
    let (taps, meter) = fed(MINUTE_NS, Browsers::default());
    assert!(meter.below_floor().is_empty());
    let old = answer("meter-old._netaudio-arc._udp.local", "dstack_vers=0.6.20");
    let new = answer("meter-new._netaudio-arc._udp.local", "dstack_vers=0.6.144");
    let deadline = Instant::now() + Duration::from_secs(10);
    let listed = loop {
        taps.feed(&new, from(4, 5353));
        taps.feed(&old, from(4, 5353));
        std::thread::sleep(Duration::from_millis(10));
        let listed = meter.below_floor();
        if !listed.is_empty() {
            break listed;
        }
        assert!(Instant::now() < deadline, "the old peer never listed");
    };
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].name, "meter-old");
    assert_eq!(listed[0].kind, Kind::Peer);
    assert_eq!(listed[0].address, Some(Ipv4Addr::new(127, 0, 0, 4)));
    assert_eq!(listed[0].version, network_floor::Version::new(0, 6, 20));
}
