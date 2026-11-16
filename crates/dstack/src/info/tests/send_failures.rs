//! The info server's multicast sends failing for a long time (dstack
//! #340): on dsg (Windows, 15.-16.11.2026) every heartbeat and info packet
//! failed with WSAEADDRNOTAVAIL (10049) for ~24 h, logged as one WARN a
//! packet (11 678 lines). A sustained failure is counted and logged at
//! most once a minute, with the count and the OS error.

use super::*;

use std::io;
use std::time::Instant;

use crate::info::sendwatch::REOPEN_AT;
use crate::test_log::Captured;

/// Where a send always fails: the broadcast address without
/// `SO_BROADCAST` (EACCES, os error 13; WSAEACCES, 10013 on Windows), as
/// each send failed on dsg.
const REFUSED: &str = "255.255.255.255:9";
const REFUSED_ERROR: &str = if cfg!(windows) {
    "os error 10013"
} else {
    "os error 13"
};

#[tokio::test]
async fn a_sustained_send_failure_is_one_warning_not_one_a_packet() {
    let log = Captured::start(tracing::Level::WARN);
    let (_at, info_rx, _beat_rx, mut to, sock) = loopback(Duration::from_millis(5)).await;
    to.heartbeat = REFUSED.parse().unwrap();
    let task = tokio::spawn(serve_info(
        sock,
        InfoBuilder::new(identity()),
        to,
        Arc::new(NoClock),
        Arc::new(Changes::default()),
        Arc::new(Requests::default()),
    ));
    // The board and product info still go out.
    recv(&info_rx).await;
    recv(&info_rx).await;
    // A heartbeat every 5 ms for 300 ms: dozens of refused sends.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!task.is_finished(), "the server goes on: {:?}", task.await);
    task.abort();
    let text = log.text();
    // The sends' warnings only: on Windows the loopback address may refuse
    // the multicast interface's re-naming, a warning of its own.
    let warnings = text
        .lines()
        .filter(|l| l.contains("WARN") && l.contains("could not be sent"))
        .count();
    assert_eq!(warnings, 1, "one warning, not one a packet:\n{text}");
    assert!(
        text.contains(REFUSED_ERROR),
        "the OS error is named:\n{text}"
    );
}

/// WSAEADDRNOTAVAIL, as every send failed on dsg.
const WSAEADDRNOTAVAIL: i32 = 10049;

/// A send that fails with WSAEADDRNOTAVAIL the first `n` times, then sends.
struct FailFirst {
    left: std::sync::atomic::AtomicU32,
}

impl FailFirst {
    fn new(n: u32) -> Self {
        FailFirst { left: n.into() }
    }

    async fn send(&self, sock: &UdpSocket, packet: &[u8], to: SocketAddrV4) -> io::Result<usize> {
        let failing = self
            .left
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
            .is_ok();
        if failing {
            return Err(io::Error::from_raw_os_error(WSAEADDRNOTAVAIL));
        }
        sock.send_to(packet, to).await
    }
}

