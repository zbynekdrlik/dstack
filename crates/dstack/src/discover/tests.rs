use super::*;

use std::collections::HashMap;

use simple_dns::ResourceRecord;
use simple_dns::rdata::{PTR, SRV};

use crate::announce::ServiceRecord;

/// Every wait in these tests ends.
const BOUND: Duration = Duration::from_secs(5);
const LOOPBACK: Ipv4Addr = Ipv4Addr::LOCALHOST;

/// A record as the queries read it: `ip`:`port` with `txt`.
fn srv(ip: [u8; 4], port: u16, txt: &[(&str, &str)]) -> Srv {
    Srv {
        port,
        ips: vec![Ipv4Addr::from(ip)],
        txt: txt
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect::<HashMap<_, _>>(),
    }
}

fn at(ip: [u8; 4], port: u16) -> SocketAddrV4 {
    SocketAddrV4::new(Ipv4Addr::from(ip), port)
}

#[test]
fn a_device_is_listed_with_its_arc_record_name_address_and_txt() {
    let mut d = Devices::default();
    let mut two = srv([10, 0, 0, 12], 4440, &[("model", "X1"), ("mf", "Acme")]);
    two.ips.push(Ipv4Addr::new(10, 0, 0, 11));
    let Some(Change::Added(dev)) = d.set(Service::Arc, "Card-1", Some(&two)) else {
        panic!("listed");
    };
    assert_eq!(dev.name, "Card-1", "its case kept");
    assert_eq!(dev.arc, at([10, 0, 0, 11], 4440), "the lowest address");
    assert_eq!(dev.txt.get("model").map(String::as_str), Some("X1"));
    assert_eq!((dev.dbc, dev.device_id), (None, None));
    assert_eq!(d.list(), [dev]);
}

#[test]
fn a_dbc_record_alone_lists_nothing_and_joins_the_device_later() {
    let mut d = Devices::default();
    let dbc = srv([10, 0, 0, 11], 4455, &[]);
    assert_eq!(d.set(Service::Dbc, "card", Some(&dbc)), None);
    assert!(d.list().is_empty());
    let Some(Change::Added(dev)) =
        d.set(Service::Arc, "card", Some(&srv([10, 0, 0, 11], 4440, &[])))
    else {
        panic!("listed");
    };
    assert_eq!(dev.dbc, Some(at([10, 0, 0, 11], 4455)));
}

#[test]
fn a_changed_address_txt_or_flow_control_is_a_change_and_the_same_record_is_none() {
    let mut d = Devices::default();
    let arc = srv([10, 0, 0, 11], 4440, &[("model", "X1")]);
    d.set(Service::Arc, "card", Some(&arc));
    assert_eq!(d.set(Service::Arc, "card", Some(&arc)), None);
    let moved = srv([10, 0, 0, 12], 4440, &[("model", "X1")]);
    assert!(matches!(
        d.set(Service::Arc, "CARD", Some(&moved)),
        Some(Change::Changed(_))
    ));
    let txt = srv([10, 0, 0, 12], 4440, &[("model", "X2")]);
    assert!(matches!(
        d.set(Service::Arc, "card", Some(&txt)),
        Some(Change::Changed(_))
    ));
    let Some(Change::Changed(dev)) =
        d.set(Service::Dbc, "card", Some(&srv([10, 0, 0, 12], 4455, &[])))
    else {
        panic!("changed");
    };
    assert_eq!(dev.dbc, Some(at([10, 0, 0, 12], 4455)));
    // Flow control gone: a change, the device stays.
    let Some(Change::Changed(dev)) = d.set(Service::Dbc, "card", None) else {
        panic!("changed");
    };
    assert_eq!(dev.dbc, None);
    assert_eq!(d.set(Service::Dbc, "card", None), None);
}

#[test]
fn a_gone_arc_record_removes_the_device_and_an_unknown_one_is_none() {
    let mut d = Devices::default();
    assert_eq!(d.set(Service::Arc, "card", None), None);
    d.set(Service::Arc, "Card", Some(&srv([10, 0, 0, 11], 4440, &[])));
    assert_eq!(
        d.set(Service::Arc, "card", None),
        Some(Change::Removed("Card".to_string()))
    );
    assert!(d.list().is_empty() && d.entries.is_empty());
    // A record with no address is no record.
    let mut none = srv([10, 0, 0, 11], 4440, &[]);
    none.ips.clear();
    assert_eq!(d.set(Service::Arc, "card", Some(&none)), None);
    assert!(d.entries.is_empty());
}

