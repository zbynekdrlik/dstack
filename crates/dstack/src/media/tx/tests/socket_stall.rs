// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #485: on a host with a frame sender (the FOH PC, dasio's NDIS
//! filter) the TX pass sends its NIC flows' frames in one call at its end,
//! while a flow to an on-PC receiver goes through the slot's UDP socket.
//! A Winsock send there once took ~1.4 ms (dasio's trace of 26.11.2026:
//! afd -> tcpip -> WFP -> Ndu -> npcap), and every NIC flow of that pass
//! left that late against its 250 us receiver. A socket send must never
//! hold the pass that feeds the NIC flows.

use super::*;
use std::sync::Mutex;

use crate::media::bypass::{FRAME_HEADROOM, FrameSender, Neighbour};

const OUR_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
const PEER_MAC: [u8; 6] = [0x00, 0x1d, 0xc1, 0x01, 0x02, 0x03];
const IFINDEX: u32 = 7;
/// The slow socket send of the FOH trace.
const STALL_NS: u64 = 1_400_000;
/// The NIC receivers' latency on FOH (iem-mixer, the Yamaha card).
const NIC_RECEIVER_NS: u64 = 250_000;

/// A host sender that takes every frame and keeps each one's stamp.
#[derive(Default)]
struct Taker {
    stamps: Mutex<Vec<u64>>,
    /// Each frame's UDP destination port.
    ports: Mutex<Vec<u16>>,
}

impl FrameSender for Taker {
    fn send(&self, frames: &[&[u8]], sent: &mut [bool]) -> io::Result<()> {
        let mut stamps = self.stamps.lock().unwrap();
        stamps.extend(frames.iter().map(|f| stamp_of(&f[FRAME_HEADROOM..])));
        // Ethernet II (14), IPv4 (20), then UDP's source and destination.
        let mut ports = self.ports.lock().unwrap();
        ports.extend(frames.iter().map(|f| u16::from_be_bytes([f[36], f[37]])));
        sent.fill(true);
        Ok(())
    }
}

/// A socket bound to `ip`.
fn socket_at(ip: Ipv4Addr) -> UdpSocket {
    let s = UdpSocket::bind((ip, 0)).unwrap();
    s.set_nonblocking(true).unwrap();
    s
}

/// A device with a host sender: a NIC flow to 127.0.0.1 (in the neighbour
/// table) and a socket flow to 127.0.0.2 (not in it: the on-PC receiver's
/// path). Returns the device, the sender, the socket flow's receiver and
/// the two slots (NIC, socket).
fn host_device() -> (Arc<TxShared>, Arc<Taker>, UdpSocket, (usize, usize)) {
    let (shared, taker, sock_rx, slots) = host_device_at(10 * MS);
    (shared, taker, sock_rx, slots)
}

/// [`host_device`] with the socket flow's receiver at `sock_latency_ns`.
fn host_device_at(sock_latency_ns: u64) -> (Arc<TxShared>, Arc<Taker>, UdpSocket, (usize, usize)) {
    let shared = shared_with(Vec::new());
    assert!(shared.set_source(0, Source::Counter));
    assert!(shared.set_source(1, Source::Counter));
    let nic_rx = socket_at(Ipv4Addr::LOCALHOST);
    let sock_rx = socket_at(Ipv4Addr::new(127, 0, 0, 2));
    let (nic, _) = add(&shared, &req(v4(&nic_rx)));
    let (sock, _) = add(&shared, &req(v4(&sock_rx)));
    let taker = Arc::new(Taker::default());
    let host_nic = HostNic {
        ifindex: IFINDEX,
        mac: OUR_MAC,
        name: "test0".to_string(),
    };
    assert!(shared.use_frame_sender(taker.clone(), host_nic));
    shared.set_neighbours(&[Neighbour {
        ip: Ipv4Addr::LOCALHOST,
        mac: PEER_MAC,
        ifindex: IFINDEX,
    }]);
    // The on-PC receiver plays at 10 ms in most tests: its delayed packets
    // are still worth sending, so its schedule never restarts.
    shared.set_receiver_latency(sock, v4(&sock_rx), sock_latency_ns);
    shared.set_receiver_latency(nic, v4(&nic_rx), NIC_RECEIVER_NS);
    (shared, taker, sock_rx, (nic, sock))
}