/// `requests`' info send counts (errors, recoveries) once `errors` sends
/// failed, within the bound.
async fn sends_failed(requests: &Requests, errors: u64) -> (u64, u64) {
    let start = Instant::now();
    loop {
        let c = requests.counts();
        if c.info_send_errors >= errors {
            return (c.info_send_errors, c.info_send_recoveries);
        }
        assert!(
            start.elapsed() < BOUND,
            "{} of {errors} failures",
            c.info_send_errors
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn ctx(to: Multicast, requests: &Arc<Requests>) -> server::Ctx {
    server::Ctx {
        to,
        beat: Arc::new(NoClock),
        changes: Arc::new(Changes::default()),
        requests: Arc::clone(requests),
    }
}

/// Each wiring test serves on a loopback address of its own (Linux answers
/// on all of 127/8), so the info port 8700 and the share port are its alone.
#[tokio::test]
async fn sends_failing_with_10049_name_the_interface_again_then_reopen_and_beats_come_back() {
    let log = Captured::start(tracing::Level::INFO);
    let ip = Ipv4Addr::new(127, 0, 34, 1);
    let (_, info_rx, beat_rx, to, _) = loopback(Duration::from_millis(5)).await;
    let requests = Arc::new(Requests::default());
    let ctx = ctx(to, &requests);
    let failing = REOPEN_AT + 2;
    let fail = FailFirst::new(failing);
    let send = async |s: &UdpSocket, p: &[u8], t: SocketAddrV4| fail.send(s, p, t).await;
    let own = UdpSocket::bind(SocketAddrV4::new(ip, REQUEST_PORT))
        .await
        .unwrap();
    let task = server::serve_shared_by(ip, Some(own), InfoBuilder::new(identity()), &ctx, &send);
    let check = async {
        let (errors, recoveries) = sends_failed(&requests, failing.into()).await;
        assert_eq!(errors, u64::from(failing), "every refused send counted");
        assert_eq!(recoveries, 2, "the interface named again, then a reopen");
        // The beats come back, from the reopened socket on the same address.
        let mut buf = [0u8; 2048];
        let (_, from) = tokio::time::timeout(BOUND, beat_rx.recv_from(&mut buf))
            .await
            .expect("a heartbeat after the failures")
            .unwrap();
        assert_eq!(from, SocketAddr::from(SocketAddrV4::new(ip, REQUEST_PORT)));
        // The reopened socket answers requests to the info port.
        let peer = UdpSocket::bind((ip, 0)).await.unwrap();
        peer.send_to(&captured(), (ip, REQUEST_PORT)).await.unwrap();
        let reply = recv(&info_rx).await;
        assert_eq!(id_of(&reply), identity().device_id);
    };
    tokio::select! {
        ended = task => panic!("the server ended: {ended:?}"),
        () = check => {}
    }
    assert_eq!(requests.counts().info_send_errors, u64::from(failing));
    let text = log.text();
    // One line for every failure (on Windows a loopback alias may refuse
    // the multicast interface: that is another warning, not a send's).
    assert_eq!(
        text.matches("info packets could not be sent").count(),
        1,
        "one warning for every failure:\n{text}"
    );
    assert!(text.contains("os error 10049"), "the OS error:\n{text}");
    // The interface named again once, whether the OS took it or not.
    assert_eq!(
        text.lines().filter(|l| l.contains("named again")).count(),
        1,
        "{text}"
    );
    assert_eq!(
        text.matches("the info socket reopened").count(),
        1,
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "info multicast sends work again failures={failing}"
        )),
        "the outage's end:\n{text}"
    );
}

/// Unix only: the other device shares the port by `SO_REUSEPORT`, which
/// Windows has not (its `SO_REUSEADDR` lets a later bind take the port).
#[cfg(unix)]
#[tokio::test]
async fn a_port_taken_during_the_reopen_makes_the_device_a_member() {
    let ip = Ipv4Addr::new(127, 0, 34, 2);
    let (_, _info_rx, _beat_rx, to, _) = loopback(Duration::from_millis(5)).await;
    let requests = Arc::new(Requests::default());
    let ctx = ctx(to, &requests);
    // Our port and another device's on it at once (SO_REUSEPORT on both):
    // our plain bind at the reopen is refused while the other holds it.
    let shared = || {
        let s = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )
        .unwrap();
        s.set_reuse_port(true).unwrap();
        s.set_nonblocking(true).unwrap();
        s.bind(&SocketAddrV4::new(ip, REQUEST_PORT).into()).unwrap();
        UdpSocket::from_std(s.into()).unwrap()
    };
    let own = shared();
    let _other = shared();
    let fail = FailFirst::new(u32::MAX);
    let send = async |s: &UdpSocket, p: &[u8], t: SocketAddrV4| fail.send(s, p, t).await;
    let task = server::serve_shared_by(ip, Some(own), InfoBuilder::new(identity()), &ctx, &send);
    // Once the device left its manager's loopback port, it registers there
    // as a member of whoever holds the info port now.
    let check = async {
        sends_failed(&requests, REOPEN_AT.into()).await;
        let share_at = (Ipv4Addr::LOCALHOST, share::share_port(ip));
        let start = Instant::now();
        let manager = loop {
            if let Ok(s) = UdpSocket::bind(share_at).await {
                break s;
            }
            assert!(start.elapsed() < BOUND, "the share port never freed");
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        // Its start went before the port was ours; the lease comes again
        // within a second.
        let mut buf = [0u8; 2048];
        loop {
            let (n, _) = tokio::time::timeout(BOUND, manager.recv_from(&mut buf))
                .await
                .expect("the member registers")
                .unwrap();
            if let Some(share::Msg::Register(id)) = share::Msg::decode(&buf[..n]) {
                assert_eq!(id, identity().device_id);
                break;
            }
        }
    };
    tokio::select! {
        ended = task => panic!("the server ended: {ended:?}"),
        () = check => {}
    }
}