/// dcontroller (1.11): a controller maps a device to its heartbeats by the
/// device id, which the CMC record's `id` gives (two of our devices share
/// an address on the site).
#[test]
fn the_cmc_record_gives_the_device_id_its_heartbeat_carries() {
    let mut d = Devices::default();
    let cmc = |id: &str| srv([10, 0, 0, 11], 8800, &[("id", id), ("process", "0")]);
    assert_eq!(
        d.set(Service::Cmc, "card", Some(&cmc("00000a4d07720001"))),
        None
    );
    let Some(Change::Added(dev)) =
        d.set(Service::Arc, "card", Some(&srv([10, 0, 0, 11], 4440, &[])))
    else {
        panic!("listed");
    };
    assert_eq!(dev.device_id, Some([0, 0, 10, 77, 7, 114, 0, 1]));
    // A hardware id, upper case: read the same.
    let Some(Change::Changed(dev)) = d.set(Service::Cmc, "CARD", Some(&cmc("001DC10821C40000")))
    else {
        panic!("changed");
    };
    assert_eq!(dev.device_id, Some([0, 0x1d, 0xc1, 0x08, 0x21, 0xc4, 0, 0]));
    // An id that does not read: none, the device still listed.
    let Some(Change::Changed(dev)) = d.set(Service::Cmc, "card", Some(&cmc("xyz"))) else {
        panic!("changed");
    };
    assert_eq!(dev.device_id, None);
    d.set(Service::Cmc, "card", Some(&cmc("c8a362fffe2e958a")));
    // The CMC record gone: the id goes, the device stays.
    let Some(Change::Changed(dev)) = d.set(Service::Cmc, "card", None) else {
        panic!("changed");
    };
    assert_eq!(dev.device_id, None);
    // Gone last, a CMC record alone keeps no entry.
    d.set(Service::Cmc, "solo", Some(&cmc("0000000000000001")));
    assert_eq!(d.set(Service::Cmc, "solo", None), None);
    assert_eq!(d.entries.len(), 1, "only card");
    // The ARC record goes while the CMC record stays: removed from the
    // list, kept until the CMC record goes too.
    d.set(Service::Cmc, "card", Some(&cmc("0000000000000002")));
    assert_eq!(
        d.set(Service::Arc, "card", None),
        Some(Change::Removed("card".to_string()))
    );
    assert_eq!(d.entries.len(), 1);
    d.set(Service::Cmc, "card", None);
    assert!(d.entries.is_empty());
}

/// dstack #180: a device beside another on one PC takes CMC 8800+k and
/// says so in its `_netaudio-cmc` record; a controller sends its meter
/// start there, not to 8800 (the PC's first device).
#[test]
fn the_cmc_record_gives_the_device_s_cmc_address_and_a_new_port_is_a_change() {
    let mut d = Devices::default();
    let cmc = |port: u16| srv([10, 0, 0, 30], port, &[("id", "00000a4d071e0001")]);
    d.set(Service::Arc, "band", Some(&srv([10, 0, 0, 30], 4441, &[])));
    let Some(Change::Changed(dev)) = d.set(Service::Cmc, "band", Some(&cmc(8801))) else {
        panic!("changed");
    };
    assert_eq!(dev.cmc, Some(at([10, 0, 0, 30], 8801)));
    let Some(Change::Changed(dev)) = d.set(Service::Cmc, "band", Some(&cmc(8802))) else {
        panic!("a new CMC port is a change");
    };
    assert_eq!(dev.cmc, Some(at([10, 0, 0, 30], 8802)));
    // The CMC record gone: no CMC address, the device stays.
    let Some(Change::Changed(dev)) = d.set(Service::Cmc, "band", None) else {
        panic!("changed");
    };
    assert_eq!(dev.cmc, None);
}

#[test]
fn a_device_id_is_16_hex_digits() {
    assert_eq!(
        parse_device_id("0123456789abcdef"),
        Some([0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef])
    );
    for bad in [
        "",
        "0123456789abcde",
        "0123456789abcdef0",
        "0123456789abcdeg",
        "01234567é9abcde",
    ] {
        assert_eq!(parse_device_id(bad), None, "{bad:?}");
    }
}

