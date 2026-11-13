use super::*;

use std::cell::RefCell;
use std::net::UdpSocket;

use mdns_sd::ServiceInfo;

use crate::announce::{CHAN_SERVICE, ServiceRecord};
use crate::arc::client::TxChannel;
use crate::arc::{ArcOutcome, Live, handle};
use crate::ctl::Sent;
use crate::device::{DeviceConfig, DeviceState, Subscription};
use crate::flows::control::FlowRequest;
use crate::media::tx::TxTable;
use crate::subscribe::resolve::{Multicast, Own};
use crate::subscribe::{Action, MAX_FLOWS, Manager};

mod arc27;
mod record_race;

const TEST_DEVICE: &str = include_str!("../../../../../config/test-device.toml");
const S: u64 = 1_000_000_000;
const MS: u64 = 1_000_000;
const OUR_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 10);
const PEER_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 11);
const LOOPBACK: Ipv4Addr = Ipv4Addr::LOCALHOST;
/// Every wait in these tests ends.
const BOUND: Duration = Duration::from_secs(5);

fn cfg() -> DeviceConfig {
    DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-device.toml")
}

/// A record of `service` named `device` at `ip`:`port`, as the browse
/// hands it over.
fn record(service: &str, device: &str, ip: &str, port: u16) -> ServiceEvent {
    let info = ServiceInfo::new(service, device, "peer-device.local.", ip, port, None).unwrap();
    ServiceEvent::ServiceResolved(Box::new(info.as_resolved_service()))
}

fn gone(service: &str, device: &str) -> ServiceEvent {
    ServiceEvent::ServiceRemoved(service.to_string(), format!("{device}.{service}"))
}

/// A book that knows peer-device's records since 0.
fn peer_book() -> Book {
    peer_book_with(Timing::default())
}

/// [`peer_book`] on `timing`.
fn peer_book_with(timing: Timing) -> Book {
    let mut book = Book::new(timing);
    book.apply(&record(ARC_SERVICE, "peer-device", "10.0.0.11", 4440), 0);
    book.apply(&record(DBC_SERVICE, "peer-device", "10.0.0.11", 4455), 0);
    book
}

/// [`peer_book_with`] `timing`, the card's directory read at [`SETTLE_NS`].
fn read_peer_book(timing: Timing) -> Book {
    let mut book = peer_book_with(timing);
    assert_eq!(book.lookup("peer-device", "Spare", SETTLE_NS), None);
    let job = book.next(SETTLE_NS).unwrap();
    book.finished(&job, done(card(), 6), SETTLE_NS);
    book
}

/// A card's directory: 16 channels at 96 kHz, 24 bits, default names `01`
/// to `16`, channel 3 named `Spare`.
fn card() -> Directory {
    let listed = (1..=16)
        .map(|id| TxChannel {
            id,
            rate: 96_000,
            bits: 24,
            default: format!("{id:02}"),
            name: None,
        })
        .collect();
    Directory::new(listed, &[(3, "Spare".to_string())])
}

fn done(dir: Directory, requests: usize) -> Outcome {
    Outcome {
        result: Ok(dir),
        requests,
    }
}

fn no_reply() -> Outcome {
    Outcome {
        result: Err(ReadError::Timeout { op: 0x1000 }),
        requests: 1,
    }
}

const DBC: SocketAddrV4 = SocketAddrV4::new(PEER_IP, 4455);

#[test]
fn an_arc_resolved_channel_offers_the_dbc_port_4_slots_fpp_16_and_our_latency() {
    let dir = card();
    let want = Advertised {
        control: DBC,
        tx_channel_id: 3,
        nchan: 4,
        bits: 24,
        rate: 96_000,
        dbcp1: 0x1102,
        fpp_min: 16,
        fpp_max: 16,
        latency_ns: 0,
    };
    assert_eq!(offer(dir.find("spare").unwrap(), DBC, 16), want);
    // A book on the default timing (a config without arc_rx_fpp) offers it.
    let mut book = read_peer_book(Timing::default());
    assert_eq!(book.lookup("peer-device", "Spare", SETTLE_NS), Some(want));
}

#[test]
fn an_arc_resolved_channel_offers_the_fpp_its_timing_asks() {
    // #72 phase F: the device config's arc_rx_fpp, both ends of it.
    for fpp in [4, 2, 32] {
        let mut book = read_peer_book(Timing {
            fpp,
            ..Timing::default()
        });
        let a = book.lookup("peer-device", "Spare", SETTLE_NS).unwrap();
        assert_eq!((a.fpp_min, a.fpp_max), (fpp, fpp));
        assert_eq!(a, offer(card().find("Spare").unwrap(), DBC, fpp));
    }
}

#[test]
fn a_device_config_s_timing_asks_its_arc_rx_fpp() {
    assert_eq!(Timing::default().fpp, FPP);
    assert_eq!(Timing::of(&cfg()), Timing::default(), "16 when left out");
    let cfg = DeviceConfig {
        arc_rx_fpp: 4,
        ..cfg()
    };
    assert_eq!(
        Timing::of(&cfg),
        Timing {
            fpp: 4,
            ..Timing::default()
        }
    );
}

#[test]
fn many_lookups_of_one_device_make_one_read() {
    let mut book = peer_book();
    let t = SETTLE_NS;
    for _ in 0..4 {
        for ch in ["Spare", "01", "02", "16"] {
            assert_eq!(book.lookup("peer-device", ch, t), None);
        }
    }
    assert_eq!(book.wants(), 1);
    let job = book.next(t).unwrap();
    assert_eq!(
        job,
        Job {
            device: "peer-device".into(),
            arc: SocketAddrV4::new(PEER_IP, 4440),
            want: Want::Missing,
            kind: Kind::Channels,
            form: ListForm::V289,
        }
    );
    // While it runs: no second read, no want.
    assert_eq!(book.lookup("PEER-device", "01", t + MS), None);
    assert_eq!(book.next(t + 20 * S), None);
    assert_eq!(book.next_due(), None);
    assert_eq!(book.wants(), 1);
    book.finished(&job, done(card(), 6), t + 100 * MS);
    let at = t + 200 * MS;
    assert_eq!(
        book.lookup("peer-device", "SPARE", at)
            .unwrap()
            .tx_channel_id,
        3
    );
    assert_eq!(book.lookup("peer-device", "16", at).unwrap().control, DBC);
    assert_eq!(book.next(at), None);
    assert_eq!(
        book.counts(),
        Counts {
            reads: 1,
            requests: 6,
            ..Counts::default()
        }
    );
}

/// `book`'s reads over `minutes` of lookups of `channels` every 250 ms, each
/// read taking `takes` and ending with `outcome`: the reads' start times.
fn reads_over(
    book: &mut Book,
    minutes: u64,
    channels: &[&str],
    takes: u64,
    outcome: impl Fn() -> Outcome,
) -> Vec<u64> {
    let mut starts = Vec::new();
    let mut running: Option<(Job, u64)> = None;
    for tick in 0..=minutes * 60 * 4 {
        let t = tick * 250 * MS;
        if let Some((job, end)) = running.take() {
            if t >= end {
                book.finished(&job, outcome(), end);
            } else {
                running = Some((job, end));
            }
        }
        for ch in channels {
            book.lookup("peer-device", ch, t);
        }
        if let Some(job) = book.next(t) {
            assert!(running.is_none(), "two reads of one device at {t}");
            starts.push(t);
            running = Some((job, t + takes));
        }
    }
    starts
}

