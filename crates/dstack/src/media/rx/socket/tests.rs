use super::*;

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::media::packet::write;
use crate::media::ring::Ring;
use crate::media::rx::{Receiver, RxCounts, RxPlan, RxRoute, RxShared};
use crate::media::tx::ns_of;
use crate::wire::MAX_DATAGRAM;

const RATE: u32 = 96_000;
const LOOPBACK: Ipv4Addr = Ipv4Addr::LOCALHOST;
/// Every wait in these tests ends.
const BOUND: Duration = Duration::from_secs(5);
/// The flows' playout here: 2 ms at 96 kHz, block 0, so `late` past 192.
const LATENCY: u32 = 192;
/// Ten years of a grandmaster's uptime, as a position.
const P0: u64 = 10 * 365 * 86_400 * RATE as u64;

#[test]
fn a_wake_delay_is_the_pass_s_wall_time_less_the_kernel_s() {
    assert_eq!(wake_delay_ns(Some(5_000), Some(1_200)), Some(3_800));
    // The wall clock stepped back between the two reads.
    assert_eq!(wake_delay_ns(Some(1_000), Some(1_200)), Some(0));
    assert_eq!(wake_delay_ns(None, Some(1_200)), None);
    assert_eq!(wake_delay_ns(Some(5_000), None), None);
}

#[test]
fn the_wire_lateness_is_the_lateness_at_now_less_the_wake() {
    let now = ns_of(P0 + 1_000, RATE);
    // 1 000 samples late at the pass; a 10 ms wake (960 samples) leaves 40.
    assert_eq!(wire_lateness(now, 10_000_000, P0, RATE), 40);
    assert_eq!(wire_lateness(now, 0, P0, RATE), 1_000);
    // A wake longer than its lateness: on time on the wire.
    assert_eq!(wire_lateness(now, 20_000_000, P0, RATE), 0);
    assert_eq!(wire_lateness(5, u64::MAX, P0, RATE), 0);
}

#[test]
fn a_packet_is_wire_late_only_past_late_s_limit() {
    let now = ns_of(P0 + 1_000, RATE);
    // 1 000 samples late at the pass, a 10 ms wake: 40 on the wire.
    assert!(!wire_late(now, 10_000_000, P0, RATE, 40));
    assert!(wire_late(now, 10_000_000, P0, RATE, 39));
    assert!(!wire_late(now, 20_000_000, P0, RATE, 0));
}