/// One run of [`a_slow_socket_send_never_holds_the_nic_flows_of_its_pass`]:
/// 40 ms of a real TX thread (and its relay thread) on a step clock, one
/// socket send in ten passes stalled. The NIC flow's lane and frames, the
/// socket flow's lane and what its receiver got.
fn stalled_run() -> (LaneCounts, Vec<u64>, LaneCounts, Vec<u64>) {
    let (shared, taker, sock_rx, (nic, sock)) = host_device();
    let clock = StepClock::at(T);
    std::thread::scope(|scope| {
        let tx = scope.spawn(|| run_with(clock.as_ref(), Arc::clone(&shared), false));
        let _stop = OnDrop(|| shared.stop());
        for k in 1..=40 {
            if k % 10 == 0 {
                shared.stall_socket_send(STALL_NS);
            }
            clock.run_to(T + k * MS);
        }
        // Every socket packet out before the stop.
        let deadline = Instant::now() + Duration::from_secs(5);
        while shared.lane(sock).packets < shared.lane(nic).packets {
            assert!(Instant::now() < deadline, "the socket flow is not out");
            std::thread::sleep(Duration::from_millis(1));
        }
        shared.stop();
        tx.join().unwrap();
    });
    let frames = taker.stamps.lock().unwrap().clone();
    let got: Vec<u64> = drain(&sock_rx).iter().map(|p| stamp_of(p)).collect();
    (shared.lane(nic), frames, shared.lane(sock), got)
}

/// The NIC flow's frames leave within the FOH receivers' 250 us of due
/// while one socket send in ten passes takes 1.4 ms; the socket flow is
/// delayed by it, but whole and in order. The step clock's time moves only
/// at the threads' sleeps, so a frame's lateness is the real time its pass
/// spent before the NIC batch. A run whose NIC frame was late by less than
/// the stall was a preempted pass (a loaded runner), not the stall: run
/// again, at most three runs. One late by the stall fails at once.
#[test]
fn a_slow_socket_send_never_holds_the_nic_flows_of_its_pass() {
    for run in 1..=3 {
        let (lane, frames, s, got) = stalled_run();
        assert!(frames.len() > 100, "{}", frames.len());
        assert_eq!(lane.packets, frames.len() as u64, "{lane:?}");
        assert!(
            lane.late_max_ns < STALL_NS,
            "a NIC frame left {} us past due behind a slow socket send: {lane:?}",
            lane.late_max_ns / 1_000
        );
        // The socket flow: delayed by the stall, but whole and in order.
        assert_eq!(got, frames, "every stamp of the socket flow, in order");
        assert!(got.windows(2).all(|w| w[1] == w[0] + FPP));
        assert!(s.late_max_ns >= STALL_NS, "{s:?}");
        assert_eq!((s.rebootstraps, s.send_errors), (0, 0), "{s:?}");
        assert_eq!(lane.rebootstraps, 0, "{lane:?}");
        if lane.late_max_ns < NIC_RECEIVER_NS && run < 3 || run == 3 {
            assert!(
                lane.late_max_ns < NIC_RECEIVER_NS,
                "a NIC frame {} us past due in each of 3 runs: {lane:?}",
                lane.late_max_ns / 1_000
            );
            assert_eq!(lane.over_latency, 0, "{lane:?}");
            return;
        }
    }
}

/// Runs `stop` when dropped: a failed assert inside a thread scope still
/// ends the threads, so the test fails instead of hanging in the join.
struct OnDrop<F: FnMut()>(F);

impl<F: FnMut()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        (self.0)();
    }
}

/// The relay thread's queue as the test thread serves it: the passes
/// queue the socket packets, the test sends them when it chooses.
fn served(shared: &TxShared) -> &super::super::outbox::Outbox {
    let outbox = shared.outbox().expect("a host sender");
    assert!(outbox.serve_here(), "the test thread serves it");
    outbox
}

/// Steps `tx` every 500 us from `from` for `ms` ms.
fn steps(tx: &mut Transmitter, from: u64, ms: u64) -> u64 {
    let mut t = from;
    for _ in 0..ms * 2 {
        tx.step(Some(t), 0);
        t += MS / 2;
    }
    t
}

