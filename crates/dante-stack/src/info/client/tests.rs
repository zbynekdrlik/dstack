use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::ctl::Requests;
use crate::info::{Beat, Changes, Identity, Multicast, Multicaster, NetInfo, Records, serve_info};

const TEST_DEVICE: &str = include_str!("../../../../../config/test-dante-device.toml");
const WAIT: Duration = Duration::from_secs(5);

fn sparse(len: usize, set: &[(usize, &[u8])]) -> Vec<u8> {
    let mut out = vec![0u8; len];
    for (at, bytes) in set {
        out[*at..*at + bytes.len()].copy_from_slice(bytes);
    }
    out
}

/// A Brooklyn II card's product info, as captured (TEAC ML-32D).
fn captured_product() -> Vec<u8> {
    sparse(
        336,
        &[
            (0x00, b"TEACCorp"),
            (0x08, &[0, 0, 0, 0, 0x01, 0x00, 0x07, 0x97]),
            (0x18, &[1, 2, 0, 0]),
            (0x1c, &[1, 2, 0, 0]),
            (0x2c, b"TEAC Corporation"),
            (0xac, b"ML-32D"),
            (0x12c, &[1, 0, 0, 0]),
        ],
    )
}

/// An Ultimo board's info, as captured (the versions netaudio showed:
/// software 4.2.5.1, hardware 4.2.4.8, API 1.0.0, boot 2.1.0).
fn captured_board() -> Vec<u8> {
    sparse(
        200,
        &[
            (0x00, &[4, 2, 0, 5]),
            (0x04, &[4, 2, 0, 4]),
            (0x08, &[1, 0, 0, 0]),
            (0x0c, b"UltimoX"),
            (0x1b, &[0x64]),
            (0x23, &[1]),
            (0x27, &[8]),
            (0x28, &[2, 1, 0, 0]),
            (0x2f, &[1]),
            (0x38, b"UltimoX2"),
            (0xbb, &[7]),
        ],
    )
}

fn v(major: u8, minor: u8, patch: u16, build: Option<u32>) -> Option<Version> {
    Some(Version {
        major,
        minor,
        patch,
        build,
    })
}

#[test]
fn a_request_is_netaudio_s() {
    let id = [0, 0x1d, 0xc1, 8, 2, 0x14, 0, 0];
    let mut want = vec![0xff, 0xff, 0x00, 0x20, 0x0f, 0xdb, 0x00, 0x00];
    want.extend_from_slice(&id);
    want.extend_from_slice(b"Audinate");
    want.extend_from_slice(&[0x07, 0x31, 0x00, 0xc1, 0, 0, 0, 0]);
    assert_eq!(request(0x0fdb, id, Ask::Product), want);
    want[27] = 0x61;
    assert_eq!(request(0x0fdb, id, Ask::Board), want);
    // Our own server reads it as the request it is.
    let r = crate::info::parse_request(&request(7, [0; 8], Ask::Board)).unwrap();
    assert_eq!(r.kind, crate::info::Kind::BoardInfo);
    assert_eq!(r.seq, 7);
}

#[test]
fn the_captured_product_info_reads_as_netaudio_shows_it() {
    let p = parse_product(&captured_product()).unwrap();
    assert_eq!(
        p,
        Product {
            manufacturer_id: *b"TEACCorp",
            product_id: [0, 0, 0, 0, 0x01, 0x00, 0x07, 0x97],
            manufacturer: "TEAC Corporation".into(),
            model: "ML-32D".into(),
            software: v(1, 2, 0, None),
            firmware: v(1, 2, 0, None),
            version: v(1, 0, 0, None),
        }
    );
    assert_eq!(p.firmware.unwrap().to_string(), "1.2.0");
}

#[test]
fn the_captured_board_info_reads_as_netaudio_shows_it() {
    let b = parse_board(&captured_board()).unwrap();
    assert_eq!(
        b,
        Board {
            board: "UltimoX".into(),
            model: "UltimoX2".into(),
            software: v(4, 2, 5, Some(1)),
            hardware: v(4, 2, 4, Some(8)),
            api: v(1, 0, 0, None),
            boot: v(2, 1, 0, None),
        }
    );
    assert_eq!(b.software.unwrap().to_string(), "4.2.5.1");
    assert_eq!(b.hardware.unwrap().to_string(), "4.2.4.8");
    assert_eq!(b.boot.unwrap().to_string(), "2.1.0");
}

