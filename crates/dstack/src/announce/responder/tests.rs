use super::*;

use simple_dns::QCLASS;
use simple_dns::Question;

use crate::announce::{ARC_SERVICE, CHAN_SERVICE};

const IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 31);
const HOST: &str = "test-device.local.";

/// Two of our records: the ARC one and a channel's.
fn recs() -> Vec<ServiceRecord> {
    let rec = |service, instance: &str, port, txt: &[&str]| ServiceRecord {
        service,
        instance: instance.to_string(),
        host: HOST.to_string(),
        ip: IP,
        port,
        txt: txt.iter().map(|s| s.to_string()).collect(),
    };
    vec![
        rec(
            ARC_SERVICE,
            "test-device",
            4440,
            &["arcp_vers=2.7.41", "mf=dstack"],
        ),
        rec(
            CHAN_SERVICE,
            "TX 1@test-device",
            4455,
            &["id=1", "fpp=32,4", ""],
        ),
    ]
}

fn ask(name: &str, t: QTYPE) -> Vec<u8> {
    let mut q = Packet::new_query(9);
    q.questions.push(Question::new(
        Name::new_unchecked(name),
        t,
        QCLASS::CLASS(CLASS::IN),
        false,
    ));
    q.build_bytes_vec().unwrap()
}

/// A record's name and type.
type Named = Vec<(String, TYPE)>;

/// The (name, type) of each record of `bytes`' answers and additionals.
fn records_of(bytes: &[u8]) -> (Named, Named) {
    let p = Packet::parse(bytes).unwrap();
    assert!(p.has_flags(simple_dns::PacketFlag::RESPONSE));
    let list = |rrs: &[ResourceRecord]| {
        rrs.iter()
            .map(|rr| (rr.name.to_string(), rr.rdata.type_code()))
            .collect()
    };
    (list(&p.answers), list(&p.additional_records))
}

#[test]
fn a_service_type_query_gets_its_ptr_with_srv_txt_and_a_along() {
    let reply = answer(
        &ask("_netaudio-arc._udp.local", QTYPE::TYPE(TYPE::PTR)),
        &recs(),
    )
    .expect("ours");
    let (answers, more) = records_of(&reply);
    assert_eq!(
        answers,
        [("_netaudio-arc._udp.local".to_string(), TYPE::PTR)]
    );
    let inst = "test-device._netaudio-arc._udp.local".to_string();
    assert_eq!(
        more,
        [
            (inst.clone(), TYPE::SRV),
            (inst, TYPE::TXT),
            ("test-device.local".to_string(), TYPE::A)
        ]
    );
    let p = Packet::parse(&reply).unwrap();
    let srv = p
        .additional_records
        .iter()
        .find_map(|rr| match &rr.rdata {
            RData::SRV(s) => Some((s.port, s.target.to_string())),
            _ => None,
        })
        .unwrap();
    assert_eq!(srv, (4440, "test-device.local".to_string()));
    let a = p
        .additional_records
        .iter()
        .find_map(|rr| match &rr.rdata {
            RData::A(a) => Some(Ipv4Addr::from(a.address)),
            _ => None,
        })
        .unwrap();
    assert_eq!(a, IP);
}

#[test]
fn an_instance_and_a_host_query_get_exactly_what_they_ask() {
    let inst = "TX 1@test-device._netaudio-chan._udp.local";
    let (answers, more) = records_of(&answer(&ask(inst, QTYPE::TYPE(TYPE::SRV)), &recs()).unwrap());
    assert_eq!(answers, [(inst.to_string(), TYPE::SRV)]);
    assert_eq!(more, [("test-device.local".to_string(), TYPE::A)]);
    let (answers, more) = records_of(&answer(&ask(inst, QTYPE::TYPE(TYPE::TXT)), &recs()).unwrap());
    assert_eq!(
        (answers, more),
        (vec![(inst.to_string(), TYPE::TXT)], vec![])
    );
    let (answers, _) = records_of(&answer(&ask(inst, QTYPE::ANY), &recs()).unwrap());
    assert_eq!(
        answers,
        [(inst.to_string(), TYPE::SRV), (inst.to_string(), TYPE::TXT)]
    );
    // The host's A once, though both records name it; asked in any case,
    // answered with our name.
    let (answers, more) =
        records_of(&answer(&ask("TEST-device.local", QTYPE::TYPE(TYPE::A)), &recs()).unwrap());
    assert_eq!(
        (answers, more),
        (vec![("test-device.local".to_string(), TYPE::A)], vec![])
    );
}

#[test]
fn a_query_for_nothing_of_ours_or_a_response_gets_no_reply() {
    assert_eq!(
        answer(&ask("_http._tcp.local", QTYPE::TYPE(TYPE::PTR)), &recs()),
        None
    );
    assert_eq!(
        answer(
            &ask("test-device._netaudio-arc._udp.local", QTYPE::TYPE(TYPE::A)),
            &recs()
        ),
        None,
        "an instance has no A"
    );
    assert_eq!(
        answer(
            &ask("_netaudio-arc._udp.local", QTYPE::TYPE(TYPE::PTR)),
            &[]
        ),
        None
    );
    let reply = answer(
        &ask("_netaudio-arc._udp.local", QTYPE::TYPE(TYPE::PTR)),
        &recs(),
    )
    .unwrap();
    assert_eq!(answer(&reply, &recs()), None, "a response is no query");
    assert_eq!(answer(b"junk", &recs()), None);
}

