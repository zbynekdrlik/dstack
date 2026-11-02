use super::*;

use simple_dns::ResourceRecord;
use simple_dns::rdata::{A, SRV, TXT};

const S: u64 = 1_000_000_000;
/// A mic's instance as its device publishes it (the shape of the field's,
/// with a synthetic name): a space in the channel, the device after `@`.
const DEVICE: &str = "mics-01";
const CHANNEL: &str = "Vox 02";
const HOST: &str = "Other-Host-1.local";
const IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 12);

fn inst() -> String {
    instance(DEVICE, CHANNEL)
}

/// The mic's TXT (the field record's keys and values).
const TXT_KV: &[&str] = &[
    "txtvers=2",
    "dbcp1=0x1200",
    "id=2",
    "dbcp=0x1004",
    "rate=96000",
    "en=24",
    "pcm=3 4",
    "enc=24",
    "latency_ns=1000000",
    "fpp=32,32",
    "nchan=2",
];

/// An answer to the instance query: SRV and TXT, and the host's A when
/// `with_a`.
fn answer(with_a: bool) -> Vec<u8> {
    let name = inst();
    let name = Name::new_unchecked(name.trim_end_matches('.'));
    let mut p = Packet::new_reply(0);
    p.answers.push(ResourceRecord::new(
        name.clone(),
        CLASS::IN,
        120,
        RData::SRV(SRV {
            priority: 0,
            weight: 0,
            port: 4455,
            target: Name::new_unchecked(HOST),
        }),
    ));
    let mut txt = TXT::new();
    for kv in TXT_KV {
        txt.add_string(kv).unwrap();
    }
    p.answers
        .push(ResourceRecord::new(name, CLASS::IN, 120, RData::TXT(txt)));
    if with_a {
        p.additional_records.push(host_a());
    }
    p.build_bytes_vec().unwrap()
}

fn host_a() -> ResourceRecord<'static> {
    ResourceRecord::new(
        Name::new_unchecked(HOST),
        CLASS::IN,
        120,
        RData::A(A {
            address: u32::from(IP),
        }),
    )
}

fn a_answer() -> Vec<u8> {
    let mut p = Packet::new_reply(0);
    p.answers.push(host_a());
    p.build_bytes_vec().unwrap()
}

#[test]
fn a_query_asks_srv_and_txt_of_the_instance_spaces_and_at_included() {
    let bytes = query(7, &inst()).expect("a query");
    let p = Packet::parse(&bytes).unwrap();
    assert_eq!(p.id(), 7);
    let qs: Vec<(String, QTYPE)> = p
        .questions
        .iter()
        .map(|q| (q.qname.to_string(), q.qtype))
        .collect();
    let name = inst().trim_end_matches('.').to_string();
    assert_eq!(
        qs,
        [
            (name.clone(), QTYPE::TYPE(TYPE::SRV)),
            (name, QTYPE::TYPE(TYPE::TXT))
        ]
    );
    assert!(p.questions.iter().all(|q| !q.unicast_response));
    let host_bytes = host_query(8, HOST).unwrap();
    let host = Packet::parse(&host_bytes).unwrap();
    assert_eq!(host.questions.len(), 1);
    assert_eq!(host.questions[0].qname.to_string(), HOST);
    assert_eq!(host.questions[0].qtype, QTYPE::TYPE(TYPE::A));
}

#[test]
fn a_name_dns_cannot_carry_is_not_asked() {
    let long_label = format!("{}.local", "x".repeat(64));
    assert_eq!(query(1, &long_label), None);
    let long_name = vec!["y".repeat(63); 4].join(".") + ".local";
    assert!(long_name.len() >= 255);
    assert_eq!(query(1, &long_name), None);
    assert_eq!(host_query(1, "a..local"), None);
    assert!(query(1, &format!("{}.local", "x".repeat(63))).is_some());
}