#[test]
fn a_zero_version_is_none_and_a_wide_patch_reads() {
    let mut body = captured_product();
    body[0x18..0x1c].copy_from_slice(&[0, 0, 0, 0]);
    body[0x1c..0x20].copy_from_slice(&[4, 0, 0x01, 0x02]);
    let p = parse_product(&body).unwrap();
    assert_eq!(p.software, None);
    assert_eq!(p.firmware, v(4, 0, 0x0102, None));
    assert_eq!(p.firmware.unwrap().to_string(), "4.0.258");
}

#[test]
fn a_name_fills_its_field_or_stops_at_a_zero() {
    let mut body = captured_board();
    body[0x0c..0x14].copy_from_slice(b"ABCDEFGH");
    body[0x14] = b'X';
    assert_eq!(parse_board(&body).unwrap().board, "ABCDEFGH");
    let mut body = captured_product();
    body[0xac..0x12c].fill(b'M');
    assert_eq!(parse_product(&body).unwrap().model, "M".repeat(0x80));
}

#[test]
fn a_short_body_does_not_read() {
    let p = captured_product();
    assert!(parse_product(&p[..0x130]).is_some());
    assert_eq!(parse_product(&p[..0x12f]), None);
    let b = captured_board();
    assert!(parse_board(&b[..0xb8]).is_some());
    assert_eq!(parse_board(&b[..0xb7]), None);
}

fn reply(op: u8, id: [u8; 8], body: &[u8]) -> Vec<u8> {
    let mut out = vec![0xff, 0xff, 0, 0, 0, 1, 0, 0];
    out.extend_from_slice(&id);
    out.extend_from_slice(b"Audinate");
    out.extend_from_slice(&[0x07, 0x2e, 0x00, op, 0, 0, 0, 0]);
    out.extend_from_slice(body);
    out
}

#[test]
fn only_the_asked_reply_from_the_asked_device_is_taken() {
    let id = [1, 2, 3, 4, 5, 6, 0, 0];
    let d = reply(0xc0, id, &[9, 9]);
    assert_eq!(reply_body(&d, Ask::Product, None), Some(&[9u8, 9][..]));
    assert_eq!(reply_body(&d, Ask::Product, Some(id)), Some(&[9u8, 9][..]));
    assert_eq!(reply_body(&d, Ask::Product, Some([0; 8])), None);
    assert_eq!(reply_body(&d, Ask::Board, None), None);
    assert_eq!(
        reply_body(&reply(0x60, id, &[]), Ask::Board, None),
        Some(&[][..])
    );
    // The request itself (0xc1), a heartbeat start code, a set flag byte
    // and a short datagram are not replies.
    assert_eq!(reply_body(&reply(0xc1, id, &[]), Ask::Product, None), None);
    let mut beat = d.clone();
    beat[1] = 0xfe;
    assert_eq!(reply_body(&beat, Ask::Product, None), None);
    for at in [24, 26, 28, 31] {
        let mut bad = d.clone();
        bad[at] ^= 0x01;
        assert_eq!(reply_body(&bad, Ask::Product, None), None, "byte {at}");
    }
    assert_eq!(reply_body(&d[..31], Ask::Product, None), None);
}

fn identity() -> Identity {
    let cfg = crate::device::DeviceConfig::from_toml(TEST_DEVICE).unwrap();
    Identity::new(
        &cfg,
        NetInfo {
            mac: [2, 0, 0, 0, 0, 1],
            ip: Ipv4Addr::LOCALHOST,
            netmask: Ipv4Addr::new(255, 0, 0, 0),
            gateway: Ipv4Addr::UNSPECIFIED,
            link_speed: 1000,
        },
    )
}

/// A device whose media clock is not ready.
struct NoClock;

impl Beat for NoClock {
    fn records(&self) -> Option<Records> {
        None
    }
}

