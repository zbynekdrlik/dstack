//! dstack #504 (iem-mixer's restart, 26.11.2026 17:46:56 CET): a hardware
//! receiver asked for its three flows four times while our TX clock was
//! not ready (one destination twice, its first channel `null`: our TX
//! channel not resolved yet). The clock turned ready, the receiver's next
//! request (its channels resolved) was granted at once, and ~58 ms later
//! the expiry tick replayed the OLDER held copies after it: a request for a
//! destination already served sets that flow's channels, so the flow came
//! up `[null,3,12,14]` and its TX 6 stayed silent until a re-patch (a
//! granted receiver never asks again). Whatever the order in which the
//! held and the new requests reach the server, the newest request of a
//! destination is the one its flow keeps.

use super::*;

/// The receiver's address (its control socket and its flows').
const RECEIVER: Ipv4Addr = Ipv4Addr::new(127, 0, 44, 1);

/// A request of the receiver's shape: 4 channels, 4 frames a packet,
/// 24 bits, audio to `port` on its own address.
fn tascam(port: u16, channels: [Option<u16>; 4]) -> FlowRequest {
    FlowRequest {
        rx_host: "stage-receiver".to_string(),
        rx_flow_name: format!("flow-{port}"),
        sample_rate: 96_000,
        bits: 24,
        channels: channels.to_vec(),
        fpp: 4,
        rx: SocketAddrV4::new(RECEIVER, port),
    }
}

#[tokio::test]
async fn every_requested_channel_is_in_the_flow_after_requests_held_before_the_clock() {
    let rings: Arc<[Ring]> = Arc::from(Vec::new());
    let shared = Arc::new(TxShared::new(cfg(), Ipv4Addr::LOCALHOST, rings, 7, &[]).unwrap());
    let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server = sock.local_addr().unwrap();
    let task = tokio::spawn(serve(
        sock,
        Arc::clone(&shared),
        Arc::new(Requests::default()),
    ));
    let client = tokio::net::UdpSocket::bind((RECEIVER, 0)).await.unwrap();
    let (a, b, c) = (41_001, 41_002, 41_003);
    // Before the receiver resolved our TX 6: flow A's first channel unset.
    let stale_a = [None, Some(3), Some(12), Some(14)];
    let held = [
        tascam(a, stale_a),
        tascam(b, [Some(1), Some(2), Some(4), Some(5)]),
        tascam(c, [Some(7), Some(8), Some(9), Some(10)]),
        tascam(a, stale_a),
    ];
    for (seq, r) in (1u16..).zip(&held) {
        client
            .send_to(&build_request(seq, r), server)
            .await
            .unwrap();
    }
    let mut buf = [0u8; 2048];
    // Not ready: every request held, none answered. 250 ms sits between two
    // expiry ticks (every 100 ms from the server's start), so the fresh
    // request below comes about 50 ms before the next tick, as in the field
    // (58 ms): on the old code a late wake past the tick only lets the test
    // pass, never fails it on the fixed code (either arm replays first).
    assert!(
        tokio::time::timeout(Duration::from_millis(250), client.recv_from(&mut buf))
            .await
            .is_err(),
        "a request answered before our TX clock was ready"
    );
    // The clock turns ready and the receiver, its channels resolved, asks
    // for flow A again before the next expiry tick.
    shared.set_clock_ready(true);
    let fresh_a = [Some(6), Some(3), Some(12), Some(14)];
    client
        .send_to(&build_request(5, &tascam(a, fresh_a)), server)
        .await
        .unwrap();
    // Every request answered OK: the four held and the fresh one.
    let mut seqs = Vec::new();
    while seqs.len() < 5 {
        let (n, _) = tokio::time::timeout(Duration::from_secs(1), client.recv_from(&mut buf))
            .await
            .unwrap_or_else(|_| panic!("answered only {seqs:?}"))
            .unwrap();
        let (h, _) = Header::parse(&buf[..n]).unwrap();
        assert_eq!(h.op2, status::OK, "request {} refused", h.seq);
        seqs.push(h.seq);
    }
    seqs.sort_unstable();
    assert_eq!(seqs, [1, 2, 3, 4, 5]);
    // Past two more expiry ticks: nothing held is replayed later.
    tokio::time::sleep(EXPIRE_CHECK * 3).await;
    let flows: Vec<(u16, Vec<Option<u16>>)> = shared
        .lock()
        .table
        .flows()
        .map(|(_, f)| (f.dest.port(), f.channels.clone()))
        .collect();
    let channels_of = |port: u16| {
        flows
            .iter()
            .find(|(p, _)| *p == port)
            .map(|(_, ch)| ch.clone())
    };
    assert_eq!(flows.len(), 3, "one flow a destination: {flows:?}");
    assert_eq!(
        channels_of(a),
        Some(fresh_a.to_vec()),
        "flow A keeps its newest request's channels (TX 6 in it): {flows:?}"
    );
    assert_eq!(channels_of(b), Some(held[1].channels.clone()));
    assert_eq!(channels_of(c), Some(held[2].channels.clone()));
    task.abort();
}

/// A request held past [`HOLD`] with the clock never ready is dropped and
/// logged once with its count; a tick that drops nothing logs nothing.
#[tokio::test]
async fn a_request_held_past_the_hold_is_dropped_and_logged_once() {
    let log = crate::test_log::Captured::start(tracing::Level::INFO);
    let rings: Arc<[Ring]> = Arc::from(Vec::new());
    let shared = Arc::new(TxShared::new(cfg(), Ipv4Addr::LOCALHOST, rings, 7, &[]).unwrap());
    let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server = sock.local_addr().unwrap();
    let task = tokio::spawn(serve(
        sock,
        Arc::clone(&shared),
        Arc::new(Requests::default()),
    ));
    let client = tokio::net::UdpSocket::bind((Ipv4Addr::new(127, 0, 44, 2), 0))
        .await
        .unwrap();
    let r = FlowRequest {
        rx: SocketAddrV4::new(Ipv4Addr::new(127, 0, 44, 2), 41_004),
        ..tascam(41_004, [Some(1), Some(2), Some(3), Some(4)])
    };
    client.send_to(&build_request(1, &r), server).await.unwrap();
    // Past the hold and several ticks after it.
    tokio::time::sleep(HOLD + EXPIRE_CHECK * 5).await;
    task.abort();
    let text = log.text();
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("held flow requests dropped"))
        .collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert!(lines[0].contains("dropped=1"), "{text}");
    assert!(text.contains("a flow request held"), "{text}");
    assert_eq!(shared.lock().table.flows().count(), 0, "never granted");
}
