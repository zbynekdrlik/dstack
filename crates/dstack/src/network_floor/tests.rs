// SPDX-License-Identifier: MIT OR Apache-2.0

//! The network floor (dstack #321): the comparison, the lists and their
//! hour, the once-an-hour warning, the versions an mDNS answer states.

use super::*;

use std::net::SocketAddrV4;

use simple_dns::rdata::TXT;
use simple_dns::{CLASS, Name, ResourceRecord};

const OLD: Version = Version::new(0, 6, 20);
const NEW: Version = Version::new(0, 6, 144);
const AT: Option<Ipv4Addr> = Some(Ipv4Addr::new(192, 0, 2, 7));

#[test]
fn the_floor_starts_at_the_first_build_with_no_standing_browse() {
    assert_eq!(NETWORK_FLOOR, Version::new(0, 6, 58));
    assert_eq!(crate::NETWORK_FLOOR.to_string(), "0.6.58");
}

#[test]
fn a_version_below_the_floor_is_below_and_the_floor_itself_is_not() {
    assert!(Version::new(0, 6, 57).below_floor());
    assert!(!Version::new(0, 6, 58).below_floor());
    assert!(!Version::new(0, 6, 59).below_floor());
    assert!(Version::new(0, 5, 999).below_floor());
    assert!(!Version::new(0, 7, 0).below_floor());
    assert!(!Version::new(1, 0, 0).below_floor());
    assert!(OLD.below_floor());
    assert!(!NEW.below_floor());
}

#[test]
fn a_version_reads_as_three_numbers_with_any_suffix_ignored() {
    assert_eq!(Version::parse("0.6.58"), Some(Version::new(0, 6, 58)));
    assert_eq!(Version::parse("1.20.300"), Some(Version::new(1, 20, 300)));
    assert_eq!(Version::parse("0.6.144-dev"), Some(NEW));
    assert_eq!(Version::parse("0.6.144+abc"), Some(NEW));
    for bad in [
        "",
        "0.6",
        "0.6.",
        "0.6.58.1",
        "a.6.58",
        "0.6.5x",
        "0..58",
        "+1.0.0",
        "0.6.-1",
        "70000.0.0",
    ] {
        assert_eq!(Version::parse(bad), None, "{bad:?}");
    }
    assert_eq!(Version::new(0, 6, 20).to_string(), "0.6.20");
}

#[test]
fn a_binary_below_the_floor_is_listed_and_one_upgraded_is_taken_off() {
    let mut g = Guard::default();
    assert_eq!(g.see(Kind::Peer, "Alpha", AT, NEW, 1), None);
    assert!(g.list(1).is_empty());
    let b = g.see(Kind::Peer, "Alpha", AT, OLD, 5).expect("below");
    assert_eq!(
        b,
        Below {
            kind: Kind::Peer,
            name: "Alpha".to_string(),
            address: AT,
            version: OLD,
            seen_ns: 5,
        }
    );
    assert_eq!(g.list(6), vec![b]);
    // The same peer, its name in another case: one entry, renewed.
    g.see(Kind::Peer, "alpha", AT, OLD, 9);
    assert_eq!(g.list(10).len(), 1);
    assert_eq!(g.list(10)[0].seen_ns, 9);
    // Upgraded: gone at once.
    assert_eq!(g.see(Kind::Peer, "Alpha", AT, NEW, 11), None);
    assert!(g.list(12).is_empty());
}

#[test]
fn a_binary_below_the_floor_stays_listed_for_an_hour_after_it_was_seen() {
    let mut g = Guard::default();
    g.see(Kind::Peer, "Alpha", AT, OLD, 1_000);
    assert_eq!(g.list(1_000 + HOUR_NS - 1).len(), 1);
    assert!(g.list(1_000 + HOUR_NS).is_empty());
}

#[test]
fn past_the_most_a_new_binary_takes_the_place_of_the_one_seen_longest_ago() {
    let mut g = Guard::default();
    for i in 0..MAX_BELOW {
        let t = 100 + u64::try_from(i).unwrap();
        g.see(Kind::Peer, &format!("d{i}"), AT, OLD, t);
    }
    // The one seen longest ago is seen again: no longer the oldest.
    g.see(Kind::Peer, "d0", AT, OLD, 1_000);
    g.see(Kind::Peer, "late", AT, OLD, 1_001);
    let names: Vec<String> = g.list(1_002).into_iter().map(|b| b.name).collect();
    assert_eq!(names.len(), MAX_BELOW);
    assert!(names.contains(&"late".to_string()));
    assert!(names.contains(&"d0".to_string()));
    assert!(!names.contains(&"d1".to_string()));
    // A listed one seen again takes no other's place.
    g.see(Kind::Peer, "late", AT, OLD, 1_003);
    assert_eq!(g.list(1_004).len(), MAX_BELOW);
    assert!(g.list(1_004).iter().any(|b| b.name == "d2"));
}