#[test]
fn a_query_from_5353_is_answered_to_the_group_a_legacy_one_to_its_sender() {
    let mdns = SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 9), PORT);
    assert_eq!(reply_to(mdns), SocketAddrV4::new(GROUP, PORT));
    let legacy = SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 9), 50_000);
    assert_eq!(reply_to(legacy), legacy);
}

#[test]
fn the_responder_answers_from_the_published_records_and_stops_when_dropped() {
    let published = Arc::new(Mutex::new(Vec::new()));
    let port = UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let at = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    let r = Responder::start_on(at, false, Arc::clone(&published)).unwrap();
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let query = ask("_netaudio-arc._udp.local", QTYPE::TYPE(TYPE::PTR));
    // Nothing published yet: no reply.
    client.send_to(&query, at).unwrap();
    let mut buf = [0u8; 1500];
    assert!(client.recv_from(&mut buf).is_err(), "no records, no reply");
    *published.lock().unwrap() = recs();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let n = loop {
        client.send_to(&query, at).unwrap();
        if let Ok((n, _)) = client.recv_from(&mut buf) {
            break n;
        }
        assert!(std::time::Instant::now() < deadline, "never answered");
    };
    let (answers, _) = records_of(&buf[..n]);
    assert_eq!(
        answers,
        [("_netaudio-arc._udp.local".to_string(), TYPE::PTR)]
    );
    let stop = Arc::clone(&r.stop);
    assert!(!r.stopped());
    drop(r);
    assert!(stop.load(Ordering::Relaxed));
}

/// A device of `n` channels as band-asio-dante publishes them: a channel
/// record each, with Dante's keys.
fn many(n: usize) -> Vec<ServiceRecord> {
    (1..=n)
        .map(|k| ServiceRecord {
            service: CHAN_SERVICE,
            instance: format!("Channel name {k:02}@band-asio-dante"),
            host: HOST.to_string(),
            ip: IP,
            port: 4455,
            txt: [
                format!("id={k}"),
                "rate=96000".to_string(),
                "en=24".to_string(),
                "enc=24".to_string(),
                "latency_ns=1000000".to_string(),
                "fpp=32,4".to_string(),
                "nchan=8".to_string(),
                "dbcp1=0x1102".to_string(),
                "dbcp=0x1004".to_string(),
                "pdbcp=1".to_string(),
            ]
            .to_vec(),
        })
        .collect()
}

#[test]
fn a_big_answer_is_split_into_datagrams_with_no_ip_fragments() {
    // dstack #141: band's 64 channels went out as one 9 kB datagram (IP
    // fragments flooded to every port); a UltimoX2 receiver read a spike at
    // each. Every datagram fits MAX_DATAGRAM; every instance's PTR travels
    // with its SRV and TXT; every datagram that names the host carries its A.
    let recs = many(64);
    let replies = answers(
        &ask("_netaudio-chan._udp.local", QTYPE::TYPE(TYPE::PTR)),
        &recs,
    );
    assert!(replies.len() > 1);
    let mut ptrs = Vec::new();
    for r in &replies {
        assert!(r.len() <= MAX_DATAGRAM, "{} bytes", r.len());
        let (answered, more) = records_of(r);
        for (name, t) in &answered {
            assert_eq!(*t, TYPE::PTR);
            assert_eq!(name, "_netaudio-chan._udp.local");
        }
        let p = Packet::parse(r).unwrap();
        for rr in &p.answers {
            let RData::PTR(PTR(target)) = &rr.rdata else {
                panic!("a PTR")
            };
            let inst = target.to_string();
            assert!(more.contains(&(inst.clone(), TYPE::SRV)), "{inst} SRV");
            assert!(more.contains(&(inst.clone(), TYPE::TXT)), "{inst} TXT");
            ptrs.push(inst);
        }
        assert!(more.contains(&("test-device.local".to_string(), TYPE::A)));
    }
    ptrs.sort();
    let mut want: Vec<String> = recs.iter().map(full).map(|f| key(&f)).collect();
    want.sort();
    let got: Vec<String> = ptrs.iter().map(|p| key(p)).collect();
    assert_eq!(got, want, "every channel answered once");
}

#[test]
fn an_answer_packs_as_many_channels_as_fit_and_sends_an_oversize_one_alone() {
    // 64 channels of ~150 bytes each fit in far fewer datagrams than one a
    // channel: each datagram but the last has no room for the next one.
    let recs = many(64);
    let replies = answers(
        &ask("_netaudio-chan._udp.local", QTYPE::TYPE(TYPE::PTR)),
        &recs,
    );
    assert!(replies.len() <= 64 / 6, "{} datagrams", replies.len());
    for r in &replies[..replies.len() - 1] {
        assert!(r.len() > MAX_DATAGRAM / 2, "{} bytes", r.len());
    }
    // A channel whose TXT alone passes a datagram still goes out, alone.
    let mut big = many(2);
    big[0].txt = (0..12)
        .map(|k| format!("k{k}={}", "x".repeat(200)))
        .collect();
    let replies = answers(
        &ask("_netaudio-chan._udp.local", QTYPE::TYPE(TYPE::PTR)),
        &big,
    );
    assert_eq!(replies.len(), 2);
    assert!(replies[0].len() > MAX_DATAGRAM);
    assert!(replies[1].len() <= MAX_DATAGRAM);
}

