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

#[test]
fn the_wall_clock_is_now() {
    let wall = wall_now_ns().unwrap();
    let sys = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    assert!(sys.abs_diff(wall) < 1_000_000_000, "{wall} vs {sys}");
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
    assert_eq!(info(&mut || minute.watch(0, &s, named)), "");
    assert_eq!(s.last_wake_minute(2).is_some(), s.stamps());
    // The minute: one packet that sat 30 ms in its socket, one read at once.
    send(s.local_addr(2).unwrap(), P0 + 16);
    std::thread::sleep(Duration::from_millis(30));
    pump(&mut rx, 2, ns_of(P0 + 100, RATE));
    send(s.local_addr(2).unwrap(), P0 + 32);
    pump(&mut rx, 3, ns_of(P0 + 100, RATE));
    let log = info(&mut || minute.watch(MINUTE_NS, &s, named));
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
    assert_eq!(info(&mut || minute.watch(2 * MINUTE_NS, &s, named)), "");
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
    // With the stamps off nothing is recorded.
    s.set_stamps(false);
    send(s.local_addr(2).unwrap(), P0 + 32);
    pump(&mut rx, 3, ns_of(P0 + 100, RATE));
    assert_eq!(s.take_write_minute(2), None);
    assert_eq!(s.take_write_minute(crate::media::rx::MAX_FLOWS), None);
    assert_eq!(s.last_write_minute(crate::media::rx::MAX_FLOWS), None);
}