#[test]
fn a_peer_and_a_client_of_one_name_are_two_and_so_are_two_addresses() {
    let mut g = Guard::default();
    g.see(Kind::Peer, "x", AT, OLD, 1);
    g.see(Kind::Peer, "x", Some(Ipv4Addr::new(192, 0, 2, 8)), OLD, 1);
    g.see(Kind::Client, "x", None, OLD, 1);
    assert_eq!(g.list(2).len(), 3);
}

#[test]
fn clients_are_listed_by_name_for_an_hour_and_bounded() {
    let mut c = Clients::default();
    c.see("dtray", NEW, 10);
    c.see("DTRAY", OLD, 20);
    assert_eq!(
        c.list(30),
        vec![Client {
            name: "DTRAY".to_string(),
            version: OLD,
            seen_ns: 20,
        }]
    );
    assert_eq!(c.list(20 + HOUR_NS - 1).len(), 1);
    assert!(c.list(20 + HOUR_NS).is_empty());
    for i in 0..MAX_CLIENTS {
        c.see(&format!("c{i}"), NEW, 100 + u64::try_from(i).unwrap());
    }
    c.see("one-more", NEW, 1_000);
    let names: Vec<String> = c.list(1_001).into_iter().map(|c| c.name).collect();
    assert_eq!(names.len(), MAX_CLIENTS);
    assert!(names.contains(&"one-more".to_string()));
    assert!(!names.contains(&"c0".to_string()));
}

/// dstack #323: what the ARC record names is the lowest client version
/// below the floor among the clients of the last hour; none without one.
#[test]
fn the_lowest_client_below_the_floor_is_named_for_an_hour() {
    let mut c = Clients::default();
    assert_eq!(c.lowest_below_floor(0), None);
    c.see("dtray-cli", NEW, 5);
    c.see("at-floor", NETWORK_FLOOR, 5);
    assert_eq!(c.lowest_below_floor(6), None, "none below the floor");
    c.see("dtray", OLD, 10);
    let later = Version::new(0, 6, 40);
    c.see("other", later, 20);
    assert_eq!(c.lowest_below_floor(21), Some(OLD));
    // The hour runs from each client's last request.
    assert_eq!(c.lowest_below_floor(10 + HOUR_NS - 1), Some(OLD));
    assert_eq!(c.lowest_below_floor(10 + HOUR_NS), Some(later));
    assert_eq!(c.lowest_below_floor(20 + HOUR_NS), None);
    assert!(c.list(20 + HOUR_NS).is_empty(), "expired ones are dropped");
    // Upgraded under its name: gone at once.
    c.see("dtray", OLD, 30 + HOUR_NS);
    assert_eq!(c.lowest_below_floor(31 + HOUR_NS), Some(OLD));
    c.see("DTRAY", NEW, 32 + HOUR_NS);
    assert_eq!(c.lowest_below_floor(33 + HOUR_NS), None);
}

fn below(name: &str) -> Below {
    Below {
        kind: Kind::Peer,
        name: name.to_string(),
        address: AT,
        version: OLD,
        seen_ns: 0,
    }
}

#[test]
fn a_binary_is_warned_about_once_an_hour() {
    let mut w = Warned::default();
    let b = below("Alpha");
    assert!(w.due(&b, 1_000));
    assert!(!w.due(&b, 1_001));
    assert!(!w.due(&b, 1_000 + HOUR_NS - 1));
    assert!(w.due(&b, 1_000 + HOUR_NS));
    assert!(!w.due(&b, 1_000 + HOUR_NS + 1));
    // Another binary, or the same name in another kind, is its own.
    assert!(w.due(&below("Beta"), 1_002));
    let mut client = below("Alpha");
    client.kind = Kind::Client;
    client.address = None;
    assert!(w.due(&client, 1_002));
}

#[test]
fn a_full_warned_list_drops_the_hour_old_and_otherwise_warns_no_newcomer() {
    let mut w = Warned::default();
    for i in 0..MAX_WARNED {
        assert!(w.due(&below(&format!("d{i}")), 10));
    }
    // Full, all within the hour: a newcomer waits, a known one is as before.
    assert!(!w.due(&below("new"), 20));
    assert!(!w.due(&below("d0"), 20));
    // An hour on, the old ones are dropped and the newcomer is warned.
    assert!(w.due(&below("new"), 10 + HOUR_NS));
    assert!(!w.due(&below("new"), 11 + HOUR_NS));
    // One short of full takes a newcomer without dropping anything.
    let mut w = Warned::default();
    for i in 0..MAX_WARNED - 1 {
        w.due(&below(&format!("d{i}")), 10);
    }
    assert!(w.due(&below("new"), 20));
    assert!(!w.due(&below("d0"), 21));
}