#[test]
fn a_dead_device_gets_at_most_12_reads_of_one_request_in_10_minutes() {
    let mut book = peer_book();
    let channels: Vec<String> = (1..=18).map(|n| format!("{n:02}")).collect();
    let names: Vec<&str> = channels.iter().map(String::as_str).collect();
    // Each read is one request that waits its 1 s.
    let starts = reads_over(&mut book, 10, &names, S, no_reply);
    assert_eq!(
        starts,
        [5, 15, 34, 71, 144, 217, 290, 363, 436, 509, 582].map(|s| s * S)
    );
    for pair in starts.windows(2) {
        assert!(pair[1] - pair[0] >= 9 * S + S);
    }
    let c = book.counts();
    assert_eq!((c.reads, c.no_reply, c.requests), (0, 11, 11));
    assert_eq!(c.failed(), 11);
}

#[test]
fn a_name_not_listed_is_read_again_within_the_same_bounds() {
    let mut book = peer_book();
    let starts = reads_over(&mut book, 10, &["Spare", "Nope"], 0, || done(card(), 6));
    assert_eq!(
        starts,
        [5, 14, 23, 41, 77, 149, 221, 293, 365, 437, 509, 581].map(|s| s * S)
    );
    let c = book.counts();
    assert_eq!((c.reads, c.requests), (12, 72));
    assert!(c.not_listed > 0);
    // The listed channel answers from the last read.
    assert_eq!(
        book.lookup("peer-device", "Spare", 581 * S)
            .unwrap()
            .tx_channel_id,
        3
    );
}

#[test]
fn an_old_directory_is_read_again_once_and_a_failure_clears_on_success() {
    let mut book = peer_book();
    assert_eq!(book.lookup("peer-device", "01", 5 * S), None);
    let job = book.next(5 * S).unwrap();
    book.finished(&job, no_reply(), 6 * S);
    // The wait after one failure: 9 s from the end.
    book.lookup("peer-device", "01", 6 * S);
    assert_eq!(book.next_due(), Some(15 * S));
    assert_eq!(book.next(15 * S - 1), None);
    let job = book.next(15 * S).unwrap();
    assert_eq!(job.want, Want::Missing);
    book.finished(&job, done(card(), 6), 15 * S);
    // Fresh for a minute, then old: one read, which clears the failures.
    assert!(book.lookup("peer-device", "01", 75 * S - 1).is_some());
    assert_eq!(book.lookup("peer-device", "01", 75 * S), None);
    let job = book.next(75 * S).unwrap();
    assert_eq!(job.want, Want::Old);
    assert_eq!(book.next(75 * S), None);
    book.finished(&job, done(card(), 6), 75 * S);
    assert!(book.lookup("peer-device", "01", 75 * S).is_some());
    // A name missing now waits the 9 s of no failure.
    assert_eq!(book.lookup("peer-device", "nope", 76 * S), None);
    assert_eq!(book.next_due(), Some(84 * S));
    let job = book.next(84 * S).unwrap();
    assert_eq!(job.want, Want::NotListed);
    assert_eq!(book.counts().reads, 2);
}

#[test]
fn no_read_before_its_records_settle_or_without_flow_control() {
    let mut book = Book::new(Timing::default());
    book.apply(
        &record(ARC_SERVICE, "peer-device", "10.0.0.11", 4440),
        2 * S,
    );
    // No `_netaudio-dbc` record: nothing to ask, so nothing read.
    assert_eq!(book.lookup("peer-device", "01", 60 * S), None);
    assert_eq!(book.next(60 * S), None);
    assert_eq!(book.counts().unknown, 1);
    book.apply(
        &record(DBC_SERVICE, "peer-device", "10.0.0.11", 4455),
        60 * S,
    );
    // Settled 5 s after the ARC record came, not after the DBC record.
    let mut book2 = Book::new(Timing::default());
    book2.apply(&record(DBC_SERVICE, "peer-device", "10.0.0.11", 4455), 0);
    book2.apply(
        &record(ARC_SERVICE, "peer-device", "10.0.0.11", 4440),
        2 * S,
    );
    assert_eq!(book2.lookup("peer-device", "01", 7 * S - 1), None);
    assert_eq!(book2.next(7 * S - 1), None);
    assert_eq!(book2.wants(), 0);
    assert_eq!(book2.lookup("peer-device", "01", 7 * S), None);
    assert_eq!(book2.wants(), 1);
    assert!(book2.next(7 * S).is_some());
    // A device the browse never showed.
    assert_eq!(book2.lookup("other-device", "01", 60 * S), None);
    assert_eq!(book2.counts().unknown, 1);
    // An ARC record again (after its goodbye) settles again.
    let job = book.next(60 * S);
    assert!(job.is_none());
    book.lookup("peer-device", "01", 60 * S);
    book.apply(&gone(ARC_SERVICE, "peer-device"), 61 * S);
    // Its want is dropped with the record.
    assert_eq!(book.next(61 * S), None);
    assert_eq!(book.next_due(), None);
    book.apply(
        &record(ARC_SERVICE, "peer-device", "10.0.0.11", 4440),
        62 * S,
    );
    assert_eq!(book.lookup("peer-device", "01", 66 * S), None);
    assert_eq!(book.next(66 * S), None);
    book.lookup("peer-device", "01", 67 * S);
    assert!(book.next(67 * S).is_some());
}