/// Our own info server on loopback, its info replies sent to `client`
/// (standing in for the group); its address.
fn our_server(client: &UdpSocket) -> (tokio::runtime::Runtime, std::net::SocketAddr) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let info = match client.local_addr().unwrap() {
        std::net::SocketAddr::V4(a) => a,
        _ => unreachable!(),
    };
    let sock = rt
        .block_on(tokio::net::UdpSocket::bind("127.0.0.1:0"))
        .unwrap();
    let at = sock.local_addr().unwrap();
    rt.spawn(serve_info(
        sock,
        Multicaster::new(identity()),
        Multicast {
            info,
            heartbeat: "127.0.0.1:9".parse().unwrap(),
            every: Duration::from_secs(3600),
        },
        Arc::new(NoClock),
        Arc::new(Changes::default()),
        Arc::new(Requests::default()),
    ));
    (rt, at)
}

#[test]
fn our_own_server_s_answers_read_back() {
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    let (rt, at) = our_server(&client);
    let port = at.port();
    let _keep = std::thread::spawn(move || rt.block_on(std::future::pending::<()>()));
    let body = ask_at(&client, port, Ask::Product);
    let p = parse_product(&body).unwrap();
    assert_eq!(p.manufacturer, "iemmixer");
    assert_eq!(p.model, "iem-dante");
    let ver = env!("CARGO_PKG_VERSION");
    assert_eq!(p.firmware.unwrap().to_string(), ver);
    assert_eq!(p.version.unwrap().to_string(), ver);
    let b = parse_board(&ask_at(&client, port, Ask::Board)).unwrap();
    assert_eq!(b.board, "iem-dant");
    assert_eq!(b.model, "iem-dante");
    assert_eq!(b.software, v(4, 1, 6, Some(2)));
    assert_eq!(b.hardware, v(4, 1, 3, Some(1)));
    assert_eq!(b.boot, v(1, 0, 0, None));
}

/// [`ask`] to our server's port on loopback.
fn ask_at(client: &UdpSocket, port: u16, a: Ask) -> Vec<u8> {
    ask(
        client,
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, port),
        None,
        a,
        WAIT,
    )
    .unwrap()
}

#[test]
fn no_reply_is_a_timeout() {
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    let quiet = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = quiet.local_addr().unwrap().port();
    let started = std::time::Instant::now();
    let got = ask(
        &client,
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, port),
        None,
        Ask::Board,
        Duration::from_millis(200),
    );
    assert_eq!(got, Err(InfoError::Timeout { ask: Ask::Board }));
    assert!(started.elapsed() >= Duration::from_millis(200));
}

#[test]
fn another_device_s_reply_is_skipped() {
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    let dev = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = dev.local_addr().unwrap().port();
    let want = [7u8; 8];
    let to = client.local_addr().unwrap();
    let t = std::thread::spawn(move || {
        let mut buf = [0u8; 64];
        let (n, _) = dev.recv_from(&mut buf).unwrap();
        assert_eq!(n, 32);
        dev.send_to(&reply(0xc0, [1; 8], &[1]), to).unwrap();
        dev.send_to(&reply(0x60, want, &[2]), to).unwrap();
        dev.send_to(&reply(0xc0, want, &[3]), to).unwrap();
    });
    let got = ask(
        &client,
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, port),
        Some(want),
        Ask::Product,
        WAIT,
    );
    t.join().unwrap();
    assert_eq!(got, Ok(vec![3]));
}

#[test]
fn a_reply_from_another_address_is_skipped() {
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    let dev = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = dev.local_addr().unwrap().port();
    let to = client.local_addr().unwrap();
    let other = UdpSocket::bind("127.0.0.2:0").unwrap();
    let t = std::thread::spawn(move || {
        let mut buf = [0u8; 64];
        dev.recv_from(&mut buf).unwrap();
        other.send_to(&reply(0xc0, [0; 8], &[1]), to).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        dev.send_to(&reply(0xc0, [0; 8], &[2]), to).unwrap();
    });
    let got = ask(
        &client,
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, port),
        None,
        Ask::Product,
        WAIT,
    );
    t.join().unwrap();
    assert_eq!(got, Ok(vec![2]));
}