#[test]
fn the_check_warns_once_and_lists_an_old_peer_and_ignores_a_current_one() {
    let guard = Mutex::new(Guard::default());
    let text = crate::test_log::capture_warnings(|| {
        for _ in 0..3 {
            check(&guard, Kind::Peer, "floor-test-old", AT, "0.6.20");
        }
        check(&guard, Kind::Peer, "floor-test-new", AT, "0.6.144");
        check(&guard, Kind::Peer, "floor-test-junk", AT, "new");
    });
    assert_eq!(
        text.matches("a dstack device below the network floor")
            .count(),
        1,
        "{text}"
    );
    assert!(text.contains("floor-test-old"), "{text}");
    assert!(text.contains("kind=\"peer\""), "{text}");
    assert!(text.contains("version=0.6.20"), "{text}");
    assert!(text.contains("floor=0.6.58"), "{text}");
    assert!(text.contains("address=192.0.2.7"), "{text}");
    let listed = lock(&guard).list(now_ns());
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "floor-test-old");
}

#[test]
fn a_client_below_the_floor_is_warned_about_as_local() {
    let b = Below {
        kind: Kind::Client,
        name: "floor-test-client".to_string(),
        address: None,
        version: OLD,
        seen_ns: 0,
    };
    let text = crate::test_log::capture_warnings(|| {
        for _ in 0..3 {
            warn_below(&b);
        }
    });
    assert_eq!(
        text.matches("a dstack device below the network floor")
            .count(),
        1,
        "{text}"
    );
    assert!(text.contains("kind=\"client\""), "{text}");
    assert!(text.contains("address=local"), "{text}");
}

/// A peer's name comes from any responder on the leg: a line break in it
/// is logged escaped, so it cannot forge a line of its own.
#[test]
fn a_peer_s_name_is_logged_escaped() {
    let b = Below {
        kind: Kind::Peer,
        name: "floor-test-inj\nWARN forged".to_string(),
        address: AT,
        version: OLD,
        seen_ns: 0,
    };
    let text = crate::test_log::capture_warnings(|| warn_below(&b));
    assert!(text.contains("floor-test-inj\\nWARN forged"), "{text}");
    assert_eq!(text.lines().count(), 1, "{text}");
}

fn device(name: &str, txt: &[(&str, &str)]) -> Device {
    Device {
        name: name.to_string(),
        arc: SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 9), 4440),
        dbc: None,
        cmc: None,
        device_id: None,
        txt: txt
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
    }
}

#[test]
fn of_the_listed_devices_only_ours_below_the_floor_are_named() {
    let devices = [
        device("old", &[("dstack_vers", "0.6.20")]),
        device("new", &[("dstack_vers", "0.6.144")]),
        device("audinate", &[("mf", "Audinate")]),
        device("junk", &[("dstack_vers", "x")]),
    ];
    assert_eq!(
        below_of(&devices, 42),
        vec![Below {
            kind: Kind::Peer,
            name: "old".to_string(),
            address: Some(Ipv4Addr::new(192, 0, 2, 9)),
            version: OLD,
            seen_ns: 42,
        }]
    );
    let text = crate::test_log::capture_warnings(|| {
        for _ in 0..3 {
            warn_devices(&[device("floor-test-listed", &[("dstack_vers", "0.6.20")])]);
        }
        warn_devices(&devices[1..]);
    });
    assert_eq!(
        text.matches("a dstack device below the network floor")
            .count(),
        1,
        "{text}"
    );
    assert!(text.contains("floor-test-listed"), "{text}");
}

/// dstack #323: a device's announced old local client is named as a client
/// by the device, at its ARC address; a value that does not read or is not
/// below the floor names nothing.
#[test]
fn a_device_s_announced_old_client_is_named_as_a_client_at_its_address() {
    let at = Some(Ipv4Addr::new(192, 0, 2, 9));
    let devices = [
        device(
            "beside-dtray",
            &[("dstack_vers", "0.6.146"), ("dstack_old_client", "0.6.20")],
        ),
        device(
            "both-old",
            &[("dstack_vers", "0.6.30"), ("dstack_old_client", "0.6.20")],
        ),
        device("at-floor", &[("dstack_old_client", "0.6.58")]),
        device("junk", &[("dstack_old_client", "x")]),
        device("clean", &[("dstack_vers", "0.6.146")]),
    ];
    let below = |kind, name: &str, version| Below {
        kind,
        name: name.to_string(),
        address: at,
        version,
        seen_ns: 7,
    };
    assert_eq!(
        below_of(&devices, 7),
        vec![
            below(Kind::Client, "beside-dtray", OLD),
            below(Kind::Peer, "both-old", Version::new(0, 6, 30)),
            below(Kind::Client, "both-old", OLD),
        ]
    );
}

