use super::*;
use crate::info::{Records, heartbeat_body};
use std::net::{SocketAddr, SocketAddrV4, UdpSocket};

/// A heartbeat datagram with `body`, from device id 1.
fn datagram(body: &[u8]) -> Vec<u8> {
    datagram_from([0, 0, 0, 0, 0, 0, 0, 1], body)
}

/// A heartbeat datagram with `body`, from device id `id`.
fn datagram_from(id: [u8; 8], body: &[u8]) -> Vec<u8> {
    let mut d = Vec::new();
    d.extend_from_slice(&START_HEARTBEAT.to_be_bytes());
    d.extend_from_slice(
        &u16::try_from(HEADER_LEN + body.len())
            .unwrap()
            .to_be_bytes(),
    );
    d.extend_from_slice(&[0, 7, 0, 0]);
    d.extend_from_slice(&id);
    d.extend_from_slice(b"Audinate");
    d.extend_from_slice(&op::HEARTBEAT);
    d.extend_from_slice(body);
    d
}

/// A hardware receiver's two records as heard on the site (a Yamaha
/// card, 64 slots, 96 kHz), cut to their first 12 values.
fn hardware_records(lateness: &[u32], late: &[u32]) -> Vec<u8> {
    let mut out = Vec::new();
    let n = 64usize;
    // 0x8003: head, n 0 24 0, the rate, n values.
    out.extend_from_slice(&u16::try_from(24 + 4 * n).unwrap().to_be_bytes());
    out.extend_from_slice(&[0x80, 0x03, 0x00, 0x04, 0x01, 0x0c, 0xae, 0x38, 0, 0]);
    out.extend_from_slice(&[0x00, 0x40, 0, 0, 0x00, 0x18, 0, 0]);
    out.extend_from_slice(&96_000u32.to_be_bytes());
    for k in 0..n {
        out.extend_from_slice(&lateness.get(k).copied().unwrap_or(0).to_be_bytes());
    }
    // 0x8004: head, n 0 20 0, n values.
    out.extend_from_slice(&u16::try_from(20 + 4 * n).unwrap().to_be_bytes());
    out.extend_from_slice(&[0x80, 0x04, 0x00, 0x04, 0x01, 0x08, 0xae, 0x38, 0, 0]);
    out.extend_from_slice(&[0x00, 0x40, 0, 0, 0x00, 0x14, 0, 0]);
    for k in 0..n {
        out.extend_from_slice(&late.get(k).copied().unwrap_or(0).to_be_bytes());
    }
    out
}

#[test]
fn a_hardware_receiver_s_records_read() {
    // The values the FOH card sent on 1.11 (slots 1 to 8).
    let lateness = [0x177, 0, 0x26, 7, 7, 0xa, 0x24, 0x50];
    let late = [0x5e, 0, 0, 0, 0x2c];
    let beat = parse(&datagram(&hardware_records(&lateness, &late))).expect("a heartbeat");
    let (rate, got) = beat.lateness.expect("0x8003");
    assert_eq!(rate, 96_000);
    assert_eq!(got.len(), 64);
    assert_eq!(got[..8], lateness);
    let got = beat.late.expect("0x8004");
    assert_eq!(got.len(), 64);
    assert_eq!(got[..5], late);
    assert!(got[5..].iter().all(|&v| v == 0));
}

#[test]
fn our_own_heartbeat_reads_back() {
    let mut lateness = [0u32; crate::arc::MAX_FLOWS as usize];
    lateness[0] = 48;
    lateness[31] = 9_600;
    let body = heartbeat_body(
        5,
        &Records {
            freq_ppb: -7,
            tx_peaks: vec![1, 2, 3],
            rx_peaks: vec![4],
            rate: 48_000,
            lateness,
            late: std::array::from_fn(|slot| slot as u32 * 3),
        },
    );
    let beat = parse(&datagram(&body)).expect("a heartbeat");
    assert_eq!(beat.lateness, Some((48_000, lateness.to_vec())));
    let late: Vec<u32> = (0..32).map(|slot| slot * 3).collect();
    assert_eq!(beat.late, Some(late));
}

