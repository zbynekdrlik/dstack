// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

use std::net::UdpSocket;
use std::sync::Arc;
use std::time::Duration;

use crate::device::DeviceConfig;

const TEST_DEVICE: &str = include_str!("../../../../../../config/test-device.toml");

fn shared() -> Arc<TxShared> {
    let cfg = DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-device.toml");
    Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, Vec::new().into(), 1, &[]).unwrap())
}

fn clock() -> PassClock {
    PassClock {
        now_ns: 1_000_000_000,
        pass: Instant::now(),
        rate: 96_000,
        margin_ns: 0,
        latency_ns: 1_000_000,
        relayed: false,
        escaped_ns: None,
    }
}

/// A receiver's socket on loopback and its address.
fn receiver() -> (UdpSocket, SocketAddrV4) {
    let s = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    s.set_nonblocking(true).unwrap();
    let std::net::SocketAddr::V4(at) = s.local_addr().unwrap() else {
        panic!("IPv4");
    };
    (s, at)
}

/// Every datagram's first byte waiting on `s`.
fn firsts(s: &UdpSocket) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 64];
    while let Ok(n) = s.recv(&mut buf) {
        assert_eq!(n, 3);
        out.push(buf[0]);
    }
    out
}

/// Sends every published packet of `o`, reading `rx` every 64 sends (a
/// loopback socket holds about 270 small datagrams); what `rx` got.
fn send_all_read(o: &Outbox, shared: &TxShared, rx: &UdpSocket) -> Vec<u8> {
    let mut got = Vec::new();
    let mut n = 0;
    while o.send_next(shared) {
        n += 1;
        if n % 64 == 0 {
            got.extend(firsts(rx));
        }
    }
    got.extend(firsts(rx));
    got
}

/// Queues a 3-byte datagram starting with `k` to `dest` on slot `slot`.
fn push(o: &Outbox, slot: usize, k: u8, dest: SocketAddrV4) -> bool {
    o.push(&clock(), slot, u64::from(k), 32, dest, |buf| {
        buf[..3].copy_from_slice(&[k, 1, 2]);
        3
    })
}

#[test]
fn a_new_queue_is_served_by_no_one_until_a_thread_takes_it_once() {
    let o = Outbox::new();
    assert!(!o.serving());
    assert_eq!(o.view(), OutboxView::default());
    assert!(o.serve_here());
    assert!(o.serving());
    assert!(!o.serve_here(), "one server");
    let text = format!("{o:?}");
    assert!(
        text.contains("Outbox") && text.contains("serving: true"),
        "{text}"
    );
}

#[test]
fn only_published_packets_are_sent_in_order_and_counted_per_slot() {
    let shared = shared();
    let o = Outbox::new();
    let (rx, at) = receiver();
    assert!(!o.pending(3));
    for k in 0..5 {
        assert!(push(&o, 3, k, at));
    }
    assert!(o.pending(3) && !o.pending(4));
    assert_eq!(o.send_all(&shared), 0, "nothing published yet");
    o.publish();
    assert!(push(&o, 3, 5, at), "after the publish: the next pass's");
    assert_eq!(o.send_all(&shared), 5);
    assert_eq!(firsts(&rx), vec![0, 1, 2, 3, 4]);
    assert!(o.pending(3), "one still queued");
    o.publish();
    assert!(o.send_next(&shared));
    assert!(!o.send_next(&shared));
    assert_eq!(firsts(&rx), vec![5]);
    assert!(!o.pending(3));
    let v = o.view();
    assert_eq!((v.relayed, v.full), (6, 0), "{v:?}");
    let lane = shared.lane(3);
    assert_eq!((lane.packets, lane.send_errors), (6, 0), "{lane:?}");
}

#[test]
fn the_queue_holds_its_length_and_takes_more_as_they_leave_across_its_end() {
    let shared = shared();
    let o = Outbox::new();
    let (rx, at) = receiver();
    for round in 0..3u8 {
        for k in 0..OUTBOX_LEN {
            assert!(push(&o, 0, k as u8, at), "round {round}, packet {k}");
        }
        assert!(!push(&o, 0, 0, at), "full at {OUTBOX_LEN}");
        o.count_full();
        o.publish();
        // Half of it out, then half again in: the indices wrap.
        let mut got = Vec::new();
        for _ in 0..OUTBOX_LEN / 2 {
            assert!(o.send_next(&shared));
        }
        got.extend(firsts(&rx));
        for k in 0..OUTBOX_LEN / 2 {
            assert!(push(&o, 0, k as u8, at));
        }
        assert!(!push(&o, 0, 0, at));
        o.publish();
        got.extend(send_all_read(&o, &shared, &rx));
        let mut want: Vec<u8> = (0..OUTBOX_LEN).map(|k| k as u8).collect();
        want.extend((0..OUTBOX_LEN / 2).map(|k| k as u8));
        assert_eq!(got.len(), want.len());
        assert_eq!(got, want, "round {round}");
    }
    assert_eq!(o.view().full, 3);
}

#[test]
fn a_packet_s_wait_counts_until_it_leaves_and_the_peak_is_taken_once() {
    let shared = shared();
    let o = Outbox::new();
    let (_rx, at) = receiver();
    assert!(push(&o, 0, 0, at));
    o.publish();
    std::thread::sleep(Duration::from_millis(3));
    assert!(o.send_next(&shared));
    let v = o.view();
    assert!((3_000_000..1_000_000_000).contains(&v.wait_max_ns), "{v:?}");
    let peak = o.take_wait_peak();
    assert!(peak >= 3_000_000 && peak <= v.wait_max_ns, "{peak}");
    assert_eq!(o.take_wait_peak(), 0);
    assert_eq!(o.view().wait_max_ns, v.wait_max_ns, "the maximum stays");
    // A late packet sent by the relay counts as the relay thread's: the
    // clock is the pass's, so it is 3 ms past due here.
    let lane = shared.lane(0);
    assert!(lane.late_max_ns >= 3_000_000, "{lane:?}");
}

/// The producer's publish wakes the thread serving the queue.
#[test]
fn a_publish_wakes_the_serving_thread() {
    let shared = shared();
    let o = Arc::new(Outbox::new());
    let (rx, at) = receiver();
    let server = {
        let (o, shared) = (Arc::clone(&o), Arc::clone(&shared));
        std::thread::spawn(move || {
            assert!(o.serve_here());
            let mut sent = 0;
            while sent < 3 {
                if o.send_next(&shared) {
                    sent += 1;
                } else {
                    std::thread::park();
                }
            }
        })
    };
    while !o.serving() {
        std::thread::yield_now();
    }
    for k in 0..3 {
        // Give the server time to park.
        std::thread::sleep(Duration::from_millis(20));
        assert!(push(&o, 1, k, at));
        o.publish();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while !server.is_finished() {
        assert!(Instant::now() < deadline, "the server was not woken");
        std::thread::sleep(Duration::from_millis(1));
    }
    server.join().unwrap();
    assert_eq!(firsts(&rx), vec![0, 1, 2]);
}