#[test]
fn the_browse_s_records_are_kept_case_aside_and_bounded() {
    let mut book = Book::new(Timing {
        settle_ns: 0,
        ..Timing::default()
    });
    assert_eq!(
        book.apply(
            &record(ARC_SERVICE, "Peer-Device", "10.0.0.12,10.0.0.11", 4440),
            0
        ),
        Applied::Resolved
    );
    assert_eq!(
        book.apply(&record(DBC_SERVICE, "PEER-device", "10.0.0.11", 4455), 0),
        Applied::Resolved
    );
    book.lookup("peer-device", "01", 0);
    let job = book.next(0).unwrap();
    // The lowest address.
    assert_eq!(job.arc, SocketAddrV4::new(PEER_IP, 4440));
    book.finished(&job, done(card(), 6), 0);
    assert_eq!(book.lookup("peer-DEVICE", "01", 0).unwrap().control, DBC);
    // A newer flow-control record moves the port.
    book.apply(&record(DBC_SERVICE, "peer-device", "10.0.0.11", 4456), 0);
    assert_eq!(
        book.lookup("peer-device", "01", 0).unwrap().control,
        SocketAddrV4::new(PEER_IP, 4456)
    );
    // Its goodbye: no flow control, nothing resolved.
    assert_eq!(
        book.apply(&gone(DBC_SERVICE, "Peer-Device"), 0),
        Applied::Removed
    );
    assert_eq!(book.lookup("peer-device", "01", 0), None);
    assert_eq!(
        book.apply(&gone(DBC_SERVICE, "peer-device"), 0),
        Applied::Ignored
    );
    // Both gone: the device is forgotten.
    assert_eq!(
        book.apply(&gone(ARC_SERVICE, "peer-device"), 0),
        Applied::Removed
    );
    assert_eq!(
        book.apply(&gone(ARC_SERVICE, "peer-device"), 0),
        Applied::Ignored
    );
    // Other services, records without an IPv4 address, names that are not
    // the service's, and other events change nothing.
    let other = ServiceInfo::new(
        CHAN_SERVICE,
        "01@peer-device",
        "peer-device.local.",
        "10.0.0.11",
        4455,
        None,
    )
    .unwrap();
    let no_ip = ServiceInfo::new(ARC_SERVICE, "x-device", "x.local.", "", 4440, None).unwrap();
    for ev in [
        ServiceEvent::ServiceResolved(Box::new(other.as_resolved_service())),
        ServiceEvent::ServiceResolved(Box::new(no_ip.as_resolved_service())),
        ServiceEvent::ServiceRemoved(
            CHAN_SERVICE.to_string(),
            format!("01@peer-device.{CHAN_SERVICE}"),
        ),
        ServiceEvent::ServiceRemoved(
            ARC_SERVICE.to_string(),
            format!("peer-device.{DBC_SERVICE}"),
        ),
        ServiceEvent::ServiceRemoved(ARC_SERVICE.to_string(), ARC_SERVICE.to_string()),
        ServiceEvent::ServiceRemoved(ARC_SERVICE.to_string(), format!(".{ARC_SERVICE}")),
        ServiceEvent::SearchStarted(ARC_SERVICE.to_string()),
    ] {
        assert_eq!(book.apply(&ev, 0), Applied::Ignored, "{ev:?}");
    }
    assert!(book.records.is_empty());
    // At most MAX_RECORDS devices; a known one is still updated.
    for n in 0..MAX_RECORDS {
        let ev = record(ARC_SERVICE, &format!("d{n}"), "10.0.0.11", 4440);
        assert_eq!(book.apply(&ev, 0), Applied::Resolved);
    }
    assert_eq!(
        book.apply(&record(ARC_SERVICE, "one-more", "10.0.0.11", 4440), 0),
        Applied::Ignored
    );
    assert_eq!(
        book.apply(&record(DBC_SERVICE, "d0", "10.0.0.11", 4455), 0),
        Applied::Resolved
    );
    assert_eq!(book.records.len(), MAX_RECORDS);
}

#[test]
fn at_most_max_devices_are_read() {
    let mut book = Book::new(Timing {
        settle_ns: 0,
        ..Timing::default()
    });
    for n in 0..=MAX_DEVICES {
        let device = format!("d{n}");
        book.apply(&record(ARC_SERVICE, &device, "10.0.0.11", 4440), 0);
        book.apply(&record(DBC_SERVICE, &device, "10.0.0.11", 4455), 0);
        book.lookup(&device, "01", 0);
    }
    assert_eq!(book.wants(), MAX_DEVICES as u64);
    assert_eq!(book.counts().unknown, 1);
    // A known one still asks.
    book.lookup("d0", "02", 0);
    assert_eq!(book.counts().unknown, 1);
    let mut jobs = 0;
    while book.next(0).is_some() {
        jobs += 1;
    }
    assert_eq!(jobs, MAX_DEVICES);
}

#[test]
fn a_failed_read_counts_why() {
    let mut book = peer_book();
    let errors = [
        ReadError::Io {
            op: 0x1000,
            kind: std::io::ErrorKind::ConnectionRefused,
        },
        ReadError::Timeout { op: 0x2000 },
        ReadError::Refused {
            op: 0x2010,
            status: 0xffff,
        },
        ReadError::Malformed { op: 0x2000 },
        ReadError::Stopped,
    ];
    let mut t = SETTLE_NS;
    for e in errors {
        book.lookup("peer-device", "01", t);
        let job = book.next(t).unwrap();
        book.finished(
            &job,
            Outcome {
                result: Err(e),
                requests: 2,
            },
            t,
        );
        t += 100 * S;
    }
    assert_eq!(
        book.counts(),
        Counts {
            no_reply: 2,
            refused: 1,
            malformed: 1,
            socket: 1,
            requests: 10,
            ..Counts::default()
        }
    );
    // A read ended for a device forgotten meanwhile only counts its requests.
    let job = Job {
        device: "gone".into(),
        arc: DBC,
        want: Want::Missing,
        kind: Kind::Channels,
        form: ListForm::V289,
    };
    book.finished(&job, done(card(), 3), t);
    assert_eq!(book.counts().requests, 13);
    assert_eq!(book.counts().reads, 0);
}

#[test]
fn the_report_is_at_most_once_a_minute_and_only_of_what_happened() {
    let mut r = Report::new(0);
    let one = Counts {
        reads: 1,
        requests: 6,
        ..Counts::default()
    };
    assert_eq!(r.due(REPORT_NS - 1, one), None);
    assert_eq!(r.due(REPORT_NS, one), Some(one));
    // Nothing new: no line; the next minute's counts are the difference.
    assert_eq!(r.due(2 * REPORT_NS, one), None);
    let more = Counts {
        reads: 3,
        requests: 18,
        not_listed: 4,
        ..Counts::default()
    };
    assert_eq!(r.due(3 * REPORT_NS - 1, more), None);
    assert_eq!(
        r.due(3 * REPORT_NS, more),
        Some(Counts {
            reads: 2,
            requests: 12,
            not_listed: 4,
            ..Counts::default()
        })
    );
    let c = Counts {
        reads: 6,
        no_reply: 1,
        refused: 2,
        malformed: 3,
        socket: 4,
        requests: 7,
        not_listed: 8,
        unknown: 5,
    };
    assert_eq!(c.failed(), 10);
    assert_eq!(c.since(&Counts::default()), c);
    assert_eq!(c.since(&c), Counts::default());
}