#[test]
fn a_socket_is_read_through_its_handle_and_no_handle_is_an_error() {
    // dstack #326: the pump reads a group socket the RX thread holds.
    let a = UdpSocket::bind((LOOPBACK, 0)).unwrap();
    let b = UdpSocket::bind((LOOPBACK, 0)).unwrap();
    for s in [&a, &b] {
        s.set_nonblocking(true).unwrap();
    }
    let (ha, hb) = (handle_of(&a), handle_of(&b));
    assert!(ha != 0 && hb != 0 && ha != hb, "{ha} {hb}");
    let t = UdpSocket::bind((LOOPBACK, 0)).unwrap();
    t.send_to(b"to-b", b.local_addr().unwrap()).unwrap();
    let mut buf = [0u8; 16];
    let deadline = Instant::now() + BOUND;
    let got = loop {
        match recv_handle(hb, &mut buf) {
            Ok(r) => break r,
            Err(e) if e.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) => panic!("{e}"),
        }
    };
    assert_eq!(&buf[..got.len], b"to-b");
    assert_eq!(got.from.map(SocketAddr::V4), Some(t.local_addr().unwrap()));
    // The other socket has nothing, and both stay open.
    let none = recv_handle(ha, &mut buf).unwrap_err();
    assert_eq!(none.kind(), ErrorKind::WouldBlock);
    assert!(a.local_addr().is_ok() && b.local_addr().is_ok());
    assert_eq!(
        recv_handle(0, &mut buf).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_timespec_reads_as_ns_since_1970() {
    use linux::timespec_ns;
    assert_eq!(timespec_ns(2, 5), Some(2_000_000_005));
    assert_eq!(timespec_ns(0, 999_999_999), Some(999_999_999));
    assert_eq!(timespec_ns(0, 1_000_000_000), None);
    assert_eq!(timespec_ns(-1, 0), None);
    assert_eq!(timespec_ns(1, -1), None);
    assert_eq!(timespec_ns(i64::MAX, 0), None);
    assert_eq!(timespec_ns(18_446_744_073, 709_551_615), Some(u64::MAX));
    assert_eq!(timespec_ns(18_446_744_073, 709_551_616), None);
}

/// Linux stamps a receive in the wall clock: the stamp clock is it.
#[cfg(not(windows))]
#[test]
fn the_stamp_clock_is_the_wall_clock_now() {
    let wall = stamp_now_ns().unwrap();
    let sys = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    assert!(sys.abs_diff(wall) < 1_000_000_000, "{wall} vs {sys}");
}

/// dstack #381: Windows stamps a receive in QPC: the stamp clock is the
/// app's raw QPC clock, which the stamps are scaled to.
#[cfg(windows)]
#[test]
fn the_stamp_clock_is_the_app_s_qpc_now() {
    let before = crate::clock::app_monotonic_ns();
    let at = stamp_now_ns().unwrap();
    let after = crate::clock::app_monotonic_ns();
    assert!(
        i64::try_from(at).is_ok_and(|a| before <= a && a <= after),
        "{before} {at} {after}"
    );
}

#[test]
fn a_box_stamps_only_when_its_sockets_took_the_request_and_its_nic_stamps_on_qpc() {
    let nic = |software_rx, hardware_rx| {
        Some(NicStamps {
            software_rx,
            hardware_rx,
        })
    };
    // Linux: the kernel's, whatever an interface would say.
    assert_eq!(stamping_of(true, true, None), Stamping::Kernel);
    assert_eq!(stamping_of(true, true, nic(false, true)), Stamping::Kernel);
    assert_eq!(stamping_of(true, false, None), Stamping::Refused);
    // Windows: the NIC driver's software stamps only.
    assert_eq!(stamping_of(false, true, nic(true, false)), Stamping::Nic);
    assert_eq!(
        stamping_of(false, true, nic(false, false)),
        Stamping::NicOff
    );
    assert_eq!(stamping_of(false, true, None), Stamping::NicOff);
    // A hardware clock's stamps are not QPC: none taken, even beside
    // software ones.
    assert_eq!(
        stamping_of(false, true, nic(false, true)),
        Stamping::NicClock
    );
    assert_eq!(
        stamping_of(false, true, nic(true, true)),
        Stamping::NicClock
    );
    // A refused request (Windows before 10 2004) wins over the interface.
    assert_eq!(
        stamping_of(false, false, nic(true, false)),
        Stamping::Refused
    );
}

#[test]
fn only_the_kernel_s_and_the_nic_s_software_stamps_are_taken() {
    let all = [
        (Stamping::Kernel, true, "kernel"),
        (Stamping::Nic, true, "nic"),
        (Stamping::Refused, false, "refused"),
        (Stamping::NicOff, false, "nic_off"),
        (Stamping::NicClock, false, "nic_clock"),
    ];
    for (s, on, word) in all {
        assert_eq!((s.on(), s.word()), (on, word), "{s:?}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_stamps_in_the_kernel_once_the_sockets_took_the_request() {
    assert_eq!(stamping(LOOPBACK, true), Stamping::Kernel);
    assert_eq!(stamping(LOOPBACK, false), Stamping::Refused);
    let s = UdpSocket::bind((LOOPBACK, 0)).unwrap();
    assert!(stamp_receives(&s));
    let (shared, _rx) = receiver(None);
    assert_eq!(shared.stamping(), Stamping::Kernel);
    assert!(shared.stamps());
}

/// QPC at 10 MHz: a tick is 100 ns.
const QPC_HZ: i64 = 10_000_000;

#[test]
fn a_qpc_stamp_up_to_a_second_before_its_read_is_taken_in_ns() {
    let now = 50 * QPC_HZ;
    // 3 ms before the read: its own ticks as ns.
    let stamp = now - 30_000;
    assert_eq!(
        qpc_stamp_ns(stamp as u64, now, QPC_HZ),
        Some(stamp as u64 * 100)
    );
    assert_eq!(
        qpc_stamp_ns(now as u64, now, QPC_HZ),
        Some(now as u64 * 100)
    );
    // Exactly the window before the read, and one tick more.
    let edge = now - QPC_HZ;
    assert_eq!(STAMP_WINDOW_NS, 1_000_000_000);
    assert_eq!(
        qpc_stamp_ns(edge as u64, now, QPC_HZ),
        Some(edge as u64 * 100)
    );
    assert_eq!(qpc_stamp_ns(edge as u64 - 1, now, QPC_HZ), None);
    // After the read, or no count of ours (a NIC clock's).
    assert_eq!(qpc_stamp_ns(now as u64 + 1, now, QPC_HZ), None);
    assert_eq!(qpc_stamp_ns(u64::MAX, now, QPC_HZ), None);
    assert_eq!(qpc_stamp_ns(1 << 63, now, QPC_HZ), None);
    // Another frequency scales both the window and the value: 3 GHz.
    let ghz3 = 3_000_000_000;
    let now = 9 * ghz3;
    assert_eq!(
        qpc_stamp_ns(now as u64 - 3, now, ghz3),
        Some(9_000_000_000 - 1)
    );
    assert_eq!(
        qpc_stamp_ns((now - ghz3) as u64, now, ghz3),
        Some(8_000_000_000)
    );
    assert_eq!(qpc_stamp_ns((now - ghz3 - 3) as u64, now, ghz3), None);
}

/// One Windows control message: its `cmsg_len` (`len`), level and type,
/// and `data`, padded to the next header.
fn control_message(len: u64, level: i32, kind: i32, data: &[u8]) -> Vec<u8> {
    let mut m = Vec::new();
    m.extend_from_slice(&len.to_le_bytes());
    m.extend_from_slice(&level.to_le_bytes());
    m.extend_from_slice(&kind.to_le_bytes());
    m.extend_from_slice(data);
    m.resize(m.len().next_multiple_of(WIN_CMSG_ALIGN), 0);
    m
}

#[test]
fn the_receive_stamp_is_the_whole_socket_level_timestamp_message() {
    let stamp = 0x0102_0304_0506_0708u64.to_le_bytes();
    let ts = |len: u64| control_message(len, WIN_SOL_SOCKET, WIN_SO_TIMESTAMP, &stamp);
    // The header (16 bytes) and the UINT64, as WSA_CMSG_LEN(8) says.
    assert_eq!(qpc_stamp_of(&ts(24)), Some(0x0102_0304_0506_0708));
    assert_eq!(qpc_stamp_of(&ts(32)), Some(0x0102_0304_0506_0708));
    // A length short of the stamp: none read.
    assert_eq!(qpc_stamp_of(&ts(23)), None);
    // Cut short in the buffer: none read.
    assert_eq!(qpc_stamp_of(&ts(24)[..23]), None);
    assert_eq!(qpc_stamp_of(&ts(24)[..15]), None);
    assert_eq!(qpc_stamp_of(&[]), None);
    // Another type or level.
    let other = control_message(24, WIN_SOL_SOCKET, WIN_SO_TIMESTAMP + 1, &stamp);
    assert_eq!(qpc_stamp_of(&other), None);
    let ip = control_message(24, 0, WIN_SO_TIMESTAMP, &stamp);
    assert_eq!(qpc_stamp_of(&ip), None);
}

#[test]
fn the_walk_steps_over_other_messages_by_their_aligned_length() {
    let stamp = 77u64.to_le_bytes();
    let ts = || control_message(24, WIN_SOL_SOCKET, WIN_SO_TIMESTAMP, &stamp);
    // An IP_PKTINFO-shaped message first (28 bytes: the header, an address
    // and an index, padded to 32), then the stamp.
    let mut c = control_message(28, 0, 19, &[0xaa; 12]);
    assert_eq!(c.len(), 32);
    c.extend(ts());
    assert_eq!(qpc_stamp_of(&c), Some(77));
    // An odd length rounds up to the alignment: 20 -> 24.
    let mut c = control_message(20, 0, 19, &[0xbb; 4]);
    assert_eq!(c.len(), 24);
    c.extend(ts());
    assert_eq!(qpc_stamp_of(&c), Some(77));
    // A header whose length cannot hold itself stops the walk (it would
    // never move on); one that holds only itself is stepped over.
    let mut c = control_message(15, 0, 19, &[]);
    c.extend(ts());
    assert_eq!(qpc_stamp_of(&c), None);
    let mut c = control_message(16, 0, 19, &[]);
    c.extend(ts());
    assert_eq!(qpc_stamp_of(&c), Some(77));
    // A length past the buffer ends it.
    let mut c = control_message(u64::MAX, 0, 19, &[]);
    c.extend(ts());
    assert_eq!(qpc_stamp_of(&c), None);
}

/// dstack #381: Windows takes the stamp request on a UDP socket (Windows 10
/// 2004 and later, as the runners), but stamps only in a NIC's miniport
/// driver: loopback has none, so its datagrams come unstamped through
/// `WSARecvMsg` and the box reads `nic_off`.
#[cfg(windows)]
#[test]
fn windows_reads_a_loopback_datagram_through_wsarecvmsg_unstamped() {
    let r = UdpSocket::bind((LOOPBACK, 0)).unwrap();
    r.set_nonblocking(true).unwrap();
    assert!(stamp_receives(&r), "SIO_TIMESTAMPING refused");
    assert_eq!(stamping(LOOPBACK, true), Stamping::NicOff);
    assert_eq!(stamping(LOOPBACK, false), Stamping::Refused);
    let t = UdpSocket::bind((LOOPBACK, 0)).unwrap();
    t.send_to(b"stamped?", r.local_addr().unwrap()).unwrap();
    let mut buf = [0u8; 64];
    let deadline = Instant::now() + BOUND;
    let got = loop {
        match recv_handle(handle_of(&r), &mut buf) {
            Ok(g) => break g,
            Err(e) if e.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) => panic!("{e}"),
        }
    };
    assert_eq!(&buf[..got.len], b"stamped?");
    assert_eq!(got.from.map(SocketAddr::V4), Some(t.local_addr().unwrap()));
    assert_eq!(got.at_ns, None);
    let (shared, _rx) = receiver(None);
    assert_eq!(shared.stamping(), Stamping::NicOff);
    assert!(!shared.stamps());
}

#[cfg(target_os = "linux")]
#[test]
fn only_the_socket_level_ns_stamp_is_the_receive_time() {
    use linux::is_stamp;
    // A cmsghdr (16 bytes on 64-bit Linux) and a timespec (16).
    let whole = std::mem::size_of::<libc::cmsghdr>() + std::mem::size_of::<libc::timespec>();
    assert!(is_stamp(libc::SOL_SOCKET, libc::SCM_TIMESTAMPNS, whole));
    assert!(is_stamp(libc::SOL_SOCKET, libc::SCM_TIMESTAMPNS, whole + 8));
    // Cut short (a truncated control buffer): no stamp read.
    assert!(!is_stamp(
        libc::SOL_SOCKET,
        libc::SCM_TIMESTAMPNS,
        whole - 1
    ));
    assert!(!is_stamp(
        libc::SOL_SOCKET,
        libc::SCM_TIMESTAMPNS + 1,
        whole
    ));
    assert!(!is_stamp(libc::IPPROTO_IP, libc::SCM_TIMESTAMPNS, whole));
}

#[cfg(target_os = "linux")]
#[test]
fn the_sender_is_a_whole_ipv4_address_only() {
    use linux::sender_of;
    let whole = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
    // SAFETY: all-zero is a valid sockaddr_in.
    let mut a: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    a.sin_family = libc::AF_INET as libc::sa_family_t;
    a.sin_port = 4321u16.to_be();
    a.sin_addr.s_addr = u32::from(Ipv4Addr::new(10, 1, 2, 3)).to_be();
    assert_eq!(
        sender_of(&a, whole),
        Some(SocketAddrV4::new(Ipv4Addr::new(10, 1, 2, 3), 4321))
    );
    assert_eq!(sender_of(&a, whole + 4), sender_of(&a, whole));
    assert_eq!(sender_of(&a, whole - 1), None);
    assert_eq!(sender_of(&a, 0), None);
    a.sin_family = libc::AF_INET6 as libc::sa_family_t;
    assert_eq!(sender_of(&a, whole), None);
}

/// The RX state of one ring, flow slot 2 from `LOOPBACK` routed to it
/// (`group` for a multicast flow), and its thread.
fn receiver(group: Option<SocketAddrV4>) -> (Arc<RxShared>, Receiver) {
    let rings: Arc<[Ring]> = vec![Ring::new()].into();
    let s = Arc::new(RxShared::new(RATE, LOOPBACK, rings).unwrap());
    let rx = Receiver::new(Arc::clone(&s)).unwrap();
    let plan = RxPlan {
        src: Some(LOOPBACK),
        bps: 3,
        nslots: 1,
        latency: LATENCY,
        delay: 0,
        block: 0,
        live: false,
        generation: 1,
        group,
    };
    s.publish(
        |slot| if slot == 2 { plan } else { RxPlan::default() },
        |rx| (rx == 0).then_some(RxRoute { flow: 2, slot: 0 }),
    );
    (s, rx)
}

/// A one-slot 24-bit packet of 16 frames from position `pos`.
fn packet_at(pos: u64) -> Vec<u8> {
    let mut out = vec![0u8; MAX_DATAGRAM];
    let (secs, sub) = crate::clock::stamp(pos, RATE);
    let n = write(&mut out, secs, sub, 1, 3, 16, |_, _| 0);
    out.truncate(n);
    out
}

/// Passes at PTP time `now` until flow slot 2 has `n` packets (bounded).
fn pump(rx: &mut Receiver, n: u64, now: u64) -> RxCounts {
    let deadline = Instant::now() + BOUND;
    loop {
        rx.wait(Duration::from_millis(20)).unwrap();
        rx.pass(Some(now), 0);
        let c = rx.shared.lane(2);
        if c.packets >= n {
            return c;
        }
        assert!(Instant::now() < deadline, "only {c:?}");
    }
}

fn send(to: SocketAddr, pos: u64) {
    let t = UdpSocket::bind((LOOPBACK, 0)).unwrap();
    t.send_to(&packet_at(pos), to).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn a_packet_that_waited_in_its_socket_is_late_by_the_wake_not_on_the_wire() {
    let (s, mut rx) = receiver(None);
    assert!(s.stamps());
    // 1 000 samples (10.4 ms) late at the pass, past the flow's 192; it
    // sits in the socket 30 ms (2 880 samples) before the pass reads it, so
    // it was on time when the kernel received it.
    let now = ns_of(P0 + 1_000, RATE);
    send(s.local_addr(2).unwrap(), P0);
    std::thread::sleep(Duration::from_millis(30));
    let c = pump(&mut rx, 1, now);
    assert_eq!((c.packets, c.late, c.wire_late), (1, 1, 0), "{c:?}");
    assert!(c.wake_delay_peak_us >= 30_000, "{c:?}");
    // dstack #381: its stamp was taken.
    assert_eq!(c.stamped, 1, "{c:?}");
    // The heartbeat's take hands the second's largest on to the status.
    assert_eq!(s.last_wake_delay_us(2), 0);
    s.take_max_lateness(2);
    let second = s.last_wake_delay_us(2);
    assert!(second >= 30_000, "{second}");
    assert!(second <= c.wake_delay_peak_us, "{second} {c:?}");
    // A second without packets: none that second, the peak kept.
    s.take_max_lateness(2);
    assert_eq!(s.last_wake_delay_us(2), 0);
    assert_eq!(s.lane(2).wake_delay_peak_us, c.wake_delay_peak_us);
    assert_eq!(s.lane(1), RxCounts::default());
}

#[cfg(target_os = "linux")]
#[test]
fn a_packet_already_late_when_the_kernel_got_it_counts_wire_late() {
    let (s, mut rx) = receiver(None);
    // A whole second late at the pass: no wake of this test explains it.
    let now = ns_of(P0 + u64::from(RATE), RATE);
    send(s.local_addr(2).unwrap(), P0);
    let c = pump(&mut rx, 1, now);
    assert_eq!((c.packets, c.late, c.wire_late), (1, 1, 1), "{c:?}");
    assert!(c.wake_delay_peak_us < 1_000_000, "{c:?}");
}

#[cfg(target_os = "linux")]
#[test]
fn a_packet_on_time_at_the_pass_counts_nothing_late() {
    let (s, mut rx) = receiver(None);
    // 100 samples late at the pass, within the flow's 192.
    let now = ns_of(P0 + 100, RATE);
    send(s.local_addr(2).unwrap(), P0);
    let c = pump(&mut rx, 1, now);
    assert_eq!(
        (c.packets, c.late, c.wire_late, c.late_at_read),
        (1, 0, 0, 0),
        "{c:?}"
    );
    // A wake was still measured: the pass came after the kernel's receive.
    s.take_max_lateness(2);
    assert!(s.last_wake_delay_us(2) <= c.wake_delay_peak_us);
}

#[cfg(target_os = "linux")]
#[test]
fn a_multicast_flow_s_packets_carry_their_receive_time_too() {
    let port = UdpSocket::bind((LOOPBACK, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let group = SocketAddrV4::new(Ipv4Addr::new(239, 255, 77, 9), port);
    let (s, mut rx) = receiver(Some(group));
    rx.pass(None, 0);
    assert_eq!((s.lane(2).joins, s.lane(2).join_errors), (1, 0));
    let t = UdpSocket::bind((LOOPBACK, 0)).unwrap();
    socket2::SockRef::from(&t)
        .set_multicast_if_v4(&LOOPBACK)
        .unwrap();
    t.set_multicast_loop_v4(true).unwrap();
    t.send_to(&packet_at(P0), group).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    let c = pump(&mut rx, 1, ns_of(P0 + 1_000, RATE));
    assert_eq!((c.packets, c.late, c.wire_late), (1, 1, 0), "{c:?}");
    assert!(c.wake_delay_peak_us >= 30_000, "{c:?}");
}

#[cfg(not(target_os = "linux"))]
#[test]
fn without_receive_stamps_no_wake_and_no_wire_lateness_are_counted() {
    let (s, mut rx) = receiver(None);
    assert!(!s.stamps());
    send(s.local_addr(2).unwrap(), P0);
    std::thread::sleep(Duration::from_millis(30));
    let c = pump(&mut rx, 1, ns_of(P0 + u64::from(RATE), RATE));
    assert_eq!((c.packets, c.late), (1, 1), "{c:?}");
    assert_eq!((c.wire_late, c.wake_delay_peak_us), (0, 0), "{c:?}");
    s.take_max_lateness(2);
    assert_eq!(s.last_wake_delay_us(2), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn with_the_stamps_off_a_linux_pass_measures_as_windows_does() {
    let (s, mut rx) = receiver(None);
    s.set_stamps(false);
    assert!(!s.stamps());
    send(s.local_addr(2).unwrap(), P0);
    std::thread::sleep(Duration::from_millis(30));
    let c = pump(&mut rx, 1, ns_of(P0 + u64::from(RATE), RATE));
    assert_eq!((c.packets, c.late), (1, 1), "{c:?}");
    assert_eq!((c.wire_late, c.wake_delay_peak_us), (0, 0), "{c:?}");
    // dstack #381: the kernel stamped it, but no stamp was taken.
    assert_eq!(c.stamped, 0, "{c:?}");
    s.take_max_lateness(2);
    assert_eq!(s.last_wake_delay_us(2), 0);
}

/// dstack #234: once a minute each flow with stamped packets gets one INFO
/// line of its minute's wake delay (p50, p99, p99.9, largest) and network
/// latency (least, p99, largest), and the status's wake percentiles follow
/// the last whole minute. Without stamps (Windows) neither exists.
#[test]
fn the_minute_watch_logs_each_flow_s_wake_and_network_latency() {
    use crate::media::late::{MINUTE_NS, RxMinute};
    let (s, mut rx) = receiver(None);
    let named = |slot: usize| (slot == 2).then(|| "FOH-Card".to_string());
    let info = |f: &mut dyn FnMut()| crate::test_log::capture_at(tracing::Level::INFO, f);
    let mut minute = RxMinute::new();
    // A packet before the first look: that first (shorter) minute is
    // taken, not logged; the status shows it.
    send(s.local_addr(2).unwrap(), P0);
    pump(&mut rx, 1, ns_of(P0 + 100, RATE));
    assert_eq!(s.last_wake_minute(2), None, "before any look");
    assert_eq!(info(&mut || minute.watch(0, &s, None, named)), "");
    assert_eq!(s.last_wake_minute(2).is_some(), s.stamps());
    // The minute: one packet that sat 30 ms in its socket, one read at once.
    send(s.local_addr(2).unwrap(), P0 + 16);
    std::thread::sleep(Duration::from_millis(30));
    pump(&mut rx, 2, ns_of(P0 + 100, RATE));
    send(s.local_addr(2).unwrap(), P0 + 32);
    pump(&mut rx, 3, ns_of(P0 + 100, RATE));
    let log = info(&mut || minute.watch(MINUTE_NS, &s, None, named));
    if !s.stamps() {
        assert_eq!(log, "", "no stamps, no timing");
        assert_eq!(s.last_wake_minute(2), None);
        return;
    }
    assert_eq!(log.lines().count(), 1, "{log}");
    assert!(log.contains(" INFO "), "{log}");
    assert!(log.contains("RX flow timing in the last minute"), "{log}");
    assert!(
        log.contains("device=FOH-Card source=127.0.0.1:") && log.contains(" slot=2 packets=2 "),
        "{log}"
    );
    let field = |name: &str| -> u64 {
        let at = log
            .find(&format!(" {name}="))
            .unwrap_or_else(|| panic!("{name}: {log}"));
        let rest = &log[at + name.len() + 2..];
        rest.split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap()
            .parse()
            .unwrap()
    };
    let wake = [
        field("wake_delay_p50_us"),
        field("wake_delay_p99_us"),
        field("wake_delay_p999_us"),
        field("wake_delay_max_us"),
    ];
    // Two packets: the 50th percentile is the quick one, the rest the
    // 30 ms one.
    assert!(wake[0] < 30_000, "{log}");
    assert!(
        wake[1] >= 30_000 && wake[1] == wake[2] && wake[2] == wake[3],
        "{log}"
    );
    let net = [
        field("network_latency_min_us"),
        field("network_latency_p99_us"),
        field("network_latency_max_us"),
    ];
    assert!(net[0] <= net[1] && net[1] <= net[2], "{log}");
    assert_eq!(s.last_wake_minute(2), Some([wake[0], wake[1], wake[2]]));
    // A minute without packets: no line, and the status shows none.
    assert_eq!(
        info(&mut || minute.watch(2 * MINUTE_NS, &s, None, named)),
        ""
    );
    assert_eq!(s.last_wake_minute(2), None);
}

/// With the stamps off on Linux (as on Windows), the minute holds nothing.
#[test]
fn without_stamps_the_minute_has_no_timing() {
    let (s, mut rx) = receiver(None);
    s.set_stamps(false);
    send(s.local_addr(2).unwrap(), P0);
    pump(&mut rx, 1, ns_of(P0 + 100, RATE));
    assert_eq!(s.take_minute(2), (None, None));
    assert_eq!(s.take_minute(crate::media::rx::MAX_FLOWS), (None, None));
    assert_eq!(s.last_wake_minute(crate::media::rx::MAX_FLOWS), None);
}

/// dstack #234: each stamped packet's write delay (the kernel's receive
/// time to the end of its ring writes) is in the minute line beside the
/// wake delay, as its 99.9th percentile and largest, and in the status's
/// last minute; never under the wake delay of the same packets. Without
/// stamps there is none.
#[test]
fn the_minute_line_carries_each_packet_s_write_delay() {
    use crate::media::late::{MINUTE_NS, RxMinute};
    let (s, mut rx) = receiver(None);
    let info = |f: &mut dyn FnMut()| crate::test_log::capture_at(tracing::Level::INFO, f);
    let mut minute = RxMinute::new();
    assert_eq!(info(&mut || minute.watch(0, &s, None, |_| None)), "");
    assert_eq!(s.last_write_minute(2), None, "an empty first minute");
    send(s.local_addr(2).unwrap(), P0);
    std::thread::sleep(Duration::from_millis(30));
    pump(&mut rx, 1, ns_of(P0 + 100, RATE));
    send(s.local_addr(2).unwrap(), P0 + 16);
    pump(&mut rx, 2, ns_of(P0 + 100, RATE));
    let log = info(&mut || minute.watch(MINUTE_NS, &s, None, |_| None));
    if !s.stamps() {
        assert_eq!(s.last_write_minute(2), None);
        return;
    }
    let field = |name: &str| -> u64 {
        let at = log
            .find(&format!(" {name}="))
            .unwrap_or_else(|| panic!("{name}: {log}"));
        log[at + name.len() + 2..]
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap()
            .parse()
            .unwrap()
    };
    let (p999, max) = (field("write_delay_p999_us"), field("write_delay_max_us"));
    assert!(log.contains(" wake_delay_max_us="), "{log}");
    assert!(max >= 30_000 && p999 == max, "{log}");
    assert!(max >= field("wake_delay_max_us"), "{log}");
    assert!(
        log.find(" wake_delay_max_us=").unwrap() < log.find(" write_delay_p999_us=").unwrap()
            && log.find(" write_delay_max_us=").unwrap()
                < log.find(" network_latency_min_us=").unwrap(),
        "beside the wake delay: {log}"
    );
    assert_eq!(s.last_write_minute(2), Some([p999, max]));
    // A minute without packets: no write delay.
    minute.watch(2 * MINUTE_NS, &s, None, |_| None);
    assert_eq!(s.last_write_minute(2), None);
    // A packet that reaches no ring (its slot feeds no channel) has a wake
    // delay but no write delay.
    let plan = RxPlan {
        src: Some(LOOPBACK),
        bps: 3,
        nslots: 1,
        latency: LATENCY,
        delay: 0,
        block: 0,
        live: false,
        generation: 1,
        group: None,
    };
    s.publish(
        |slot| if slot == 2 { plan } else { RxPlan::default() },
        |_| None,
    );
    send(s.local_addr(2).unwrap(), P0 + 32);
    pump(&mut rx, 3, ns_of(P0 + 100, RATE));
    let (wake, _) = s.take_minute(2);
    assert_eq!(wake.map(|w| w.packets), Some(1));
    assert_eq!(s.take_write_minute(2), None);
    // With the stamps off nothing is recorded.
    s.set_stamps(false);
    send(s.local_addr(2).unwrap(), P0 + 48);
    pump(&mut rx, 4, ns_of(P0 + 100, RATE));
    assert_eq!(s.take_write_minute(2), None);
    assert_eq!(s.take_write_minute(crate::media::rx::MAX_FLOWS), None);
    assert_eq!(s.last_write_minute(crate::media::rx::MAX_FLOWS), None);
}