#[test]
fn the_list_is_bounded_and_txt_too() {
    let mut d = Devices::default();
    for i in 0..MAX_DEVICES {
        let name = format!("d{i}");
        assert!(
            d.set(Service::Arc, &name, Some(&srv([10, 0, 0, 11], 4440, &[])))
                .is_some()
        );
    }
    assert_eq!(
        d.set(
            Service::Arc,
            "one-more",
            Some(&srv([10, 0, 0, 11], 4440, &[]))
        ),
        None
    );
    // A kept device still changes.
    assert!(
        d.set(Service::Arc, "d0", Some(&srv([10, 0, 0, 12], 4440, &[])))
            .is_some()
    );
    let keys: Vec<String> = (0..MAX_TXT + 5).map(|i| format!("k{i:02}")).collect();
    let txt: Vec<(&str, &str)> = keys.iter().map(|k| (k.as_str(), "v")).collect();
    let Some(Change::Changed(dev)) =
        d.set(Service::Arc, "d1", Some(&srv([10, 0, 0, 11], 4440, &txt)))
    else {
        panic!("changed");
    };
    assert_eq!(dev.txt.len(), MAX_TXT);
    assert!(dev.txt.contains_key("k00"), "the first keys kept");
}

#[test]
fn a_record_name_keeps_its_case_and_needs_its_service_suffix() {
    assert_eq!(
        record_of("Card-1._netaudio-arc._udp.local."),
        Some((Service::Arc, "Card-1".to_string()))
    );
    assert_eq!(
        record_of("x._NETAUDIO-DBC._udp.local"),
        Some((Service::Dbc, "x".to_string()))
    );
    assert_eq!(
        record_of("a.b._netaudio-cmc._udp.local."),
        Some((Service::Cmc, "a.b".to_string()))
    );
    for bad in [
        "_netaudio-arc._udp.local.",
        "._netaudio-arc._udp.local.",
        "card_netaudio-arc._udp.local.",
        "tx 1@card._netaudio-chan._udp.local.",
        "card.local.",
    ] {
        assert_eq!(record_of(bad), None, "{bad:?}");
    }
    for s in [Service::Arc, Service::Dbc, Service::Cmc] {
        assert_eq!(Service::of(s.ty()), Some(s));
        assert_eq!(Service::of(&s.ty().to_ascii_uppercase()), Some(s));
        assert_eq!(Service::of(s.ty().trim_end_matches('.')), Some(s));
    }
    assert_eq!(Service::of("_netaudio-chan._udp.local."), None);
}

/// A response with `answers`.
fn response(answers: Vec<ResourceRecord<'static>>) -> Vec<u8> {
    let mut p = Packet::new_reply(0);
    p.answers = answers;
    p.build_bytes_vec().unwrap()
}

fn ptr(ty: &'static str, instance: &'static str, ttl: u32) -> ResourceRecord<'static> {
    ResourceRecord::new(
        Name::new_unchecked(ty),
        CLASS::IN,
        ttl,
        RData::PTR(PTR(Name::new_unchecked(instance))),
    )
}