#[test]
fn a_read_timeout_s_end_is_a_timeout_and_any_other_error_a_socket_error() {
    use std::io::{Error, ErrorKind};
    for kind in [ErrorKind::WouldBlock, ErrorKind::TimedOut] {
        assert_eq!(
            recv_error(Ask::Product, &Error::from(kind)),
            InfoError::Timeout { ask: Ask::Product }
        );
    }
    assert_eq!(
        recv_error(Ask::Board, &Error::from(ErrorKind::ConnectionReset)),
        InfoError::Io {
            ask: Ask::Board,
            kind: ErrorKind::ConnectionReset
        }
    );
}

/// A Brooklyn II follower's clock status, captured (its id, its master's
/// and the grandmaster's replaced by synthetic ones): disciplined,
/// synchronized, stratum 159, -2443 ppb, one port following, word clock
/// none.
const FOLLOWER_CLOCK: [u8; 188] = [
    0x00, 0x03, 0x00, 0x03, 0x00, 0x00, 0x00, 0x9f, 0xff, 0xff, 0xf6, 0x75, 0x02, 0x00, 0x00, 0x00,
    0x00, 0x02, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
    0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x34, 0x00, 0x09, 0x00, 0x00, 0x02, 0x94, 0x00, 0x00,
    0x00, 0x03, 0x0d, 0x40, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x60, 0x0c, 0x00,
    0x00, 0x00, 0x00, 0x0c, 0x00, 0x98, 0x00, 0x20, 0x00, 0x03, 0x00, 0x00, 0x00, 0x68, 0x10, 0x00,
    0x00, 0x00, 0x00, 0x01, 0x01, 0x02, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x09, 0x00, 0x07,
    0x00, 0x01, 0x00, 0x02, 0x02, 0x02, 0x02, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x03, 0x00, 0x03,
    0x00, 0x01, 0x00, 0x03, 0x02, 0x02, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x03, 0x00, 0x07,
    0x00, 0x03, 0x00, 0x07, 0x00, 0xb8, 0x00, 0x04, 0x02, 0x00, 0x00, 0xff, 0xfe, 0x00, 0x00, 0x02,
    0x02, 0x00, 0x00, 0xff, 0xfe, 0x00, 0x00, 0x01, 0x02, 0x00, 0x00, 0xff, 0xfe, 0x00, 0x00, 0x01,
    0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
];

/// The leader card's clock status, captured (its id synthetic):
/// undisciplined, servo none, stratum 121, -7 ppb, two ports leading,
/// word clock unknown.
const LEADER_CLOCK: [u8; 156] = [
    0x00, 0x02, 0x00, 0x06, 0x00, 0x00, 0x01, 0x79, 0xff, 0xff, 0xff, 0xf9, 0x02, 0x00, 0x00, 0x00,
    0x00, 0x01, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
    0x00, 0x01, 0x00, 0x00, 0x00, 0x02, 0x00, 0x38, 0x00, 0x06, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00,
    0x02, 0xb4, 0x00, 0x00, 0x00, 0x03, 0x0d, 0x40, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x5c, 0x00, 0x04, 0x00, 0x04, 0x00, 0x00, 0x00, 0x64, 0x00, 0x10, 0x00, 0x00, 0x00, 0x01,
    0x01, 0x02, 0x01, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x06, 0x00, 0x07, 0x00, 0x00, 0x00, 0x02,
    0x01, 0x02, 0x01, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x06, 0x00, 0x07, 0x00, 0x01, 0x00, 0x03,
    0x01, 0x02, 0x02, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x03, 0x00, 0x03, 0x00, 0x01, 0x00, 0x04,
    0x02, 0x02, 0x02, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x03, 0x00, 0x03,
];

const GM: [u8; 6] = [2, 0, 0, 0, 0, 1];