/// A log sink for the report's line.
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// What `f` logs at INFO or above on this thread, as text.
fn logged(f: impl FnOnce()) -> String {
    let sink = Sink::default();
    let writer = sink.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .finish();
    crate::test_log::undecided();
    tracing::subscriber::with_default(subscriber, f);
    let bytes = sink.0.lock().unwrap().clone();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn the_minute_s_report_is_one_line_of_numbers() {
    let mut r = Report::new(0);
    let c = Counts {
        reads: 2,
        no_reply: 1,
        requests: 13,
        not_listed: 40,
        ..Counts::default()
    };
    let early = logged(|| r.look(REPORT_NS - 1, c));
    assert_eq!(early, "");
    let line = logged(|| r.look(REPORT_NS, c));
    assert_eq!(line.lines().count(), 1, "{line}");
    for part in [
        "ARC directory reads in a minute",
        "reads=2",
        "failed=1",
        "no_reply=1",
        "requests=13",
        "not_listed=40",
        "unknown_device=0",
    ] {
        assert!(line.contains(part), "{part}: {line}");
    }
    assert_eq!(logged(|| r.look(2 * REPORT_NS, c)), "");
}

/// A resolver of a book at a fixed time.
struct At(RefCell<Book>, u64);

impl Resolver for At {
    fn lookup(&self, tx_device: &str, tx_channel: &str) -> Option<Advertised> {
        self.0.borrow_mut().lookup(tx_device, tx_channel, self.1)
    }
}

fn sub(channel: &str, device: &str) -> Subscription {
    Subscription {
        tx_channel: channel.to_string(),
        tx_device: device.to_string(),
    }
}

/// The manager of the device `cfg`, RX 1 and 2 subscribed to the card's
/// Spare and 05, ticked once on a book on `timing` that read the card: its
/// one request, and the manager.
fn manager_asks_the_card(
    cfg: &DeviceConfig,
    timing: Timing,
) -> (SocketAddrV4, FlowRequest, Manager) {
    let ports: [u16; MAX_FLOWS] = std::array::from_fn(|s| 50_000 + s as u16);
    let mut m = Manager::new(cfg, "test-device", OUR_IP, ports);
    m.set(0, Some(sub("Spare", "peer-device")));
    m.set(1, Some(sub("05", "peer-device")));
    let at = At(RefCell::new(read_peer_book(timing)), SETTLE_NS);
    let actions = m.tick(SETTLE_NS, &[0; MAX_FLOWS], &at);
    let [Action::Request { to, req, .. }] = actions.as_slice() else {
        panic!("one request: {actions:?}");
    };
    (*to, req.clone(), m)
}

/// The flow request RX 1 and 2 make of the card at `fpp`.
fn card_request(fpp: u16) -> FlowRequest {
    FlowRequest {
        rx_host: "test-device".into(),
        rx_flow_name: "1_0".into(),
        sample_rate: 96_000,
        bits: 24,
        channels: vec![Some(3), Some(5), None, None],
        fpp,
        rx: SocketAddrV4::new(OUR_IP, 50_000),
    }
}

#[test]
fn the_manager_asks_an_arc_resolved_transmitter_for_4_slots_at_fpp_16() {
    let cfg = cfg();
    let (to, req, m) = manager_asks_the_card(&cfg, Timing::default());
    assert_eq!(to, DBC);
    assert_eq!(req, card_request(16));
    let flow = m.flow(0).unwrap();
    // Played out at our own latency.
    assert_eq!(flow.latency_ns, cfg.latency_ns);
}

#[test]
fn the_manager_asks_an_arc_resolved_transmitter_for_the_config_s_arc_rx_fpp() {
    // #72 phase F: fewer frames a packet than the card's guessed 16, so its
    // packets leave it sooner.
    let cfg = DeviceConfig {
        arc_rx_fpp: 4,
        ..cfg()
    };
    let (to, req, m) = manager_asks_the_card(&cfg, Timing::of(&cfg));
    assert_eq!(to, DBC);
    assert_eq!(req, card_request(4));
    assert_eq!(m.flow(0).unwrap().fpp, 4);
}

/// Devices' mDNS records answered as our own responder answers a direct
/// query (each host's set on its own, sent back to the asker), the name of
/// every question asked logged. It answers until `BOUND` passes without a
/// query.
fn responder(hosts: Vec<Vec<ServiceRecord>>) -> (SocketAddrV4, Arc<Mutex<Vec<String>>>) {
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
            if let Ok(q) = simple_dns::Packet::parse(&buf[..n]) {
                let names = q.questions.iter().map(|q| q.qname.to_string());
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

/// `device`'s record of `service` at `ip`:`port`, its host `<device>.local.`.
fn rec(
    service: &'static str,
    instance: &str,
    device: &str,
    ip: Ipv4Addr,
    port: u16,
) -> ServiceRecord {
    ServiceRecord {
        service,
        instance: instance.to_string(),
        host: format!("{device}.local."),
        ip,
        port,
        txt: Vec::new(),
    }
}

/// Whether a question asked for a whole service (a browse's PTR query)
/// rather than one instance.
fn whole_service(name: &str) -> bool {
    [CHAN_SERVICE, ARC_SERVICE, DBC_SERVICE].iter().any(|s| {
        s.trim_end_matches('.')
            .eq_ignore_ascii_case(name.trim_end_matches('.'))
    })
}

/// Our own ARC server on loopback (the test device's channels) standing in
/// for a card: it answers until `BOUND` passes without a request and counts
/// what it got.
fn our_server() -> (SocketAddrV4, Arc<Mutex<Vec<u16>>>) {
    let sock = UdpSocket::bind((LOOPBACK, 0)).unwrap();
    sock.set_read_timeout(Some(BOUND)).unwrap();
    let std::net::SocketAddr::V4(at) = sock.local_addr().unwrap() else {
        panic!("IPv4")
    };
    let got = Arc::new(Mutex::new(Vec::new()));
    let g = Arc::clone(&got);
    std::thread::spawn(move || {
        let cfg = cfg();
        let mut state = DeviceState::from_config(&cfg);
        state.tx[2] = "Spare".to_string();
        let tx = TxTable::new(0);
        let rx = Manager::new(&cfg, &cfg.name, LOOPBACK, [0; MAX_FLOWS]);
        let mut buf = [0u8; 2048];
        while let Ok((n, from)) = sock.recv_from(&mut buf) {
            g.lock().unwrap().push(u16::from_be_bytes([buf[6], buf[7]]));
            let live = Live { tx: &tx, rx: &rx };
            if let ArcOutcome::Reply(d) = handle(&mut state, &cfg, &live, &buf[..n]) {
                sock.send_to(&d, from).unwrap();
            }
        }
    });
    (at, got)
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
fn the_network_asks_each_channel_s_record_and_reads_a_device_without_them() {
    let (at, got) = our_server();
    // A rack publishes channel records; a card only its ARC and
    // flow-control records.
    let mut rack = rec(
        CHAN_SERVICE,
        "TX 3@rack",
        "rack",
        Ipv4Addr::new(10, 0, 0, 12),
        4455,
    );
    rack.txt = [
        "id=3",
        "rate=96000",
        "enc=24",
        "dbcp1=0x1102",
        "fpp=32,2",
        "latency_ns=1000000",
        "nchan=8",
    ]
    .map(String::from)
    .to_vec();
    let card = vec![
        rec(ARC_SERVICE, "card", "card", LOOPBACK, at.port()),
        rec(DBC_SERVICE, "card", "card", LOOPBACK, 4455),
    ];
    let (mdns, asked) = responder(vec![vec![rack], card]);
    let t = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut network = Network {
        instances: Instances::with_clock(Arc::clone(&t)),
        // A settle the reader's clock must pass; a reader that is only
        // woken by the want (its idle wait is longer than the test).
        arc: ArcDirectory::new(Timing {
            settle_ns: 50 * MS,
            max_age_ns: MAX_AGE_NS,
            reply: Duration::from_secs(1),
            idle: Duration::from_secs(30),
            fpp: FPP,
        }),
    };
    assert!(network.instances.start_to(LOOPBACK, mdns).unwrap());
    eventually("the rack's record", || {
        network.lookup("rack", "TX 3").is_some()
    });
    let a = network.lookup("rack", "TX 3").unwrap();
    assert_eq!((a.nchan, a.fpp_max), (8, 32));
    // While their direct queries may still answer, the other names stay
    // away from the ARC directory.
    assert_eq!(network.lookup("rack", "TX 9"), None);
    assert_eq!(network.lookup("card", "Spare"), None);
    assert!(!network.arc.asked());
    // Past the grace a name with no record is read through the ARC.
    t.store(crate::subscribe::instance::GRACE_NS, Ordering::Relaxed);
    assert_eq!(network.lookup("card", "Spare"), None);
    assert!(network.arc.asked());
    let requests = Arc::new(Requests::default());
    assert!(
        network
            .arc
            .start(LOOPBACK, mdns, Arc::clone(&requests))
            .unwrap()
    );
    // Once for the directory's life.
    assert!(
        !network
            .arc
            .start(LOOPBACK, mdns, Arc::clone(&requests))
            .unwrap()
    );
    // The lookup answers once the records are in and the read is done; it
    // never blocks.
    eventually("the card's channel", || {
        let t = Instant::now();
        let found = network.lookup("card", "spare").is_some();
        assert!(t.elapsed() < Duration::from_millis(100));
        found
    });
    let a = network.lookup("card", "Spare").unwrap();
    assert_eq!(
        a,
        offer(
            &TxChannel {
                id: 3,
                rate: 96_000,
                bits: 24,
                default: "TX 3".into(),
                name: None,
            },
            SocketAddrV4::new(LOOPBACK, 4455),
            FPP
        )
    );
    // Other names of the card, once their own queries' grace is over.
    t.store(2 * crate::subscribe::instance::GRACE_NS, Ordering::Relaxed);
    assert_eq!(network.lookup("card", "TX 16"), None, "its query's grace");
    t.store(3 * crate::subscribe::instance::GRACE_NS, Ordering::Relaxed);
    assert_eq!(network.lookup("card", "TX 16").unwrap().tx_channel_id, 16);
    // One read: the counts, one page of channels, one of names.
    assert_eq!(*got.lock().unwrap(), [0x1000, 0x2000, 0x2010]);
    assert_eq!(
        requests.counts().sent,
        [
            Sent {
                peer: 1,
                op: 0x1000,
                n: 1
            },
            Sent {
                peer: 1,
                op: 0x2000,
                n: 1
            },
            Sent {
                peer: 1,
                op: 0x2010,
                n: 1
            },
        ]
    );
    let c = network.arc.counts();
    assert_eq!((c.reads, c.requests), (1, 3));
    // A name the card does not list (past its query's grace) asks again,
    // after the wait (9 s).
    assert_eq!(network.lookup("card", "TX 17"), None);
    t.store(4 * crate::subscribe::instance::GRACE_NS, Ordering::Relaxed);
    assert_eq!(network.lookup("card", "TX 17"), None);
    assert_eq!(network.arc.counts().not_listed, 1);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(got.lock().unwrap().len(), 3);
    // dstack #141: only instances were asked, never a whole service.
    let asked = asked.lock().unwrap().clone();
    let has = |n: &str| asked.iter().any(|a| a.eq_ignore_ascii_case(n));
    assert!(has("tx 3@rack._netaudio-chan._udp.local"), "{asked:?}");
    assert!(has("card._netaudio-arc._udp.local"), "{asked:?}");
    assert!(has("card._netaudio-dbc._udp.local"), "{asked:?}");
    assert!(!asked.iter().any(|a| whole_service(a)), "{asked:?}");
    network.arc.join_within(BOUND);
}

#[test]
fn own_records_then_channel_records_then_arc() {
    let cfg = cfg();
    let state = DeviceState::from_config(&cfg);
    let own = Own::new(&state, &cfg, OUR_IP);
    let network = Network::default();
    let both = (own, network);
    assert_eq!(
        both.lookup("test-device", "TX 1").unwrap().control.ip(),
        &OUR_IP
    );
    assert!(!both.1.arc.asked());
    assert_eq!(both.lookup("peer-device", "01"), None);
    assert!(both.1.arc.asked());
    assert!(!both.1.arc.started());
}

#[test]
fn a_dead_device_read_by_the_reader_stays_unresolved_and_is_counted() {
    // A port that never answers: the read waits its reply time once.
    let silent = UdpSocket::bind((LOOPBACK, 0)).unwrap();
    let std::net::SocketAddr::V4(at) = silent.local_addr().unwrap() else {
        panic!("IPv4")
    };
    let (mdns, _) = responder(vec![vec![
        rec(ARC_SERVICE, "card", "card", LOOPBACK, at.port()),
        rec(DBC_SERVICE, "card", "card", LOOPBACK, 4455),
    ]]);
    // The reader's idle wait is longer than the test: the want wakes it.
    let mut arc = ArcDirectory::new(Timing {
        settle_ns: 0,
        max_age_ns: MAX_AGE_NS,
        reply: Duration::from_millis(100),
        idle: Duration::from_secs(30),
        fpp: FPP,
    });
    let requests = Arc::new(Requests::default());
    assert!(arc.start(LOOPBACK, mdns, Arc::clone(&requests)).unwrap());
    // The records come by their direct queries (an ARC address lookup asks
    // for them and wants no read).
    let records = arc.records();
    eventually("the card's records", || {
        records.arc_at("card", LOOPBACK).is_ok()
            && lock(&arc.shared.book)
                .records
                .get("card")
                .is_some_and(|r| r.dbc.is_some())
    });
    // One lookup and no other: its want alone wakes the reader.
    assert_eq!(arc.lookup("card", "01"), None);
    let mut buf = [0u8; 64];
    silent.set_read_timeout(Some(BOUND)).unwrap();
    let n = silent.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], crate::arc::client::page_request(1, 0x1000, 1));
    eventually("the read's failure", || arc.counts().no_reply == 1);
    // The next read waits 9 s from the failure: none meanwhile.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(arc.lookup("card", "01"), None);
    assert_eq!(arc.counts().no_reply, 1);
    assert_eq!(
        requests.counts().sent,
        [Sent {
            peer: 1,
            op: 0x1000,
            n: 1
        }]
    );
    arc.join_within(BOUND);
}

#[test]
fn dropping_the_directory_ends_its_reader_at_once() {
    let (mdns, _) = responder(Vec::new());
    let mut arc = ArcDirectory::new(Timing {
        idle: Duration::from_secs(30),
        ..Timing::default()
    });
    assert!(
        arc.start(LOOPBACK, mdns, Arc::new(Requests::default()))
            .unwrap()
    );
    let shared = Arc::downgrade(&arc.shared);
    drop(arc);
    // The reader was asked to stop and woken: nothing holds the directory.
    eventually("the reader's end", || shared.upgrade().is_none());
}

/// The stop is set under the book's lock, which the reader holds from its
/// look at the stop to its wait, so a stop never lands in between and is
/// lost (a lost one leaves the reader asleep for its whole wait: a full run
/// of the crate's tests once waited one out, #72). While the test holds
/// the lock, the stop is not set.
#[test]
fn the_stop_is_set_under_the_book_s_lock() {
    let arc = ArcDirectory::new(Timing::default());
    let shared = Arc::clone(&arc.shared);
    let book = lock(&shared.book);
    let asking = {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || ask_stop(&shared))
    };
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !shared.stop.load(Ordering::Relaxed),
        "the stop was set while the book was locked"
    );
    drop(book);
    asking.join().unwrap();
    assert!(shared.stop.load(Ordering::Relaxed));
}