fn srv_rr(instance: &'static str, ttl: u32) -> ResourceRecord<'static> {
    ResourceRecord::new(
        Name::new_unchecked(instance),
        CLASS::IN,
        ttl,
        RData::SRV(SRV {
            priority: 0,
            weight: 0,
            port: 4440,
            target: Name::new_unchecked("card.local"),
        }),
    )
}

#[test]
fn a_datagram_names_the_instances_its_ptrs_point_at_and_its_goodbyes() {
    let bytes = response(vec![
        ptr(
            "_netaudio-arc._udp.local",
            "Card._netaudio-arc._udp.local",
            4500,
        ),
        ptr(
            "_netaudio-cmc._udp.local",
            "Card._netaudio-cmc._udp.local",
            4500,
        ),
        // Another service's PTR, and a PTR to an instance of another type.
        ptr(
            "_netaudio-chan._udp.local",
            "tx 1@card._netaudio-chan._udp.local",
            4500,
        ),
        ptr(
            "_netaudio-arc._udp.local",
            "card._netaudio-chan._udp.local",
            4500,
        ),
        // A PTR of another type naming one of ours: not ours to take.
        ptr(
            "_netaudio-chan._udp.local",
            "Stray._netaudio-arc._udp.local",
            4500,
        ),
        // A goodbye of the PTR and one of an SRV.
        ptr(
            "_netaudio-dbc._udp.local",
            "Old._netaudio-dbc._udp.local",
            0,
        ),
        srv_rr("Gone._netaudio-arc._udp.local", 0),
        // A live SRV names nothing (a PTR does).
        srv_rr("Other._netaudio-arc._udp.local", 120),
    ]);
    let h = heard(&bytes);
    assert_eq!(
        h.named,
        [
            (Service::Arc, "Card".to_string()),
            (Service::Cmc, "Card".to_string())
        ]
    );
    assert_eq!(
        h.gone,
        [
            (Service::Dbc, "Old".to_string()),
            (Service::Arc, "Gone".to_string())
        ]
    );
    // A query, or bytes that do not read, say nothing.
    assert_eq!(heard(&sweep_query(1)), Heard::default());
    assert_eq!(heard(&[1, 2, 3]), Heard::default());
}

#[test]
fn the_sweep_asks_the_three_services_ptr_once() {
    let bytes = sweep_query(7);
    let p = Packet::parse(&bytes).unwrap();
    assert!(!p.has_flags(simple_dns::PacketFlag::RESPONSE));
    assert_eq!(p.id(), 7);
    let asked: Vec<String> = p.questions.iter().map(|q| q.qname.to_string()).collect();
    let want: Vec<String> = SERVICES
        .iter()
        .map(|s| s.trim_end_matches('.').to_string())
        .collect();
    assert_eq!(asked, want);
    assert!(
        p.questions
            .iter()
            .all(|q| q.qtype == QTYPE::TYPE(TYPE::PTR) && !q.unicast_response)
    );
}

#[test]
fn a_sweep_waits_between_its_least_and_most_and_spreads() {
    let waits: Vec<Duration> = (0..200u64).map(|s| sweep_after(s * 1_000_003)).collect();
    assert!(
        waits
            .iter()
            .all(|w| *w >= SWEEP && *w <= SWEEP + SWEEP_JITTER)
    );
    let spread = waits
        .iter()
        .max()
        .unwrap()
        .saturating_sub(*waits.iter().min().unwrap());
    assert!(spread > SWEEP_JITTER / 2, "{spread:?}");
    assert_eq!(sweep_after(0), SWEEP);
    // Its most is reached too.
    assert!(
        (0..200_000u64).any(|s| sweep_after(s) == SWEEP + SWEEP_JITTER),
        "the jitter's top"
    );
}

#[test]
fn names_are_kept_named_or_fresh_and_forgotten_after_a_while() {
    let mut n = Names::default();
    let named = Heard {
        named: vec![
            (Service::Arc, "Card".to_string()),
            (Service::Dbc, "card".to_string()),
        ],
        gone: Vec::new(),
    };
    assert_eq!(n.take(&named, 0), 2);
    assert_eq!(n.take(&named, 1), 0, "known already");
    assert_eq!(n.all(FORGET_NS - 1).len(), 2);
    // Kept by a fresh record, forgotten without one.
    n.touch(Service::Arc, "CARD", FORGET_NS - 1);
    assert_eq!(n.all(FORGET_NS + 1), [(Service::Arc, "Card".to_string())]);
    // A goodbye forgets at once.
    let bye = Heard {
        named: Vec::new(),
        gone: vec![(Service::Arc, "card".to_string())],
    };
    n.take(&bye, FORGET_NS + 2);
    assert!(n.all(FORGET_NS + 2).is_empty());
    // Bounded.
    let many = Heard {
        named: (0..MAX_NAMES + 3)
            .map(|i| (Service::Arc, format!("d{i}")))
            .collect(),
        gone: Vec::new(),
    };
    assert_eq!(n.take(&many, 0), MAX_NAMES);
}

/// Devices' mDNS records answered as our own responder answers (each set
/// on its own, sent back to the asker), every query's names logged. It
/// answers until `BOUND` passes without a query.
/// Every query's names, with the port it came from.
type Asked = Arc<Mutex<Vec<(u16, String)>>>;

fn responder(hosts: Vec<Vec<ServiceRecord>>) -> (SocketAddrV4, Asked) {
    let sock = UdpSocket::bind((LOOPBACK, 0)).unwrap();
    sock.set_read_timeout(Some(BOUND)).unwrap();
    let std::net::SocketAddr::V4(at) = sock.local_addr().unwrap() else {
        panic!("IPv4")
    };
    let asked = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&asked);
    std::thread::spawn(move || {
        let mut buf = [0u8; 1500];
        while let Ok((n, from)) = sock.recv_from(&mut buf) {
            if let Ok(q) = Packet::parse(&buf[..n]) {
                let names = q
                    .questions
                    .iter()
                    .map(|q| (from.port(), q.qname.to_string()));
                log.lock().unwrap().extend(names);
            }
            for recs in &hosts {
                for d in crate::announce::responder::answers(&buf[..n], recs) {
                    sock.send_to(&d, from).unwrap();
                }
            }
        }
    });
    (at, asked)
}