/// A queued socket packet's age and lateness are judged when the relay
/// thread sends it, not when the pass queued it; a late one counts as the
/// relay thread's, and the late watch's line says how long it waited.
#[test]
fn a_relayed_socket_packet_is_judged_when_it_leaves() {
    let (shared, taker, sock_rx, (nic, sock)) = host_device_at(NIC_RECEIVER_NS);
    let outbox = served(&shared);
    let mut tx = Transmitter::new(Arc::clone(&shared));
    let mut watch = crate::media::late::LateWatch::new();
    assert_eq!(
        warned(|| watch.watch(0, &shared)),
        "",
        "the first look learns"
    );
    steps(&mut tx, T, 2);
    assert!(drain(&sock_rx).is_empty(), "the pass sent no socket packet");
    assert_eq!(shared.lane(sock).packets, 0);
    let frames = taker.stamps.lock().unwrap().len() as u64;
    assert!(frames >= 3, "{frames}");
    assert_eq!(
        shared.lane(nic).packets,
        frames,
        "the NIC flow went at once"
    );
    // The relay thread sends 2 ms later.
    std::thread::sleep(Duration::from_millis(2));
    let n = outbox.send_all(&shared);
    assert_eq!(n as u64, frames, "every queued packet, once");
    assert_eq!(outbox.send_all(&shared), 0);
    let got: Vec<u64> = drain(&sock_rx).iter().map(|p| stamp_of(p)).collect();
    assert_eq!(got, *taker.stamps.lock().unwrap(), "in order");
    let s = shared.lane(sock);
    assert_eq!(s.packets, frames, "{s:?}");
    assert!(s.late_max_ns >= 2 * MS, "{s:?}");
    assert!(s.age_max_ns >= 2 * MS, "{s:?}");
    assert_eq!(s.over_latency, frames, "every one late at 250 us: {s:?}");
    // The queue's late packets, never the escape relay's (#177).
    assert_eq!(shared.over_latency_queued(sock), Some(frames));
    assert_eq!(shared.over_latency_queued(nic), Some(0));
    assert_eq!(shared.over_latency_relayed(sock), 0);
    assert_eq!(shared.take_since_escape(sock), None);
    let q = shared.tx_queue().unwrap();
    assert!(q.serving, "{q:?}");
    assert_eq!((q.relayed, q.full), (frames, 0), "{q:?}");
    assert!(q.wait_max_ns >= 2 * MS, "{q:?}");
    let log = warned(|| watch.watch(crate::media::late::EVERY_NS, &shared));
    let line = log
        .lines()
        .find(|l| l.contains(&format!("slot={sock} ")))
        .unwrap_or_else(|| panic!("{log}"));
    assert!(
        line.contains(&format!("relayed=0 queued={frames} ")),
        "{line}"
    );
    let waited = line
        .split("queue_wait_max_us=")
        .nth(1)
        .and_then(|r| r.split(' ').next())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or_else(|| panic!("{line}"));
    assert!((2_000..=q.wait_max_ns / 1_000).contains(&waited), "{line}");
    assert!(line.contains("queue_full=0"), "{line}");
    // Taken by the look.
    assert_eq!(shared.take_queue_wait_peak(), 0);
    assert_eq!(shared.tx_queue().unwrap().wait_max_ns, q.wait_max_ns);
    // The next second's line counts that second's queued late packets only.
    let t = steps(&mut tx, T + 2 * MS, 1);
    std::thread::sleep(Duration::from_millis(1));
    let more = outbox.send_all(&shared) as u64;
    assert!(more >= 1, "{more}");
    let log = warned(|| watch.watch(2 * crate::media::late::EVERY_NS, &shared));
    assert!(log.contains(&format!("queued={more} ")), "{log}");
    assert!(t > T);
}

/// A device without a host sender has no queue: no queue fields in its
/// late line, no `tx_queue`.
#[test]
fn a_device_without_a_host_sender_has_no_queue() {
    let mut cfg = cfg();
    cfg.tx_latency_ns = Some(500_000);
    let shared =
        Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, Vec::new().into(), 1, &[]).unwrap());
    let mut rig = Rig::with(shared);
    let (slot, _) = rig.flow(vec![Some(1)]);
    rig.shared.set_receiver_latency(slot, v4(&rig.rx), 250_000);
    assert_eq!(rig.shared.tx_queue(), None);
    assert_eq!(rig.shared.take_queue_wait_peak(), 0);
    let mut watch = crate::media::late::LateWatch::new();
    watch.watch(0, &rig.shared);
    rig.tx.step(Some(T), 0);
    rig.tx.step(Some(first_due_ns(T) + 50_000), 0);
    let log = warned(|| watch.watch(crate::media::late::EVERY_NS, &rig.shared));
    assert!(log.contains("relayed=0"), "{log}");
    assert!(!log.contains("queue"), "{log}");
    assert_eq!(rig.shared.over_latency_queued(slot), None);
}