#[test]
fn only_a_heartbeat_parses_and_a_bad_record_is_left_out() {
    // A heartbeat without a record that reads: its sender, nothing else.
    let empty = Beat {
        device_id: [0, 0, 0, 0, 0, 0, 0, 1],
        ..Beat::default()
    };
    let good = datagram(&hardware_records(&[1], &[2]));
    assert!(parse(&good).is_some());
    assert_eq!(
        parse(&good[..HEADER_LEN - 1]),
        None,
        "shorter than the header"
    );
    let mut info = good.clone();
    info[1] = 0xff;
    assert_eq!(parse(&info), None, "an info packet");
    let mut other = good.clone();
    other[31] = 1;
    assert_eq!(parse(&other), None, "another opcode");
    assert_eq!(parse(&datagram(&[])), Some(empty.clone()), "no records");
    // A record whose values pass it: left out, the next one still read.
    let mut records = hardware_records(&[1], &[2]);
    records[12..14].copy_from_slice(&0x7fu16.to_be_bytes());
    let beat = parse(&datagram(&records)).unwrap();
    assert_eq!(beat.lateness, None);
    assert_eq!(beat.late.unwrap()[0], 2);
    // A record longer than the datagram ends the walk.
    let mut long = hardware_records(&[1], &[2]);
    long[0..2].copy_from_slice(&0x7fffu16.to_be_bytes());
    assert_eq!(parse(&datagram(&long)), Some(empty.clone()));
    // A length under 4 ends it too.
    let mut tiny = hardware_records(&[1], &[2]);
    tiny[0..2].copy_from_slice(&3u16.to_be_bytes());
    assert_eq!(parse(&datagram(&tiny)), Some(empty.clone()));
    // A record of only its 4-byte length and type is stepped over.
    let mut bare = vec![0x00, 0x04, 0x80, 0x07];
    bare.extend(hardware_records(&[6], &[8]));
    let beat = parse(&datagram(&bare)).unwrap();
    assert_eq!(beat.lateness.unwrap().1[0], 6);
    // An unknown record is stepped over.
    let mut skip = vec![0x00, 0x08, 0x80, 0x01, 0, 0, 0, 0];
    skip.extend(hardware_records(&[3], &[4]));
    let beat = parse(&datagram(&skip)).unwrap();
    assert_eq!(beat.lateness.unwrap().1[0], 3);
}

#[test]
fn a_gather_turns_samples_into_us_and_counts_late_packets_in_the_window() {
    let mut g = Gather::default();
    let beat = |lateness: Vec<u32>, late: Vec<u32>| Beat {
        lateness: Some((96_000, lateness)),
        late: Some(late),
        ..Beat::default()
    };
    g.add(&beat(vec![96, 24], vec![1_000, 5]));
    g.add(&beat(vec![960, 0], vec![1_024, 5]));
    // The device started again: its count went down, all of it is new.
    g.add(&beat(vec![9_600, 1], vec![3, 5]));
    let got = g.result();
    assert_eq!((got.beats, got.rate), (3, 96_000));
    assert_eq!(
        got.slots,
        [
            SlotLatency {
                slot: 1,
                peaks_us: vec![1_000, 10_000, 100_000],
                late: 27,
                late_total: 3,
            },
            SlotLatency {
                slot: 2,
                peaks_us: vec![250, 0, 10],
                late: 0,
                late_total: 5,
            },
        ]
    );
    // A beat with one record only: the other slots' lists are untouched.
    let mut g = Gather::default();
    g.add(&Beat {
        lateness: None,
        late: Some(vec![7, 0, 1]),
        ..Beat::default()
    });
    let got = g.result();
    assert_eq!((got.beats, got.rate, got.slots.len()), (1, 0, 3));
    assert!(
        got.slots
            .iter()
            .all(|s| s.peaks_us.is_empty() && s.late == 0)
    );
    assert_eq!(got.slots[0].late_total, 7);
    // A rate of 0 does not divide by zero.
    let mut g = Gather::default();
    g.add(&Beat {
        lateness: Some((0, vec![5])),
        late: None,
        ..Beat::default()
    });
    assert_eq!(g.result().slots[0].peaks_us, [5_000_000]);
}

#[test]
fn a_histogram_bins_each_peak_once() {
    let edges = [250, 500, 1_000];
    assert_eq!(histogram(&[], &edges), [0, 0, 0, 0]);
    assert_eq!(
        histogram(&[0, 250, 251, 500, 999, 1_000, 1_001, 40_000], &edges),
        [2, 2, 2, 2]
    );
    assert_eq!(histogram(&[3, 9], &[]), [2]);
}

/// dasio (1.11): two of our devices on one address (own ports, own device
/// ids). Their heartbeats came from the same IP and mixed in one gather.
#[test]
fn two_devices_on_one_address_are_heard_apart_by_their_device_id() {
    let one = [0, 0, 10, 77, 7, 114, 0, 0];
    let two = [0, 0, 10, 77, 7, 114, 0, 1];
    let mut heard = Heard::default();
    for (id, peak, late) in [(one, 13, 0), (two, 960, 5), (one, 12, 0), (two, 96, 9)] {
        let beat = parse(&datagram_from(id, &hardware_records(&[peak], &[late]))).unwrap();
        assert_eq!(beat.device_id, id);
        heard.add(&beat);
    }
    let got = heard.result();
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!((got[0].device_id, got[0].beats), (one, 2));
    assert_eq!(got[0].slots[0].peaks_us, [135, 125]);
    assert_eq!((got[0].slots[0].late, got[0].slots[0].late_total), (0, 0));
    assert_eq!((got[1].device_id, got[1].beats), (two, 2));
    assert_eq!(got[1].slots[0].peaks_us, [10_000, 1_000]);
    assert_eq!((got[1].slots[0].late, got[1].slots[0].late_total), (4, 9));
    assert_eq!(Heard::default().result(), []);
}