fn rec(service: &'static str, name: &str, port: u16, txt: &[&str]) -> ServiceRecord {
    ServiceRecord {
        service,
        instance: name.to_string(),
        host: "card.local.".to_string(),
        ip: LOOPBACK,
        port,
        txt: txt.iter().map(|s| (*s).to_string()).collect(),
    }
}

/// The card's three records.
fn card() -> Vec<ServiceRecord> {
    vec![
        rec(ARC_SERVICE, "Card", 4440, &["model=X1", "mf=Acme"]),
        rec(DBC_SERVICE, "Card", 4455, &[]),
        rec(CMC_SERVICE, "Card", 8800, &["id=00000a4d070b0000"]),
    ]
}

/// Waits until `f` is true, at most `BOUND`.
fn eventually(what: &str, f: impl Fn() -> bool) {
    let deadline = Instant::now() + BOUND;
    while !f() {
        assert!(Instant::now() < deadline, "{what} never happened");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn live_discovery_sweeps_lists_a_device_and_drops_it_at_its_goodbye() {
    let (mdns, asked) = responder(vec![card()]);
    // The mDNS group stands in as a socket of the test's.
    let group = UdpSocket::bind((LOOPBACK, 0)).unwrap();
    let group_at = group.local_addr().unwrap();
    let listener = group.try_clone().unwrap();
    listener.set_read_timeout(Some(ROUND / 5)).unwrap();
    let (discovery, changes) = Discovery::start_to(LOOPBACK, mdns, Some(listener)).unwrap();
    let Change::Added(dev) = changes.recv_timeout(BOUND).unwrap() else {
        panic!("an addition first");
    };
    assert_eq!(dev.name, "Card");
    assert_eq!(dev.arc, SocketAddrV4::new(LOOPBACK, 4440));
    assert_eq!(dev.txt.get("model").map(String::as_str), Some("X1"));
    eventually("its flow control and id", || {
        discovery.devices().first().is_some_and(|d| {
            d.dbc == Some(SocketAddrV4::new(LOOPBACK, 4455))
                && d.device_id == Some([0, 0, 10, 77, 7, 11, 0, 0])
        })
    });
    // One sweep so far, from a port of our own: never mDNS's 5353, so its
    // answers come back to us alone (dstack #141).
    let queries = asked.lock().unwrap().clone();
    let sweeps = queries
        .iter()
        .filter(|(_, n)| Service::of(n).is_some())
        .count();
    assert_eq!(sweeps, 3, "{queries:?}");
    assert!(queries.iter().all(|(port, _)| *port != MDNS.port()));
    // The device's goodbye on the group: gone at once.
    let bye = response(vec![ptr(
        "_netaudio-arc._udp.local",
        "Card._netaudio-arc._udp.local",
        0,
    )]);
    UdpSocket::bind((LOOPBACK, 0))
        .unwrap()
        .send_to(&bye, group_at)
        .unwrap();
    loop {
        match changes.recv_timeout(BOUND).unwrap() {
            Change::Removed(name) => break assert_eq!(name, "Card"),
            Change::Changed(_) => {}
            other => panic!("{other:?}"),
        }
    }
    assert!(discovery.devices().is_empty());
    // The stop ends the thread before it returns: nothing holds the list.
    let devices = Arc::clone(&discovery.devices);
    let started = Instant::now();
    discovery.stop().unwrap();
    assert!(started.elapsed() < STOP_WAIT);
    assert_eq!(Arc::strong_count(&devices), 1);
}

#[test]
fn the_list_follows_with_nobody_listening_for_changes() {
    let (mdns, _) = responder(vec![card()]);
    let (discovery, changes) = Discovery::start_to(LOOPBACK, mdns, None).unwrap();
    drop(changes);
    eventually("the device listed", || !discovery.devices().is_empty());
    discovery.stop().unwrap();
}

#[test]
fn dropping_discovery_ends_its_thread() {
    let (mdns, _) = responder(Vec::new());
    let (discovery, _changes) = Discovery::start_to(LOOPBACK, mdns, None).unwrap();
    let stop = Arc::clone(&discovery.stop);
    drop(discovery);
    assert!(stop.load(Ordering::Relaxed));
}

#[test]
fn discovery_that_cannot_bind_its_address_is_an_error() {
    let not_ours = Ipv4Addr::new(192, 0, 2, 1);
    assert!(matches!(
        Discovery::start_to(not_ours, MDNS, None),
        Err(DiscoverError::Io(_))
    ));
}

#[test]
fn discovery_on_loopback_starts_and_stops() {
    let (discovery, _changes) = Discovery::start(LOOPBACK).unwrap();
    discovery.stop().unwrap();
}

#[test]
fn the_thread_s_time_runs_from_its_origin() {
    let origin = Instant::now()
        .checked_sub(Duration::from_secs(2))
        .expect("a clock 2 s old");
    let t = now_ns(origin);
    assert!((2_000_000_000..60_000_000_000).contains(&t), "{t}");
}

/// dstack #141 (v0.6.58, dcontroller): a device the sweeps still name is
/// not taken off the list while its record's queries catch up.
#[test]
fn a_name_is_named_lately_until_the_record_s_age() {
    let max = crate::subscribe::instance::MAX_AGE_NS;
    let mut n = Names::default();
    let named = Heard {
        named: vec![(Service::Arc, "Card".to_string())],
        gone: Vec::new(),
    };
    n.take(&named, 10);
    assert!(n.named_lately(Service::Arc, "CARD", 10 + max - 1));
    assert!(!n.named_lately(Service::Arc, "card", 10 + max));
    assert!(!n.named_lately(Service::Dbc, "card", 10));
    // A fresh record keeps a name known, not "named".
    n.touch(Service::Arc, "card", 10 + max);
    assert!(!n.named_lately(Service::Arc, "card", 10 + max));
    n.take(&named, 10 + max);
    assert!(n.named_lately(Service::Arc, "card", 10 + max));
}

/// dstack #190: an old dstack device (inferno's model id, no version key)
/// wrote its flow count as its slots; every other device states them.
#[test]
fn only_an_old_dstack_device_does_not_state_its_flow_slots() {
    let dev = |txt: &[(&str, &str)]| {
        let mut d = Devices::default();
        let Some(Change::Added(dev)) =
            d.set(Service::Arc, "x", Some(&srv([10, 0, 0, 5], 4440, txt)))
        else {
            panic!("listed");
        };
        dev
    };
    let ours = ("model", "_000000000000000b");
    assert!(!dev(&[ours, ("mf", "dasio")]).states_flow_slots());
    assert!(dev(&[ours, ("dstack_vers", "0.6.84")]).states_flow_slots());
    assert!(dev(&[("model", "_000a000100020003"), ("mf", "Audinate")]).states_flow_slots());
    assert!(dev(&[]).states_flow_slots(), "no model: not ours");
}

/// dstack #104/#203: our devices name their local status port in the ARC
/// TXT; only a port other than 0 counts.
#[test]
fn the_status_port_is_a_port_other_than_0() {
    let port = |v: Option<&str>| {
        let mut d = Devices::default();
        let txt: Vec<(&str, &str)> = v.map(|v| ("dstack_ctl", v)).into_iter().collect();
        let Some(Change::Added(dev)) =
            d.set(Service::Arc, "x", Some(&srv([10, 0, 0, 5], 4440, &txt)))
        else {
            panic!("listed");
        };
        dev.status_port()
    };
    assert_eq!(port(Some("41234")), Some(41234));
    assert_eq!(port(Some("65535")), Some(65535));
    assert_eq!(port(Some("1")), Some(1));
    assert_eq!(port(Some("0")), None);
    assert_eq!(port(Some("65536")), None);
    assert_eq!(port(Some("x")), None);
    assert_eq!(port(Some("")), None);
    assert_eq!(port(None), None);
}