#[test]
fn an_answer_reads_as_srv_txt_and_the_host_s_address() {
    let found = read(&answer(true), &inst()).expect("a DNS answer");
    assert_eq!(found.srv, Some((4455, HOST.to_ascii_lowercase())));
    let txt = found.txt.expect("its TXT");
    assert_eq!(txt.get("latency_ns").map(String::as_str), Some("1000000"));
    assert_eq!(txt.get("fpp").map(String::as_str), Some("32,32"));
    assert_eq!(found.hosts, [(HOST.to_ascii_lowercase(), IP)]);
    // Another instance's records are not this one's.
    let other = read(&answer(true), &instance(DEVICE, "Other")).unwrap();
    assert_eq!((other.srv, other.txt), (None, None));
    assert_eq!(read(b"not dns", &inst()), None);
}

#[test]
fn a_wanted_instance_is_asked_answered_and_used_until_it_ages() {
    let mut t = Table::default();
    assert_eq!(t.lookup(DEVICE, CHANNEL, 0), None);
    let name = key(&inst());
    assert_eq!(t.due(0), [Ask::Instance(name.clone())]);
    assert_eq!(t.due(QUICK_NS[0] - 1), [], "not again within 1 s");
    assert_eq!(t.due(QUICK_NS[0]), [Ask::Instance(name.clone())]);
    assert_eq!(t.answer(&answer(true), REQUERY_NS + 1), 1);
    let a = t
        .lookup(DEVICE, CHANNEL, REQUERY_NS + 2)
        .expect("the record");
    assert_eq!(
        (
            a.latency_ns,
            a.fpp_min,
            a.fpp_max,
            a.nchan,
            a.rate,
            a.tx_channel_id,
            a.dbcp1
        ),
        (1_000_000, 32, 32, 2, 96_000, 2, 0x1200)
    );
    assert_eq!(a.control, SocketAddrV4::new(IP, 4455));
    // Fresh: nothing to ask; from half its age, asked again while used.
    let answered = REQUERY_NS + 1;
    assert_eq!(t.due(answered + MAX_AGE_NS / 2 - 1), []);
    t.lookup(DEVICE, CHANNEL, answered + MAX_AGE_NS / 2);
    assert_eq!(
        t.due(answered + MAX_AGE_NS / 2),
        [Ask::Instance(name.clone())]
    );
    // Past its age the record is not used.
    assert_eq!(t.lookup(DEVICE, CHANNEL, answered + MAX_AGE_NS), None);
}

#[test]
fn an_answer_without_the_host_s_address_asks_for_it_then_completes() {
    let mut t = Table::default();
    t.lookup(DEVICE, CHANNEL, 0);
    t.due(0);
    assert_eq!(t.answer(&answer(false), S), 0);
    assert_eq!(t.lookup(DEVICE, CHANNEL, S), None);
    assert_eq!(
        t.due(REQUERY_NS),
        [Ask::Host(key(&inst()), HOST.to_ascii_lowercase())]
    );
    assert_eq!(t.answer(&a_answer(), REQUERY_NS + 1), 1);
    assert!(t.lookup(DEVICE, CHANNEL, REQUERY_NS + 2).is_some());
}

#[test]
fn an_instance_no_lookup_wants_any_more_is_not_asked() {
    let mut t = Table::default();
    t.lookup(DEVICE, CHANNEL, 0);
    t.due(0);
    assert_eq!(t.due(MAX_AGE_NS - 1).len(), 1, "still wanted");
    assert_eq!(t.due(MAX_AGE_NS), [], "a lookup a full age ago");
}

#[test]
fn an_answer_that_is_no_channel_record_is_not_used() {
    let name = inst();
    let name = Name::new_unchecked(name.trim_end_matches('.'));
    let mut p = Packet::new_reply(0);
    p.answers.push(ResourceRecord::new(
        name.clone(),
        CLASS::IN,
        120,
        RData::SRV(SRV {
            priority: 0,
            weight: 0,
            port: 4455,
            target: Name::new_unchecked(HOST),
        }),
    ));
    let txt = TXT::new().with_string("rate=96000").unwrap();
    p.answers
        .push(ResourceRecord::new(name, CLASS::IN, 120, RData::TXT(txt)));
    p.additional_records.push(host_a());
    let mut t = Table::default();
    t.lookup(DEVICE, CHANNEL, 0);
    assert_eq!(t.answer(&p.build_bytes_vec().unwrap(), 1), 0);
    assert_eq!(t.lookup(DEVICE, CHANNEL, 2), None);
}