#[test]
fn an_announcement_carries_each_service_type_s_ptr_with_its_records() {
    let all = announcement(&recs());
    assert_eq!(all.len(), 1, "two records fit one datagram");
    let (answers, more) = records_of(&all[0]);
    assert_eq!(
        answers,
        [
            ("_netaudio-arc._udp.local".to_string(), TYPE::PTR),
            ("_netaudio-chan._udp.local".to_string(), TYPE::PTR),
        ]
    );
    let arc = "test-device._netaudio-arc._udp.local".to_string();
    let chan = "TX 1@test-device._netaudio-chan._udp.local".to_string();
    assert_eq!(
        more,
        [
            (arc.clone(), TYPE::SRV),
            (arc, TYPE::TXT),
            (chan.clone(), TYPE::SRV),
            (chan, TYPE::TXT),
            ("test-device.local".to_string(), TYPE::A),
        ]
    );
    // A service type named by several records is asked once: each
    // instance's PTR comes once.
    let mut twice = recs();
    twice.push(ServiceRecord {
        instance: "TX 2@test-device".to_string(),
        ..twice[1].clone()
    });
    let (answers, _) = records_of(&announcement(&twice)[0]);
    assert_eq!(answers.len(), 3, "{answers:?}");
    assert!(announcement(&[]).is_empty());
}

fn lo_at(index: u32) -> Vec<crate::membership::Link> {
    vec![crate::membership::Link {
        name: "lo".to_string(),
        ip: Ipv4Addr::LOCALHOST,
        index: Some(index),
    }]
}

/// Asks `query` at `at` until answered (at most 5 s): the answer.
fn answered(client: &UdpSocket, query: &[u8], at: SocketAddrV4) -> Vec<u8> {
    let mut buf = [0u8; 1500];
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        client.send_to(query, at).unwrap();
        if let Ok((n, _)) = client.recv_from(&mut buf) {
            return buf[..n].to_vec();
        }
        assert!(std::time::Instant::now() < deadline, "never answered");
    }
}

#[test]
fn after_an_address_re_add_the_responder_rejoins_announces_and_answers_again() {
    let published = Arc::new(Mutex::new(recs()));
    let port = UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let at = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
    listener
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let SocketAddr::V4(announce_to) = listener.local_addr().unwrap() else {
        panic!("bound on IPv4");
    };
    let list = Arc::new(Mutex::new(lo_at(26)));
    // The reads that found the address gone.
    let gone_reads = Arc::new(AtomicU64::new(0));
    let read = {
        let (list, gone_reads) = (Arc::clone(&list), Arc::clone(&gone_reads));
        move || {
            let now = list.lock().unwrap().clone();
            if now.is_empty() {
                gone_reads.fetch_add(1, Ordering::Relaxed);
            }
            Ok(now)
        }
    };
    let every = Duration::from_millis(20);
    let r = Responder::start_watched(
        at,
        false,
        Arc::clone(&published),
        Watch {
            read: Box::new(read),
            every,
            announce_to,
        },
    )
    .unwrap();
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let query = ask("_netaudio-arc._udp.local", QTYPE::TYPE(TYPE::PTR));
    let (answers, _) = records_of(&answered(&client, &query, at));
    assert_eq!(answers.len(), 1);
    // Several looks with nothing changed: no rejoin, no announcement.
    std::thread::sleep(every * 10);
    assert_eq!(r.rejoins(), 0);
    let mut buf = [0u8; 1500];
    assert!(listener.recv_from(&mut buf).is_err(), "announced unasked");
    // The address removed, then added again on another index.
    list.lock().unwrap().clear();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while gone_reads.load(Ordering::Relaxed) < 2 {
        assert!(std::time::Instant::now() < deadline, "the list never read");
        std::thread::sleep(every);
    }
    assert_eq!(r.rejoins(), 0, "nothing to join on while it is gone");
    *list.lock().unwrap() = lo_at(19);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let n = loop {
        if let Ok((n, _)) = listener.recv_from(&mut buf) {
            break n;
        }
        assert!(std::time::Instant::now() < deadline, "never announced");
    };
    let (answers, _) = records_of(&buf[..n]);
    assert_eq!(answers.len(), 2, "every service type: {answers:?}");
    assert_eq!(r.rejoins(), 1);
    // The new socket answers queries again.
    let (answers, _) = records_of(&answered(&client, &query, at));
    assert_eq!(
        answers,
        [("_netaudio-arc._udp.local".to_string(), TYPE::PTR)]
    );
    std::thread::sleep(every * 10);
    assert_eq!(r.rejoins(), 1, "one change, one rejoin");
}
