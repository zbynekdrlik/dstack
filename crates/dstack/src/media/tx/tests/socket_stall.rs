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
}

impl FrameSender for Taker {
    fn send(&self, frames: &[&[u8]], sent: &mut [bool]) -> io::Result<()> {
        let mut stamps = self.stamps.lock().unwrap();
        stamps.extend(frames.iter().map(|f| stamp_of(&f[FRAME_HEADROOM..])));
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
    // The on-PC receiver plays at 10 ms: its delayed packets are still
    // worth sending, so its schedule never restarts.
    shared.set_receiver_latency(sock, v4(&sock_rx), 10 * MS);
    shared.set_receiver_latency(nic, v4(&nic_rx), NIC_RECEIVER_NS);
    (shared, taker, sock_rx, (nic, sock))
}

#[test]
fn a_slow_socket_send_never_holds_the_nic_flows_of_its_pass() {
    let (shared, taker, sock_rx, (nic, sock)) = host_device();
    let clock = StepClock::at(T);
    std::thread::scope(|scope| {
        let tx = scope.spawn(|| run_with(clock.as_ref(), Arc::clone(&shared), false));
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
    let lane = shared.lane(nic);
    let frames = taker.stamps.lock().unwrap().clone();
    assert!(frames.len() > 100, "{}", frames.len());
    assert_eq!(lane.packets, frames.len() as u64, "{lane:?}");
    assert!(
        lane.late_max_ns < NIC_RECEIVER_NS,
        "a NIC frame left {} us past due behind a slow socket send: {lane:?}",
        lane.late_max_ns / 1_000
    );
    assert_eq!(lane.over_latency, 0, "{lane:?}");
    // The socket flow: delayed by the stall, but whole and in order.
    let s = shared.lane(sock);
    let got: Vec<u64> = drain(&sock_rx).iter().map(|p| stamp_of(p)).collect();
    assert_eq!(got, frames, "every stamp of the socket flow, in order");
    assert!(got.windows(2).all(|w| w[1] == w[0] + FPP));
    assert!(s.late_max_ns >= STALL_NS, "{s:?}");
    assert_eq!((s.rebootstraps, s.send_errors), (0, 0), "{s:?}");
    assert_eq!(lane.rebootstraps, 0, "{lane:?}");
}