#[test]
fn the_clock_request_is_netaudio_s_and_our_server_knows_it() {
    let r = request(0x0c, [0; 8], Ask::Clock);
    assert_eq!(r.len(), 92);
    assert_eq!(r[2..4], [0, 92]);
    assert_eq!(r[24..32], [0x07, 0x31, 0x00, 0x21, 0, 0, 0, 0x64]);
    assert!(r[32..].iter().all(|&b| b == 0));
    let parsed = crate::info::parse_request(&r).unwrap();
    assert_eq!(parsed.kind, crate::info::Kind::ClockStats);
    let reply = reply(0x20, [0; 8], &[1]);
    assert_eq!(reply_body(&reply, Ask::Clock, None), Some(&[1u8][..]));
    assert_eq!(reply_body(&reply, Ask::Board, None), None);
}

#[test]
fn a_follower_s_clock_status_reads_as_netaudio_decodes_it() {
    let c = parse_clock(&FOLLOWER_CLOCK).unwrap();
    assert_eq!(
        c,
        ClockStatus {
            clock_state: 3,
            servo_state: 3,
            stratum: 159,
            frequency_ppb: -2443,
            device: [2, 0, 0, 0, 0, 2],
            master: GM,
            grandmaster: GM,
            ports: vec![9],
            capabilities: 660,
            max_drift_ppb: 200_000,
            word_clock: Some(1),
        }
    );
    assert_eq!(
        (c.clock_state_name(), c.servo_state_name()),
        (Some("disciplined"), Some("synchronized"))
    );
    assert!(!c.is_leader());
    assert!(!c.is_grandmaster());
}

#[test]
fn the_leader_s_clock_status_reads_as_netaudio_decodes_it() {
    let c = parse_clock(&LEADER_CLOCK).unwrap();
    assert_eq!(
        c,
        ClockStatus {
            clock_state: 2,
            servo_state: 6,
            stratum: 121,
            frequency_ppb: -7,
            device: GM,
            master: GM,
            grandmaster: GM,
            ports: vec![6, 6],
            capabilities: 692,
            max_drift_ppb: 200_000,
            word_clock: Some(0),
        }
    );
    assert_eq!(
        (c.clock_state_name(), c.servo_state_name()),
        (Some("undisciplined"), Some("none"))
    );
    assert!(c.is_leader());
    assert!(c.is_grandmaster());
}

#[test]
fn unknown_codes_have_no_name_and_the_port_names_are_the_seen_ones() {
    let mut body = FOLLOWER_CLOCK;
    body[1] = 7;
    body[3] = 9;
    let c = parse_clock(&body).unwrap();
    assert_eq!((c.clock_state_name(), c.servo_state_name()), (None, None));
    let names: Vec<_> = [3, 6, 9, 1].into_iter().map(port_state_name).collect();
    assert_eq!(
        names,
        [Some("disabled"), Some("leader"), Some("follower"), None]
    );
}

#[test]
fn a_short_clock_status_does_not_read() {
    // The last fixed field, the maximum drift, ends 8 bytes after the
    // ports: 0x28 + 4 (one port) + 8.
    assert!(parse_clock(&FOLLOWER_CLOCK[..0x34]).is_some());
    assert_eq!(parse_clock(&FOLLOWER_CLOCK[..0x33]), None);
    // A port count past the body.
    let mut body = FOLLOWER_CLOCK;
    body[0x25] = 40;
    assert_eq!(parse_clock(&body), None);
    // An extension past the body: no word clock, the rest reads.
    let c = parse_clock(&FOLLOWER_CLOCK[..0x40]).unwrap();
    assert_eq!(c.word_clock, None);
    assert_eq!(c.max_drift_ppb, 200_000);
}

#[test]
fn a_device_that_drops_the_first_request_answers_the_resend() {
    // dcontroller on the site (1.11.2026): Brooklyn cards left a board-info
    // request asked right after the product info unanswered; netaudio sent
    // it three times, and the reply came after the third.
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    let dev = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = dev.local_addr().unwrap().port();
    let to = client.local_addr().unwrap();
    let t = std::thread::spawn(move || {
        dev.set_read_timeout(Some(WAIT)).unwrap();
        let mut buf = [0u8; 128];
        let mut asked = 0;
        while let Ok((n, _)) = dev.recv_from(&mut buf) {
            assert_eq!(&buf[24..32], &request(1, [0; 8], Ask::Board)[24..32]);
            asked += 1;
            if asked == 2 {
                dev.send_to(&reply(0x60, [0; 8], &[5]), to).unwrap();
                return (asked, n);
            }
        }
        (asked, 0)
    });
    let got = ask(
        &client,
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, port),
        None,
        Ask::Board,
        Duration::from_secs(1),
    );
    let (asked, n) = t.join().unwrap();
    assert_eq!(got, Ok(vec![5]));
    assert_eq!((asked, n), (2, 32));
}