/// The queue full (the relay thread stuck): the pass sends each further
/// socket packet itself, counted, never dropping one.
#[test]
fn a_full_queue_sends_at_once_and_counts_it() {
    // The receiver at 500 ms: the queued packets wait the whole run in
    // real time (17.5 ms on a coverage runner), none of them late.
    let (shared, taker, sock_rx, (_, sock)) = host_device_at(500 * MS);
    let outbox = served(&shared);
    let mut tx = Transmitter::new(Arc::clone(&shared));
    // 3 packets a ms of each flow: past the queue's length.
    let ms = OUTBOX_LEN as u64 / 3 + 20;
    let mut host_watch = crate::media::late::HostWatch::new();
    host_watch.watch(0, &shared);
    steps(&mut tx, T, ms);
    let due = taker.stamps.lock().unwrap().clone();
    assert!(due.len() > OUTBOX_LEN + 20, "{}", due.len());
    let q = shared.tx_queue().unwrap();
    assert_eq!(q.full as usize, due.len() - OUTBOX_LEN, "{q:?}");
    assert_eq!(q.relayed, 0);
    let log = warned(|| host_watch.watch(crate::media::late::EVERY_NS, &shared));
    assert!(
        log.contains(crate::media::late::QUEUE_FULL)
            && log.contains(&format!("packets={}", q.full)),
        "{log}"
    );
    let at_once: Vec<u64> = drain(&sock_rx).iter().map(|p| stamp_of(p)).collect();
    assert_eq!(at_once, due[OUTBOX_LEN..], "the ones past a full queue");
    // Read as they come: a loopback socket holds about 270 datagrams.
    let mut later: Vec<u64> = Vec::new();
    for _ in 0..OUTBOX_LEN / 64 {
        for _ in 0..64 {
            assert!(outbox.send_next(&shared));
        }
        later.extend(drain(&sock_rx).iter().map(|p| stamp_of(p)));
    }
    assert!(!outbox.send_next(&shared));
    assert_eq!(later, due[..OUTBOX_LEN], "then the queued ones, in order");
    let s = shared.lane(sock);
    assert_eq!(s.packets as usize, due.len(), "none lost: {s:?}");
    assert_eq!(s.send_errors, 0);
    // Within the receiver's 500 ms: none of the queued ones late.
    assert_eq!(s.over_latency, 0, "{s:?}");
    assert_eq!(shared.over_latency_queued(sock), Some(0));
    // Room again: the next ones queue.
    let full = shared.tx_queue().unwrap().full;
    steps(&mut tx, T + ms * MS, 2);
    assert!(drain(&sock_rx).is_empty());
    assert_eq!(shared.tx_queue().unwrap().full, full);
}

/// A flow whose receiver becomes resolved while packets of it still wait
/// in the queue keeps to the queue until they left, then goes to the NIC:
/// it never passes its own packets.
#[test]
fn a_flow_with_packets_queued_keeps_to_the_queue_until_they_left() {
    let (shared, taker, sock_rx, (_, sock)) = host_device();
    let outbox = served(&shared);
    let mut tx = Transmitter::new(Arc::clone(&shared));
    let t = steps(&mut tx, T, 2);
    let sock_port = v4(&sock_rx).port();
    let resolved = |rows: &[Neighbour]| {
        let mut all = vec![Neighbour {
            ip: Ipv4Addr::LOCALHOST,
            mac: PEER_MAC,
            ifindex: IFINDEX,
        }];
        all.extend_from_slice(rows);
        shared.set_neighbours(&all);
    };
    resolved(&[Neighbour {
        ip: Ipv4Addr::new(127, 0, 0, 2),
        mac: PEER_MAC,
        ifindex: IFINDEX,
    }]);
    assert!(outbox.pending(sock));
    let t = steps(&mut tx, t, 2);
    assert!(
        !taker.ports.lock().unwrap().contains(&sock_port),
        "no frame of the flow while its packets wait"
    );
    let queued = outbox.send_all(&shared);
    assert!(queued >= 8, "{queued}");
    assert!(!outbox.pending(sock));
    steps(&mut tx, t, 2);
    let to_sock = taker
        .ports
        .lock()
        .unwrap()
        .iter()
        .filter(|&&p| p == sock_port)
        .count();
    assert!(to_sock >= 4, "the NIC once the queue is empty: {to_sock}");
    let got: Vec<u64> = drain(&sock_rx).iter().map(|p| stamp_of(p)).collect();
    assert_eq!(got.len(), queued);
    assert!(got.windows(2).all(|w| w[1] == w[0] + FPP));
    assert_eq!(shared.lane(sock).packets as usize, queued + to_sock);
}