/// A device's directory of `n` channels at 96 kHz, 24 bits, `01` up.
fn device_of(n: u16) -> Directory {
    let listed = (1..=n)
        .map(|id| TxChannel {
            id,
            rate: 96_000,
            bits: 24,
            default: format!("{id:02}"),
            name: None,
        })
        .collect();
    Directory::new(listed, &[])
}

#[test]
fn an_arc_resolved_device_is_asked_for_no_more_slots_than_its_channels() {
    // iemmixer, 31.10.2026: a 2-channel wireless mic refused our flow of 4
    // slots with 0x0311 (capture on dstack #1). Dante's own records say
    // nchan = min(the most a flow carries, the TX channels) (announce.rs
    // does the same for ours), so a 2-channel device gets 2 slots.
    for (n, want) in [(1, 1), (2, 2), (3, 3), (4, 4), (16, 4)] {
        let mut book = peer_book_with(Timing::default());
        assert_eq!(book.lookup("peer-device", "01", SETTLE_NS), None);
        let job = book.next(SETTLE_NS).unwrap();
        book.finished(&job, done(device_of(n), 4), SETTLE_NS);
        let a = book
            .lookup("peer-device", "01", SETTLE_NS)
            .expect("listed after the read");
        assert_eq!(a.nchan, want, "{n} channels");
    }
}