#[test]
fn a_silent_device_is_asked_once_every_resend() {
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    let dev = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = dev.local_addr().unwrap().port();
    // 600 ms: the request at 0, 250 and 500 ms, never more.
    let got = ask(
        &client,
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, port),
        None,
        Ask::Product,
        Duration::from_millis(600),
    );
    assert_eq!(got, Err(InfoError::Timeout { ask: Ask::Product }));
    dev.set_nonblocking(true).unwrap();
    let mut buf = [0u8; 64];
    let mut asked = 0;
    while dev.recv_from(&mut buf).is_ok() {
        asked += 1;
    }
    assert_eq!(asked, 3);
    assert_eq!(RESEND, Duration::from_millis(250));
}

#[test]
fn every_request_carries_a_new_sequence_so_a_card_answers_the_next_ask() {
    // dcontroller on the site (1.11.2026), with the resend in place: the
    // Brooklyn cards still left the board info unanswered after the product
    // info. netaudio's trace: all its requests carried one sequence, and the
    // card's two replies were consecutive packets, the board info only after
    // the third request: a card drops a request whose sequence it has just
    // answered. Here the stand-in answers only a sequence it has not seen.
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    let dev = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = dev.local_addr().unwrap().port();
    let to = client.local_addr().unwrap();
    let t = std::thread::spawn(move || {
        dev.set_read_timeout(Some(Duration::from_millis(1500)))
            .unwrap();
        let mut seen = Vec::new();
        let mut buf = [0u8; 128];
        let mut answered = 0;
        while let Ok((_, _)) = dev.recv_from(&mut buf) {
            let seq = u16::from_be_bytes([buf[4], buf[5]]);
            if seen.contains(&seq) {
                continue;
            }
            seen.push(seq);
            let op = if buf[27] == 0xc1 { 0xc0 } else { 0x60 };
            dev.send_to(&reply(op, [0; 8], &[op]), to).unwrap();
            answered += 1;
            if answered == 2 {
                return;
            }
        }
    });
    let at = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    let wait = Duration::from_millis(600);
    let product = ask(&client, at, None, Ask::Product, wait);
    let board = ask(&client, at, None, Ask::Board, wait);
    t.join().unwrap();
    assert_eq!(product, Ok(vec![0xc0]));
    assert_eq!(board, Ok(vec![0x60]));
}

/// A two-port card's network info in the captured layout, with synthetic
/// MACs and addresses: the primary at 1000 Mbit/s (flags 1), the secondary
/// at 1000 Mbit/s (flags 3) with no gateway, then the trailing blocks.
fn two_port_network() -> Vec<u8> {
    sparse(
        0x70,
        &[
            (0x00, &[0, 2]),
            (0x06, &[0x03, 0xe8, 0x00, 0x01]),
            (0x0a, &[2, 0, 0, 0, 0, 1]),
            (
                0x10,
                &[192, 0, 2, 10, 255, 255, 255, 0, 192, 0, 2, 1, 192, 0, 2, 1],
            ),
            (0x22, &[0x03, 0xe8, 0x00, 0x03]),
            (0x26, &[2, 0, 0, 0, 0, 2]),
            (0x2c, &[198, 51, 100, 10, 255, 255, 254, 0]),
            (0x3c, &[0x00, 0x18, 0x00, 0x4c]),
        ],
    )
}

#[test]
fn the_network_request_is_netaudio_s_and_our_server_knows_it() {
    let r = request(9, [0; 8], Ask::Network);
    assert_eq!(r.len(), 40);
    assert_eq!(r[24..32], [0x07, 0x31, 0x00, 0x13, 0, 0, 0, 0x64]);
    assert!(r[32..].iter().all(|&b| b == 0));
    let parsed = crate::info::parse_request(&r).unwrap();
    assert_eq!(parsed.kind, crate::info::Kind::NetworkInfo);
    assert_eq!(
        reply_body(&reply(0x11, [0; 8], &[1]), Ask::Network, None),
        Some(&[1u8][..])
    );
}