#[test]
fn at_most_64_instances_are_kept() {
    let mut t = Table::default();
    for i in 0..MAX_INSTANCES {
        t.lookup(DEVICE, &format!("ch {i}"), 0);
    }
    t.lookup(DEVICE, "one more", 0);
    assert_eq!(t.due(0).len(), MAX_INSTANCES);
    // A kept one is still wanted again at the bound; the others age out.
    t.lookup(DEVICE, "ch 0", MAX_AGE_NS - 1);
    assert_eq!(
        t.due(MAX_AGE_NS),
        [Ask::Instance(key(&instance(DEVICE, "ch 0")))]
    );
}

#[test]
fn an_answer_with_the_srv_only_asks_the_instance_again() {
    let name = inst();
    let name = Name::new_unchecked(name.trim_end_matches('.'));
    let mut p = Packet::new_reply(0);
    p.answers.push(ResourceRecord::new(
        name,
        CLASS::IN,
        120,
        RData::SRV(SRV {
            priority: 0,
            weight: 0,
            port: 4455,
            target: Name::new_unchecked(HOST),
        }),
    ));
    let mut t = Table::default();
    t.lookup(DEVICE, CHANNEL, 0);
    t.due(0);
    assert_eq!(t.answer(&p.build_bytes_vec().unwrap(), 1), 0);
    assert_eq!(t.due(REQUERY_NS), [Ask::Instance(key(&inst()))]);
}

#[test]
fn a_name_of_254_bytes_is_asked_and_one_of_255_is_not() {
    let labels = |n: usize| {
        let mut parts = vec!["z".repeat(63); 3];
        parts.push("z".repeat(n - 3 * 64));
        parts.join(".")
    };
    assert_eq!(labels(254).len(), 254);
    assert!(query(1, &labels(254)).is_some());
    assert_eq!(query(1, &labels(255)), None);
}