/// The box's routed IPv4 (connecting a UDP socket picks the route and
/// sends nothing).
fn routed_ipv4() -> Ipv4Addr {
    let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("a UDP socket");
    probe
        .connect((Ipv4Addr::new(192, 0, 2, 1), 9))
        .expect("a default route on this box");
    match probe.local_addr().expect("the routed address") {
        SocketAddr::V4(a) if !a.ip().is_loopback() && !a.ip().is_unspecified() => *a.ip(),
        other => panic!("no routed IPv4 address on this box: {other}"),
    }
}

/// A device id no other sender on the box uses (dcontroller's E2E runs
/// devices on the same address): the process id and the time.
fn unique_id() -> [u8; 8] {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .subsec_nanos();
    let mut id = [0; 8];
    id[..4].copy_from_slice(&std::process::id().to_be_bytes());
    id[4..].copy_from_slice(&nanos.to_be_bytes());
    id
}

/// A heartbeat from `id` with one 0x8004 record: `late` for slot 1 and
/// `late + 1` for slot 2.
fn late_beat(id: [u8; 8], late: u32) -> Vec<u8> {
    let mut record = Vec::new();
    record.extend_from_slice(&28u16.to_be_bytes());
    record.extend_from_slice(&LATE.to_be_bytes());
    record.extend_from_slice(&[0, 4, 0, 24, 0, 1, 0, 0]);
    record.extend_from_slice(&[0, 2, 0, 0, 0, 20, 0, 0]);
    record.extend_from_slice(&late.to_be_bytes());
    record.extend_from_slice(&(late + 1).to_be_bytes());
    datagram_from(id, &record)
}

/// A sender on `ip` to the heartbeat group, looped back to this host.
fn group_sender(ip: Ipv4Addr) -> UdpSocket {
    let s = UdpSocket::bind((ip, 0)).expect("a sending socket");
    s.set_multicast_loop_v4(true).expect("multicast loop");
    let s = socket2::Socket::from(s);
    s.set_multicast_if_v4(&ip).expect("the multicast interface");
    s.into()
}

/// The next beat from `id` within `within`, skipping other senders'.
fn next_from(
    rx: &std::sync::mpsc::Receiver<(Ipv4Addr, Beat)>,
    id: [u8; 8],
    within: Duration,
) -> (Ipv4Addr, Beat) {
    let end = Instant::now() + within;
    loop {
        match rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
            Ok((from, beat)) if beat.device_id == id => return (from, beat),
            Ok(_) => {}
            Err(e) => panic!("no heartbeat from our id within {within:?}: {e}"),
        }
    }
}

/// Reads `rx` until it closes; panics if it stays open for 2 s.
fn closes(rx: &std::sync::mpsc::Receiver<(Ipv4Addr, Beat)>) {
    use std::sync::mpsc::RecvTimeoutError;
    loop {
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(_) => {}
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => panic!("the channel stayed open"),
        }
    }
}

#[test]
fn the_listener_hands_on_every_heartbeat_with_its_sender_until_stopped() {
    let ip = routed_ipv4();
    let id = unique_id();
    let (listener, rx) = Listener::start(ip).expect("the heartbeat group joined");
    let tx = group_sender(ip);
    let group = SocketAddrV4::new(HEARTBEAT_GROUP, HEARTBEAT_PORT);
    // Something that is not a heartbeat comes first: it is left out.
    let mut junk = late_beat(id, 0);
    junk[0] = 0;
    tx.send_to(&junk, group).expect("sent");
    // A read times out between the beats (the thread polls its stop every
    // 0.2 s); the listener keeps going on the same socket.
    tx.send_to(&late_beat(id, 5), group).expect("sent");
    let (from, beat) = next_from(&rx, id, Duration::from_secs(5));
    assert_eq!(
        (from, beat.late, beat.lateness),
        (ip, Some(vec![5, 6]), None)
    );
    std::thread::sleep(STOP_POLL * 2);
    tx.send_to(&late_beat(id, 9), group).expect("sent");
    let (from, beat) = next_from(&rx, id, Duration::from_secs(5));
    assert_eq!((from, beat.late), (ip, Some(vec![9, 10])));
    let t = Instant::now();
    listener.stop();
    assert!(
        t.elapsed() < Duration::from_secs(1),
        "the stop took {:?}",
        t.elapsed()
    );
    closes(&rx);
}

#[test]
fn dropping_the_listener_closes_its_channel() {
    let (listener, rx) = Listener::start(routed_ipv4()).expect("the heartbeat group joined");
    drop(listener);
    closes(&rx);
}

#[test]
fn the_listener_stops_when_its_receiver_is_gone() {
    let ip = routed_ipv4();
    let (listener, rx) = Listener::start(ip).expect("the heartbeat group joined");
    drop(rx);
    // The next heartbeat finds no receiver: the thread ends on its own.
    let tx = group_sender(ip);
    let group = SocketAddrV4::new(HEARTBEAT_GROUP, HEARTBEAT_PORT);
    let end = Instant::now() + Duration::from_secs(5);
    while !listener.thread.as_ref().expect("running").is_finished() {
        assert!(
            Instant::now() < end,
            "the thread kept running without a receiver"
        );
        tx.send_to(&late_beat(unique_id(), 1), group).expect("sent");
        std::thread::sleep(Duration::from_millis(20));
    }
    listener.stop();
}
