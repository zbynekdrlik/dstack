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