/// dstack #1: the TX side finds a receiver's ARC by its device name in the
/// records, and asking marks the directory asked (the daemon starts it).
#[test]
fn a_device_s_arc_address_is_found_by_name_and_asks_for_the_queries() {
    let dir = ArcDirectory::new(Timing::default());
    let records = dir.records();
    let no_record = Err(ArcMiss::NoRecord);
    assert!(!dir.asked());
    assert_eq!(records.arc_at("peer-device", PEER_IP), no_record);
    assert!(dir.asked());
    let arc = record(ARC_SERVICE, "peer-device", "10.0.0.11", 4441);
    lock(&dir.shared.book).apply(&arc, 0);
    let at = Ok(SocketAddrV4::new(PEER_IP, 4441));
    assert_eq!(records.arc_at("peer-device", PEER_IP), at);
    assert_eq!(records.arc_at("PEER-Device", PEER_IP), at, "case aside");
    assert_eq!(records.arc_at("other", PEER_IP), no_record);
    assert_eq!(
        records.arc_at("peer-device", OUR_IP),
        Err(ArcMiss::OtherAddress)
    );
    // A device with only its flow-control record has no ARC address.
    let dbc = record(DBC_SERVICE, "dbc-only", "10.0.0.12", 4455);
    lock(&dir.shared.book).apply(&dbc, 0);
    assert_eq!(records.arc_at("dbc-only", dbc_ip()), no_record);
    // Several cards: every address of the record is the device's, at the
    // record's port; the lowest stays the directory's (`arc`).
    let multi = record(ARC_SERVICE, "multi", "10.0.0.20, 10.0.0.5", 4443);
    lock(&dir.shared.book).apply(&multi, 0);
    for ip in [Ipv4Addr::new(10, 0, 0, 5), Ipv4Addr::new(10, 0, 0, 20)] {
        assert_eq!(records.arc_at("multi", ip), Ok(SocketAddrV4::new(ip, 4443)));
    }
    assert_eq!(
        lock(&dir.shared.book).records["multi"].arc,
        Some(SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 5), 4443))
    );
    // Its withdrawal forgets every address.
    lock(&dir.shared.book).apply(&gone(ARC_SERVICE, "multi"), 0);
    assert_eq!(
        records.arc_at("multi", Ipv4Addr::new(10, 0, 0, 20)),
        no_record
    );
}

/// The dbc-only record's address.
fn dbc_ip() -> Ipv4Addr {
    Ipv4Addr::new(10, 0, 0, 12)
}

/// A record with more addresses than kept keeps the lowest ones.
#[test]
fn a_record_keeps_its_lowest_addresses_up_to_the_bound() {
    let ips: Vec<String> = (1..=MAX_ARC_ADDRESSES + 2)
        .rev()
        .map(|i| format!("10.0.1.{i}"))
        .collect();
    let mut book = Book::new(Timing::default());
    book.apply(&record(ARC_SERVICE, "many", &ips.join(","), 4440), 0);
    let kept = &book.records["many"].arc_ips;
    assert_eq!(kept.len(), MAX_ARC_ADDRESSES);
    assert_eq!(kept[0], Ipv4Addr::new(10, 0, 1, 1));
    assert!(kept.windows(2).all(|w| w[0] < w[1]), "{kept:?}");
    let highest = u8::try_from(MAX_ARC_ADDRESSES + 2).unwrap();
    let dropped = Ipv4Addr::new(10, 0, 1, highest);
    assert_eq!(book.arc_at("many", dropped), Err(ArcMiss::OtherAddress));
}

/// The peer's TX channel `id` as an ARC-resolved record of 24 bits.
fn peer_channel(id: u16) -> Advertised {
    Advertised {
        control: DBC,
        tx_channel_id: id,
        nchan: 4,
        bits: 24,
        rate: 96_000,
        dbcp1: 0x1102,
        fpp_min: 16,
        fpp_max: 16,
        latency_ns: 0,
    }
}

/// A UltimoX2's TX flows (`crate::arc::flows`'s replay fixture): flow 1
/// multicast of TX 1 and 2, flow 2 unicast of TX 2 and 1.
fn ultimo_flows() -> Flows {
    use crate::arc::flows::tests::{ULTIMO_LIST, hex};
    crate::arc::flows::parse_flows(&hex(ULTIMO_LIST)[10..]).unwrap()
}

const ULTIMO_GROUP: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(239, 255, 0, 1), 4321);

