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