/// An mDNS datagram of `records`, a response or a query.
fn datagram(response: bool, records: Vec<ResourceRecord<'static>>) -> Vec<u8> {
    let mut p = if response {
        Packet::new_reply(0)
    } else {
        Packet::new_query(0)
    };
    p.answers = records;
    p.build_bytes_vec().unwrap()
}

fn txt(name: &'static str, strings: &[&'static str]) -> ResourceRecord<'static> {
    let mut t = TXT::new();
    for s in strings {
        t.add_string(s).unwrap();
    }
    ResourceRecord::new(Name::new_unchecked(name), CLASS::IN, 120, RData::TXT(t))
}

#[test]
fn an_answer_states_the_version_of_each_dstack_arc_record() {
    let answer = datagram(
        true,
        vec![
            txt(
                "Alpha._netaudio-arc._udp.local",
                &["mf=x", "dstack_vers=0.6.20"],
            ),
            txt("Card._netaudio-arc._udp.local", &["mf=Audinate"]),
            txt("Alpha._netaudio-cmc._udp.local", &["dstack_vers=0.6.1"]),
            txt("Beta._netaudio-arc._udp.local", &["dstack_vers=0.6.144"]),
        ],
    );
    assert_eq!(
        arc_versions(&answer),
        vec![
            ("Alpha".to_string(), "0.6.20".to_string()),
            ("Beta".to_string(), "0.6.144".to_string()),
        ]
    );
    // The additional records count too.
    let mut p = Packet::new_reply(0);
    p.additional_records = vec![txt(
        "Alpha._netaudio-arc._udp.local",
        &["dstack_vers=0.6.20"],
    )];
    assert_eq!(arc_versions(&p.build_bytes_vec().unwrap()).len(), 1);
}

#[test]
fn a_query_s_known_answers_and_what_does_not_read_state_nothing() {
    let query = datagram(
        false,
        vec![txt(
            "Alpha._netaudio-arc._udp.local",
            &["dstack_vers=0.6.20"],
        )],
    );
    assert!(arc_versions(&query).is_empty());
    assert!(arc_versions(b"no dns").is_empty());
    assert!(arc_versions(&[0x00, 0x00]).is_empty());
    // The QR bit alone (byte 2 exactly 0x80) is a response; one bit under
    // it is a query, whatever the rest says.
    let mut reply = datagram(
        true,
        vec![txt(
            "Alpha._netaudio-arc._udp.local",
            &["dstack_vers=0.6.20"],
        )],
    );
    reply[2] = 0x80;
    assert_eq!(arc_versions(&reply).len(), 1);
    reply[2] = 0x7f;
    assert!(arc_versions(&reply).is_empty());
    // A key with no value is no version.
    let bare = datagram(
        true,
        vec![txt("Alpha._netaudio-arc._udp.local", &["dstack_vers"])],
    );
    assert!(arc_versions(&bare).is_empty());
}

#[test]
fn a_client_has_a_short_plain_name_and_a_version() {
    assert_eq!(
        client_of("dtray", "0.6.144"),
        Some(("dtray".to_string(), NEW))
    );
    assert_eq!(
        client_of("dtray-cli_2.x", "0.6.20"),
        Some(("dtray-cli_2.x".to_string(), OLD))
    );
    let longest = "a".repeat(MAX_CLIENT_NAME);
    assert!(client_of(&longest, "0.6.144").is_some());
    assert_eq!(client_of(&"a".repeat(MAX_CLIENT_NAME + 1), "0.6.144"), None);
    assert_eq!(client_of("", "0.6.144"), None);
    assert_eq!(client_of("dt\"ray", "0.6.144"), None);
    assert_eq!(client_of("dtray", "latest"), None);
}

#[test]
fn the_clock_runs_on() {
    let a = now_ns();
    std::thread::sleep(std::time::Duration::from_millis(2));
    assert!(now_ns() >= a + 1_000_000);
}

#[test]
fn each_kind_has_its_word() {
    assert_eq!(Kind::Peer.word(), "peer");
    assert_eq!(Kind::Client.word(), "client");
}