#[test]
fn a_device_s_flows_are_read_once_and_name_the_multicast_flow_of_a_channel() {
    let mut book = peer_book();
    let t = MS;
    // Not read yet: unknown, and one read wanted however often it is asked.
    for id in [1, 2, 1] {
        assert_eq!(
            book.multicast("Peer-Device", &peer_channel(id), t),
            MulticastLookup::Unknown
        );
    }
    assert_eq!(book.wants(), 1);
    let job = book.next(t).unwrap();
    assert_eq!(
        job,
        Job {
            device: "peer-device".into(),
            arc: SocketAddrV4::new(PEER_IP, 4440),
            want: Want::Missing,
            kind: Kind::Flows,
            form: ListForm::V289,
        }
    );
    // While it runs, still unknown, and no second read.
    assert_eq!(
        book.multicast("peer-device", &peer_channel(1), t),
        MulticastLookup::Unknown
    );
    assert_eq!(book.next(t), None);
    book.finished_flows(&job, Ok((ultimo_flows(), ListForm::V289)), 1, t + MS);
    assert_eq!(book.counts().requests, 1);
    let found = MulticastLookup::Found(Multicast {
        group: ULTIMO_GROUP,
        channels: vec![1, 2],
        fpp: 32,
        bits: 24,
    });
    assert_eq!(
        book.multicast("peer-device", &peer_channel(2), t + MS),
        found
    );
    // TX 3 is in no multicast flow; TX 1 at 16 bits is not this flow.
    assert_eq!(
        book.multicast("peer-device", &peer_channel(3), t + MS),
        MulticastLookup::None
    );
    let narrow = Advertised {
        bits: 16,
        ..peer_channel(1)
    };
    assert_eq!(
        book.multicast("peer-device", &narrow, t + MS),
        MulticastLookup::None
    );
    assert_eq!(book.next(t + MS), None);
    // Old: the last list still answers while a new read is wanted.
    let old = t + MS + MAX_AGE_NS;
    assert_eq!(book.multicast("peer-device", &peer_channel(1), old), found);
    let job = book.next(old).unwrap();
    assert_eq!((job.kind, job.want), (Kind::Flows, Want::Old));
}

#[test]
fn a_device_whose_flows_cannot_be_read_offers_none_and_waits_longer() {
    let mut book = peer_book();
    let t = SETTLE_NS;
    book.multicast("peer-device", &peer_channel(1), t);
    let job = book.next(t).unwrap();
    book.finished_flows(&job, Err(ReadError::Timeout { op: 0x2600 }), 1, t);
    assert_eq!(book.counts().no_reply, 1);
    assert_eq!(
        book.multicast("peer-device", &peer_channel(1), t),
        MulticastLookup::None
    );
    // Past its age it is wanted again, after the failure's wait only.
    let old = t + MAX_AGE_NS;
    book.multicast("peer-device", &peer_channel(1), old);
    assert_eq!(book.next(t + crate::subscribe::RETRY_NS - 1), None);
    assert_eq!(book.next_due(), Some(t + crate::subscribe::RETRY_NS));
    // Due at the wait's end exactly.
    assert_eq!(
        book.next(t + crate::subscribe::RETRY_NS).map(|j| j.kind),
        Some(Kind::Flows)
    );
}

#[test]
fn a_device_without_an_arc_record_is_waited_for_briefly_then_read_at_its_address() {
    let mut book = Book::new(Timing::default());
    let t = 7 * S;
    // Its record's query may not have answered yet: unknown for the grace.
    assert_eq!(
        book.multicast("peer-device", &peer_channel(1), t),
        MulticastLookup::Unknown
    );
    assert_eq!(
        book.multicast("other", &peer_channel(1), t + RECORD_GRACE_NS - 1),
        MulticastLookup::Unknown
    );
    // Past it, a read at its channel's address is wanted (dstack #148:
    // the DT_20 answers no query for its record).
    assert_eq!(
        book.multicast("peer-device", &peer_channel(1), t + RECORD_GRACE_NS),
        MulticastLookup::Unknown
    );
    assert_eq!(book.wants(), 1);
    // A record that comes is read at once, however young, at its address.
    book.apply(
        &record(ARC_SERVICE, "peer-device", "10.0.0.11", 4440),
        t + 3 * S,
    );
    assert_eq!(
        book.multicast("peer-device", &peer_channel(1), t + 3 * S),
        MulticastLookup::Unknown
    );
    let job = book.next(t + 3 * S).unwrap();
    assert_eq!(job.kind, Kind::Flows);
    assert_eq!(job.arc, SocketAddrV4::new(PEER_IP, 4440));
    // Its record gone before the read: the want is dropped.
    let mut book = peer_book();
    book.multicast("peer-device", &peer_channel(1), 0);
    book.apply(&gone(ARC_SERVICE, "peer-device"), 0);
    assert_eq!(book.next(0), None);
}

#[test]
fn a_flow_with_no_multicast_group_or_too_many_slots_carries_nothing() {
    let mut f = ultimo_flows().flows;
    let a = peer_channel(1);
    assert!(carried(&f, &a).is_some());
    f[0].destinations = vec![SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 1), 4321)];
    assert_eq!(carried(&f, &a), None);
    let mut f = ultimo_flows().flows;
    f[0].flags = 0x11;
    assert_eq!(carried(&f, &a), None);
    let mut f = ultimo_flows().flows;
    f[0].channels = (1..=crate::media::tx::MAX_CHANNELS as u16 + 1).collect();
    assert_eq!(carried(&f, &a), None);
    f[0].channels = (1..=crate::media::tx::MAX_CHANNELS as u16).collect();
    assert!(carried(&f, &a).is_some());
}

#[test]
fn the_reader_lists_a_device_s_flows_over_its_arc() {
    use crate::arc::flows::tests::{ULTIMO_LIST, hex};
    let dev = UdpSocket::bind((LOOPBACK, 0)).unwrap();
    dev.set_read_timeout(Some(BOUND)).unwrap();
    let std::net::SocketAddr::V4(at) = dev.local_addr().unwrap() else {
        panic!("IPv4")
    };
    let device = std::thread::spawn(move || {
        let mut buf = [0u8; 256];
        let (n, from) = dev.recv_from(&mut buf).unwrap();
        let mut reply = hex(ULTIMO_LIST);
        reply[4..6].copy_from_slice(&buf[4..6]);
        dev.send_to(&reply, from).unwrap();
        buf[..n].to_vec()
    });
    let (mdns, _) = responder(vec![vec![rec(
        ARC_SERVICE,
        "mics",
        "mics",
        LOOPBACK,
        at.port(),
    )]]);
    let mut arc = ArcDirectory::new(Timing {
        settle_ns: 0,
        max_age_ns: MAX_AGE_NS,
        reply: BOUND,
        idle: Duration::from_secs(30),
        fpp: FPP,
    });
    let requests = Arc::new(Requests::default());
    assert!(arc.start(LOOPBACK, mdns, Arc::clone(&requests)).unwrap());
    let records = arc.records();
    eventually("the device's record", || {
        records.arc_at("mics", LOOPBACK).is_ok()
    });
    let a = peer_channel(2);
    assert_eq!(arc.multicast("mics", "02", &a), MulticastLookup::Unknown);
    assert!(arc.asked());
    let asked = device.join().unwrap();
    assert_eq!(&asked[6..8], &0x2600u16.to_be_bytes());
    assert_eq!(&asked[..2], &0x2809u16.to_be_bytes());
    eventually("the list", || {
        matches!(arc.multicast("mics", "02", &a), MulticastLookup::Found(_))
    });
    let network = Network {
        instances: Instances::new(),
        arc,
    };
    let MulticastLookup::Found(m) = network.multicast("mics", "02", &a) else {
        panic!("the network answers from its ARC directory")
    };
    assert_eq!(m.group, ULTIMO_GROUP);
    let mut arc = network.arc;
    arc.join_within(BOUND);
}