#[test]
fn the_thread_asks_a_responder_and_the_lookup_then_has_the_record() {
    let responder = UdpSocket::bind("127.0.0.1:0").unwrap();
    responder
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let to = match responder.local_addr().unwrap() {
        std::net::SocketAddr::V4(a) => a,
        other => panic!("{other}"),
    };
    let mut i = Instances::new();
    assert_eq!(i.lookup(DEVICE, CHANNEL), None);
    assert!(i.start_to(Ipv4Addr::LOCALHOST, to).unwrap());
    assert!(!i.start_to(Ipv4Addr::LOCALHOST, to).unwrap(), "once");
    let mut buf = [0u8; 1500];
    let (n, from) = responder.recv_from(&mut buf).expect("a query");
    let q = Packet::parse(&buf[..n]).unwrap();
    assert_eq!(
        q.questions[0].qname.to_string(),
        inst().trim_end_matches('.')
    );
    responder.send_to(&answer(true), from).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let found = loop {
        if let Some(a) = i.lookup(DEVICE, CHANNEL) {
            break a;
        }
        assert!(Instant::now() < deadline, "never resolved");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!((found.latency_ns, found.fpp_max), (1_000_000, 32));
}

#[test]
fn a_lookup_marks_the_resolver_asked_and_a_failed_start_is_tried_once() {
    let mut i = Instances::new();
    assert!(!i.asked());
    assert_eq!(i.lookup(DEVICE, CHANNEL), None);
    assert!(i.asked());
    assert!(!i.tried() && !i.started());
    // No multicast on an address this box does not have.
    assert!(i.start(Ipv4Addr::new(192, 0, 2, 1)).is_err());
    assert!(i.tried() && !i.started());
}

#[test]
fn a_record_read_by_the_thread_ages_on_the_resolver_s_clock() {
    let responder = UdpSocket::bind("127.0.0.1:0").unwrap();
    responder
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let to = match responder.local_addr().unwrap() {
        std::net::SocketAddr::V4(a) => a,
        other => panic!("{other}"),
    };
    let t = Arc::new(std::sync::atomic::AtomicU64::new(5 * S));
    let mut i = Instances::with_clock(Arc::clone(&t));
    i.lookup(DEVICE, CHANNEL);
    i.start_to(Ipv4Addr::LOCALHOST, to).unwrap();
    let mut buf = [0u8; 1500];
    let (_, from) = responder.recv_from(&mut buf).expect("a query");
    responder.send_to(&answer(true), from).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while i.lookup(DEVICE, CHANNEL).is_none() {
        assert!(Instant::now() < deadline, "never resolved");
        std::thread::sleep(Duration::from_millis(10));
    }
    t.store(5 * S + MAX_AGE_NS - 1, Ordering::Relaxed);
    assert!(i.lookup(DEVICE, CHANNEL).is_some(), "fresh");
    t.store(5 * S + MAX_AGE_NS + S, Ordering::Relaxed);
    assert!(i.lookup(DEVICE, CHANNEL).is_none(), "aged on the clock");
}

#[test]
fn dropping_the_resolver_stops_its_thread() {
    let i = Instances::new();
    let stop = i.stop_flag();
    assert!(!stop.load(Ordering::Relaxed));
    drop(i);
    assert!(stop.load(Ordering::Relaxed));
}

#[test]
fn an_unanswered_name_is_asked_at_1_2_4_then_every_10_s() {
    let mut t = Table::default();
    t.lookup(DEVICE, CHANNEL, 0);
    let mut asked = Vec::new();
    let mut now = 0;
    while now <= 40 * S {
        t.lookup(DEVICE, CHANNEL, now);
        if !t.due(now).is_empty() {
            asked.push(now / (S / 10));
        }
        now += S / 10;
    }
    // In tenths of a second: 0, +1 s, +2 s, +4 s, then +10 s each.
    assert_eq!(asked, [0, 10, 30, 70, 170, 270, 370]);
    assert_eq!(requery_after(1), QUICK_NS[0]);
    assert_eq!(requery_after(3), QUICK_NS[2]);
    assert_eq!(requery_after(4), REQUERY_NS);
}

#[test]
fn a_name_is_young_until_answered_or_past_its_grace() {
    let mut t = Table::default();
    assert!(!t.young(DEVICE, CHANNEL, 0), "never wanted");
    t.lookup(DEVICE, CHANNEL, S);
    assert!(t.young(DEVICE, CHANNEL, S + GRACE_NS - 1));
    assert!(!t.young(DEVICE, CHANNEL, S + GRACE_NS), "past its grace");
    t.due(S);
    assert_eq!(t.answer(&answer(true), 2 * S), 1);
    assert!(!t.young(DEVICE, CHANNEL, 2 * S), "answered");
}

#[test]
fn only_a_running_resolver_keeps_a_young_name_from_arc() {
    let t = Arc::new(std::sync::atomic::AtomicU64::new(S));
    let mut i = Instances::with_clock(Arc::clone(&t));
    i.lookup(DEVICE, CHANNEL);
    assert!(!i.young(DEVICE, CHANNEL), "not querying: nothing kept");
    let responder = UdpSocket::bind("127.0.0.1:0").unwrap();
    let to = match responder.local_addr().unwrap() {
        std::net::SocketAddr::V4(a) => a,
        other => panic!("{other}"),
    };
    i.start_to(Ipv4Addr::LOCALHOST, to).unwrap();
    assert!(i.young(DEVICE, CHANNEL));
    t.store(S + GRACE_NS, Ordering::Relaxed);
    assert!(!i.young(DEVICE, CHANNEL), "past its grace");
}

#[test]
fn the_thread_spaces_its_queries() {
    // A wireless mic receiver answered none of a burst of 29 queries.
    let responder = UdpSocket::bind("127.0.0.1:0").unwrap();
    responder
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let to = match responder.local_addr().unwrap() {
        std::net::SocketAddr::V4(a) => a,
        other => panic!("{other}"),
    };
    let mut i = Instances::new();
    for ch in ["a", "b", "c"] {
        i.lookup(DEVICE, ch);
    }
    i.start_to(Ipv4Addr::LOCALHOST, to).unwrap();
    let mut buf = [0u8; 1500];
    let mut at = Vec::new();
    for _ in 0..3 {
        responder.recv_from(&mut buf).expect("a query");
        at.push(Instant::now());
    }
    for w in at.windows(2) {
        assert!(
            w[1].duration_since(w[0]) >= PACE - Duration::from_millis(2),
            "{:?}",
            w[1].duration_since(w[0])
        );
    }
}