/// The TX loop starts the relay thread on a frame-sender host also with
/// the escape off, for the socket sends; its start line says what for.
#[test]
fn the_tx_loop_starts_a_relay_thread_for_a_host_sender_s_socket_sends() {
    crate::test_log::undecided();
    let (shared, _, _, _) = host_device();
    let clock = StepClock::at(T);
    let stopper = {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            shared.stop();
        })
    };
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        run_with(clock.as_ref(), Arc::clone(&shared), false)
    });
    stopper.join().unwrap();
    assert!(
        log.contains("escape=false socket_sends=true")
            && log.contains(super::super::thread::relay_line(false, true)),
        "{log}"
    );
    assert!(shared.tx_queue().unwrap().serving);
    assert_eq!(
        super::super::thread::relay_line(true, false),
        "TX relay thread started: it sends TX's packets while the TX thread blocks its bounded escape"
    );
    for (escape, sends) in [(true, true), (true, false), (false, true)] {
        let line = super::super::thread::relay_line(escape, sends);
        assert!(line.starts_with("TX relay thread started: "), "{line}");
        assert_eq!(line.contains("bounded escape"), escape, "{line}");
        assert_eq!(line.contains("socket packets"), sends, "{line}");
    }
}

/// While the relay thread covers a bounded escape it runs the passes and
/// sends none of the queue (a slow socket send would hold its next pass,
/// as it held the TX thread's); the packets go after the hand-back, in
/// order. The TX thread's last pass before the escape publishes nothing,
/// so the relay thread is not inside a send at the offer.
#[test]
fn the_relay_thread_sends_no_socket_packet_while_it_covers_an_escape() {
    let (shared, taker, sock_rx, (nic, sock)) = host_device();
    let clock = StepClock::at(T);
    let relay = relay::Relay::new();
    let mut tx = Transmitter::new(Arc::clone(&shared));
    tx.step(Some(T), 0);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            run_relay(clock.as_ref(), &relay, &Scheduling::new(), Some(&*shared));
        });
        let _stop = OnDrop(|| relay.stop());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !shared.tx_queue().unwrap().serving {
            assert!(Instant::now() < deadline, "the relay thread serves");
            std::thread::sleep(Duration::from_millis(1));
        }
        // The last pass before the escape: queued, not published.
        let escapes = tx.before_wait(|| (Some(T + MS), 0), || true, || panic!("not put off"));
        assert!(escapes);
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(shared.lane(sock).packets, 0, "nothing published yet");
        let mut misses = 0;
        let mut covered = None;
        let back = escape_through(
            &relay,
            tx,
            &mut misses,
            Duration::from_secs(5),
            || {
                clock.run_to(T + 5 * MS);
                std::thread::sleep(Duration::from_millis(20));
                covered = Some((shared.lane(nic).packets, shared.lane(sock).packets));
            },
            || panic!("the relay took over"),
        );
        assert_eq!(misses, 0);
        let (nic_packets, sock_packets) = covered.unwrap();
        assert!(nic_packets >= 12, "the relay's passes sent: {nic_packets}");
        assert_eq!(sock_packets, 0, "no socket send while covering");
        // After the hand-back the relay thread sends them all.
        let deadline = Instant::now() + Duration::from_secs(5);
        while shared.lane(sock).packets < shared.lane(nic).packets {
            assert!(Instant::now() < deadline, "the socket flow is not out");
            std::thread::sleep(Duration::from_millis(1));
        }
        relay.stop();
        drop(back);
    });
    let got: Vec<u64> = drain(&sock_rx).iter().map(|p| stamp_of(p)).collect();
    assert_eq!(got, *taker.stamps.lock().unwrap(), "every stamp, in order");
    assert_eq!(shared.tx_queue().unwrap().full, 0);
}