#[test]
fn the_record_grace_runs_from_each_device_s_first_lookup() {
    let mut book = Book::new(Timing::default());
    book.multicast("early", &peer_channel(1), 0);
    // Ten minutes on, a device asked about for the first time is waited for.
    let later = 600 * S;
    assert_eq!(
        book.multicast("late", &peer_channel(1), later),
        MulticastLookup::Unknown
    );
    // Past the grace, a device with no record is read at its flow
    // control's address on ARC's port (dstack #148, v0.6.56 regression).
    assert_eq!(
        book.multicast("early", &peer_channel(1), later),
        MulticastLookup::Unknown
    );
    assert_eq!(
        book.multicast("late", &peer_channel(1), later + RECORD_GRACE_NS),
        MulticastLookup::Unknown
    );
}

/// dstack #148 (9.11.2026, a v0.6.56 regression): the DT_20 answers no
/// query for its `_netaudio-arc` instance, only for its channels. With no
/// record its flow list was never read: band and iem-mixer asked it for
/// unicast flows beside its multicast one, and band got "No more flows".
/// Past the record's grace its ARC is read at the channel's flow-control
/// address on ARC's port.
#[test]
fn a_device_without_an_arc_record_has_its_flows_read_at_its_address() {
    let mut book = Book::new(Timing::default());
    let a = peer_channel(2);
    assert_eq!(book.multicast("mics", &a, 0), MulticastLookup::Unknown);
    assert_eq!(book.next(0), None, "within the grace: no read");
    let t = RECORD_GRACE_NS;
    assert_eq!(book.multicast("mics", &a, t), MulticastLookup::Unknown);
    let job = book.next(t).expect("a read of its flows");
    assert_eq!(
        job,
        Job {
            device: "mics".into(),
            arc: SocketAddrV4::new(*DBC.ip(), crate::arc::PORT),
            want: Want::Missing,
            kind: Kind::Flows,
            form: ListForm::V289,
        }
    );
    book.finished_flows(&job, Ok((ultimo_flows(), ListForm::V289)), 1, t + MS);
    assert!(matches!(
        book.multicast("mics", &a, t + MS),
        MulticastLookup::Found(_)
    ));
}

#[test]
fn a_failed_flow_list_read_keeps_the_last_list() {
    let mut book = peer_book();
    book.multicast("peer-device", &peer_channel(1), MS);
    let job = book.next(MS).unwrap();
    book.finished_flows(&job, Ok((ultimo_flows(), ListForm::V289)), 1, MS);
    let old = MS + MAX_AGE_NS;
    book.multicast("peer-device", &peer_channel(1), old);
    let job = book.next(old).unwrap();
    book.finished_flows(&job, Err(ReadError::Timeout { op: 0x2600 }), 1, old);
    assert!(matches!(
        book.multicast("peer-device", &peer_channel(1), old),
        MulticastLookup::Found(_)
    ));
}

#[test]
fn devices_waited_for_are_bounded() {
    let mut book = Book::new(Timing::default());
    for k in 0..MAX_DEVICES {
        assert_eq!(
            book.multicast(&format!("dev-{k}"), &peer_channel(1), 0),
            MulticastLookup::Unknown
        );
    }
    // One more is not kept: none at once; the kept ones still wait.
    assert_eq!(
        book.multicast("one-more", &peer_channel(1), 0),
        MulticastLookup::None
    );
    assert_eq!(
        book.multicast("dev-0", &peer_channel(1), 0),
        MulticastLookup::Unknown
    );
}

#[test]
fn a_fresh_flow_list_asks_for_no_new_read() {
    let mut book = peer_book();
    book.multicast("peer-device", &peer_channel(1), MS);
    let job = book.next(MS).unwrap();
    book.finished_flows(&job, Ok((ultimo_flows(), ListForm::V289)), 1, MS);
    let wants = book.wants();
    for t in [MS, MAX_AGE_NS] {
        book.multicast("peer-device", &peer_channel(1), t);
    }
    assert_eq!(book.wants(), wants);
    assert_eq!(book.next_due(), None);
}

#[test]
fn the_devices_whose_flows_are_read_are_bounded() {
    let mut book = Book::new(Timing::default());
    for k in 0..=MAX_DEVICES {
        book.apply(
            &record(ARC_SERVICE, &format!("dev-{k}"), "10.0.0.11", 4440),
            0,
        );
    }
    for k in 0..MAX_DEVICES {
        assert_eq!(
            book.multicast(&format!("dev-{k}"), &peer_channel(1), 0),
            MulticastLookup::Unknown
        );
    }
    let last = format!("dev-{MAX_DEVICES}");
    assert_eq!(
        book.multicast(&last, &peer_channel(1), 0),
        MulticastLookup::None
    );
    // A device already kept is still answered.
    assert_eq!(
        book.multicast("dev-0", &peer_channel(1), 0),
        MulticastLookup::Unknown
    );
}

#[test]
fn a_flow_read_whose_socket_cannot_open_ends_as_a_failed_list() {
    let mut arc = ArcDirectory::new(Timing {
        settle_ns: 0,
        max_age_ns: MAX_AGE_NS,
        reply: Duration::from_millis(100),
        idle: Duration::from_secs(30),
        fpp: FPP,
    });
    // Our address is none of this host's: the reader's socket cannot bind.
    // (The record is the book's own: no queries run.)
    let not_ours = Ipv4Addr::new(192, 0, 2, 1);
    let requests = Arc::new(Requests::default());
    arc.start_reader(not_ours, Arc::clone(&requests)).unwrap();
    lock(&arc.shared.book).apply(&record(ARC_SERVICE, "mics", "127.0.0.1", 4440), 0);
    let a = peer_channel(1);
    assert_eq!(arc.multicast("mics", "01", &a), MulticastLookup::Unknown);
    eventually("the failed list", || {
        arc.multicast("mics", "01", &a) == MulticastLookup::None
    });
    assert_eq!(arc.counts().socket, 1);
    arc.join_within(BOUND);
}

#[test]
fn a_failed_start_is_tried_once_and_starts_nothing() {
    let mut arc = ArcDirectory::new(Timing::default());
    assert!(!arc.tried());
    // Our address is none of this host's: the queries' socket cannot bind.
    let not_ours = Ipv4Addr::new(192, 0, 2, 1);
    let mdns = SocketAddrV4::new(LOOPBACK, 9);
    assert!(
        arc.start(not_ours, mdns, Arc::new(Requests::default()))
            .is_err()
    );
    assert!(arc.tried() && !arc.started());
}