#[test]
fn a_two_port_card_s_network_info_reads_as_netaudio_decodes_it() {
    let n = parse_network(&two_port_network()).unwrap();
    assert_eq!(
        n,
        [
            NetInterface {
                link_mbit: 1000,
                flags: 1,
                mac: [2, 0, 0, 0, 0, 1],
                ip: Ipv4Addr::new(192, 0, 2, 10),
                netmask: Ipv4Addr::new(255, 255, 255, 0),
                gateway: Ipv4Addr::new(192, 0, 2, 1),
                dns: Ipv4Addr::new(192, 0, 2, 1),
            },
            NetInterface {
                link_mbit: 1000,
                flags: 3,
                mac: [2, 0, 0, 0, 0, 2],
                ip: Ipv4Addr::new(198, 51, 100, 10),
                netmask: Ipv4Addr::new(255, 255, 254, 0),
                gateway: Ipv4Addr::UNSPECIFIED,
                dns: Ipv4Addr::UNSPECIFIED,
            },
        ]
    );
    // The count passing the body, and a short body.
    let mut more = two_port_network();
    more[1] = 5;
    assert_eq!(parse_network(&more), None);
    assert_eq!(parse_network(&two_port_network()[..0x3c]).unwrap().len(), 2);
    assert_eq!(parse_network(&two_port_network()[..0x3b]), None);
    assert_eq!(parse_network(&[0, 0]), Some(vec![]));
}

#[test]
fn our_own_server_s_network_info_reads_back() {
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    let (rt, at) = our_server(&client);
    let port = at.port();
    let _keep = std::thread::spawn(move || rt.block_on(std::future::pending::<()>()));
    let n = parse_network(&ask_at(&client, port, Ask::Network)).unwrap();
    assert_eq!(
        n,
        [NetInterface {
            link_mbit: 1000,
            flags: 1,
            mac: [2, 0, 0, 0, 0, 1],
            ip: Ipv4Addr::LOCALHOST,
            netmask: Ipv4Addr::new(255, 0, 0, 0),
            gateway: Ipv4Addr::UNSPECIFIED,
            dns: Ipv4Addr::UNSPECIFIED,
        }]
    );
}

/// A Brooklyn card's sample-rate reply, captured: 96 kHz in effect, none
/// pending, four rates.
const CARD_RATE: [u8; 32] = [
    0x00, 0x18, 0x00, 0x04, 0x00, 0x01, 0x77, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00,
    0x00, 0x00, 0xac, 0x44, 0x00, 0x00, 0xbb, 0x80, 0x00, 0x01, 0x58, 0x88, 0x00, 0x01, 0x77, 0x00,
];

/// An Ultimo's, captured: 96 kHz in effect and pending, two rates.
const ULTIMO_RATE: [u8; 24] = [
    0x00, 0x18, 0x00, 0x02, 0x00, 0x01, 0x77, 0x00, 0x00, 0x01, 0x77, 0x00, 0x00, 0x02, 0x00, 0x00,
    0x00, 0x00, 0xbb, 0x80, 0x00, 0x01, 0x77, 0x00,
];

#[test]
fn the_rate_and_encoding_requests_carry_netaudio_s_bodies() {
    // netaudio 0.4.2's encoder, offline: set_sample_rate 48000 ends
    // `07 27 00 81 00 00 00 64 | 00000001 0000bb80`, set_encoding 24
    // `.. 00 83 .. 64 | 00000001 00000018`, the probe `.. 00 81 .. 64` and
    // eight zeros (byte 1 of the opcode is any).
    let w = request(3, [0; 8], Ask::SetSampleRate(48_000));
    assert_eq!(w.len(), 40);
    assert_eq!(w[2..4], [0, 40]);
    assert_eq!(w[24..32], [0x07, 0x31, 0x00, 0x81, 0, 0, 0, 0x64]);
    assert_eq!(w[32..40], [0, 0, 0, 1, 0, 0, 0xbb, 0x80]);
    let e = request(3, [0; 8], Ask::SetEncoding(24));
    assert_eq!(e[24..32], [0x07, 0x31, 0x00, 0x83, 0, 0, 0, 0x64]);
    assert_eq!(e[32..40], [0, 0, 0, 1, 0, 0, 0, 24]);
    let r = request(3, [0; 8], Ask::SampleRate);
    assert_eq!(r[24..32], [0x07, 0x31, 0x00, 0x81, 0, 0, 0, 0x64]);
    assert_eq!(r.len(), 40);
    assert!(r[32..].iter().all(|&b| b == 0));
    // The rate's reply is 0x80; an encoding write has none to match.
    let rate = reply(0x80, [0; 8], &[1]);
    assert_eq!(reply_body(&rate, Ask::SampleRate, None), Some(&[1u8][..]));
    assert_eq!(
        reply_body(&rate, Ask::SetSampleRate(48_000), None),
        Some(&[1u8][..])
    );
    assert_eq!(
        reply_body(&reply(0x00, [0; 8], &[]), Ask::SetEncoding(24), None),
        None
    );
}

#[test]
fn the_captured_rate_replies_read() {
    assert_eq!(
        parse_sample_rate(&CARD_RATE),
        Some(SampleRate {
            current: 96_000,
            pending: None,
            supported: vec![44_100, 48_000, 88_200, 96_000],
        })
    );
    assert_eq!(
        parse_sample_rate(&ULTIMO_RATE),
        Some(SampleRate {
            current: 96_000,
            pending: Some(96_000),
            supported: vec![48_000, 96_000],
        })
    );
    assert_eq!(parse_sample_rate(&CARD_RATE[..31]), None);
    assert_eq!(
        parse_sample_rate(&CARD_RATE[..0x0c]).map(|s| s.current),
        None
    );
}

/// A stand-in device on loopback answering rate reads with its rate and
/// taking a rate write when `takes`; its address.
fn rate_device(takes: bool) -> (SocketAddrV4, std::thread::JoinHandle<u32>) {
    let dev = UdpSocket::bind("127.0.0.1:0").unwrap();
    let at = match dev.local_addr().unwrap() {
        std::net::SocketAddr::V4(a) => a,
        _ => unreachable!(),
    };
    let t = std::thread::spawn(move || {
        dev.set_read_timeout(Some(Duration::from_millis(1500)))
            .unwrap();
        let mut rate = 96_000u32;
        let mut buf = [0u8; 128];
        while let Ok((n, from)) = dev.recv_from(&mut buf) {
            assert_eq!(n, 40);
            let mode = u32::from_be_bytes(buf[32..36].try_into().unwrap());
            if mode == 1 {
                if takes {
                    rate = u32::from_be_bytes(buf[36..40].try_into().unwrap());
                }
                continue;
            }
            let mut body = CARD_RATE;
            body[4..8].copy_from_slice(&rate.to_be_bytes());
            dev.send_to(&reply(0x80, [0; 8], &body), from).unwrap();
        }
        rate
    });
    (at, t)
}

#[test]
fn a_rate_write_is_confirmed_by_reading_it_back() {
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    let (at, t) = rate_device(true);
    send_write(&client, at, Ask::SetSampleRate(48_000)).unwrap();
    let got = confirm_rate(&client, at, None, 48_000, Duration::from_secs(1));
    assert_eq!(got.map(|s| s.current), Ok(48_000));
    assert_eq!(t.join().unwrap(), 48_000);
}

#[test]
fn a_rate_write_the_device_ignores_is_not_confirmed() {
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    let (at, t) = rate_device(false);
    send_write(&client, at, Ask::SetSampleRate(48_000)).unwrap();
    let started = std::time::Instant::now();
    let got = confirm_rate(&client, at, None, 48_000, Duration::from_millis(600));
    assert_eq!(
        got,
        Err(InfoError::NotConfirmed {
            ask: Ask::SetSampleRate(48_000)
        })
    );
    assert!(started.elapsed() >= Duration::from_millis(600));
    assert_eq!(t.join().unwrap(), 96_000);
}
