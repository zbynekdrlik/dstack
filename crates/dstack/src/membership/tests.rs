use super::*;

use std::cell::Cell;
use std::net::{SocketAddrV4, UdpSocket};

use tracing::Level;

/// The device's address in these tests.
const OURS: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 31);

fn link(name: &str, ip: Ipv4Addr, index: Option<u32>) -> Link {
    Link {
        name: name.to_string(),
        ip,
        index,
    }
}

/// A two-leg PC: the Dante leg at index `dante`, a VLAN adapter beside it.
fn two_legs(dante: u32) -> Vec<Link> {
    vec![
        link("Ethernet 3", OURS, Some(dante)),
        link("LAN89", Ipv4Addr::new(10, 0, 9, 171), Some(27)),
    ]
}

/// The VLAN adapter only: the Dante leg's address is gone.
fn without_ours() -> Vec<Link> {
    vec![link("LAN89", Ipv4Addr::new(10, 0, 9, 171), Some(27))]
}

fn rejoin_of(old: Option<u32>, new: Option<u32>) -> Option<Rejoin> {
    Some(Rejoin {
        interface: "Ethernet 3".to_string(),
        old_index: old,
        new_index: new,
    })
}

#[test]
fn the_first_look_learns_and_nothing_changing_asks_no_rejoin() {
    let mut m = Membership::new(OURS);
    assert_eq!(m.look(&two_legs(26)), None);
    for _ in 0..3 {
        assert_eq!(m.look(&two_legs(26)), None);
    }
}

#[test]
fn an_address_removed_then_added_again_asks_a_rejoin_once() {
    let mut m = Membership::new(OURS);
    assert_eq!(m.look(&two_legs(26)), None);
    // Removed: nothing to join on yet.
    assert_eq!(m.look(&without_ours()), None);
    assert_eq!(m.look(&without_ours()), None);
    // Back, on the same index: the membership went with the address.
    assert_eq!(m.look(&two_legs(26)), rejoin_of(Some(26), Some(26)));
    assert_eq!(m.look(&two_legs(26)), None);
    // Removed again and back on another index (seen live: 26 to 19).
    assert_eq!(m.look(&without_ours()), None);
    assert_eq!(m.look(&two_legs(19)), rejoin_of(Some(26), Some(19)));
    assert_eq!(m.look(&two_legs(19)), None);
}

#[test]
fn an_index_change_with_the_address_kept_asks_a_rejoin() {
    let mut m = Membership::new(OURS);
    assert_eq!(m.look(&two_legs(26)), None);
    assert_eq!(m.look(&two_legs(19)), rejoin_of(Some(26), Some(19)));
    assert_eq!(m.look(&two_legs(19)), None);
}

#[test]
fn another_adapter_coming_and_going_asks_no_rejoin() {
    let mut m = Membership::new(OURS);
    let alone = vec![link("Ethernet 3", OURS, Some(26))];
    assert_eq!(m.look(&alone), None);
    // The VLAN adapter appears unbound, then with a DHCP address.
    let unbound = vec![
        link("Ethernet 3", OURS, Some(26)),
        link("LAN89", Ipv4Addr::new(169, 254, 3, 9), Some(27)),
    ];
    assert_eq!(m.look(&unbound), None);
    assert_eq!(m.look(&two_legs(26)), None);
    assert_eq!(m.look(&alone), None);
}

#[test]
fn an_address_missing_at_the_first_look_asks_a_rejoin_when_it_comes() {
    let mut m = Membership::new(OURS);
    assert_eq!(m.look(&without_ours()), None);
    assert_eq!(m.look(&two_legs(19)), rejoin_of(None, Some(19)));
}

#[test]
fn a_failed_rejoin_is_asked_again_at_the_next_look() {
    let mut m = Membership::new(OURS);
    assert_eq!(m.look(&two_legs(26)), None);
    assert_eq!(m.look(&two_legs(19)), rejoin_of(Some(26), Some(19)));
    m.failed();
    assert_eq!(m.look(&two_legs(19)), rejoin_of(Some(19), Some(19)));
    assert_eq!(m.look(&two_legs(19)), None);
}

#[test]
fn a_poll_reads_the_interfaces_at_most_once_per_its_period() {
    let every = Duration::from_secs(2);
    let mut m = Membership::every(OURS, every);
    let t0 = Instant::now();
    let reads = Cell::new(0);
    let read = |links: Vec<Link>| {
        let reads = &reads;
        move || {
            reads.set(reads.get() + 1);
            Ok(links)
        }
    };
    // The first poll reads at once.
    assert_eq!(m.poll(t0, read(two_legs(26))), None);
    assert_eq!(reads.get(), 1);
    // Within the period: no read at all.
    let not_due = || -> io::Result<Vec<Link>> { panic!("read before its period") };
    assert_eq!(m.poll(t0 + every - Duration::from_millis(1), not_due), None);
    assert_eq!(m.poll(t0 + Duration::from_millis(500), not_due), None);
    // At the period: read, and the change seen.
    assert_eq!(
        m.poll(t0 + every, read(two_legs(19))),
        rejoin_of(Some(26), Some(19))
    );
    assert_eq!(reads.get(), 2);
    // The next period counts from that read.
    assert_eq!(m.poll(t0 + every + every / 2, not_due), None);
    assert_eq!(m.poll(t0 + every + every, read(two_legs(19))), None);
    assert_eq!(reads.get(), 3);
}

#[test]
fn a_failed_read_changes_nothing() {
    let every = Duration::from_millis(10);
    let mut m = Membership::every(OURS, every);
    let t0 = Instant::now();
    assert_eq!(m.poll(t0, || Ok(two_legs(26))), None);
    let failed = || -> io::Result<Vec<Link>> { Err(io::Error::other("no list")) };
    assert_eq!(m.poll(t0 + every, failed), None);
    // Still the index of the last good read: the change is seen next.
    assert_eq!(
        m.poll(t0 + every * 2, || Ok(two_legs(19))),
        rejoin_of(Some(26), Some(19))
    );
}

#[test]
fn the_default_period_is_two_seconds() {
    assert_eq!(EVERY, Duration::from_secs(2));
    let mut m = Membership::new(OURS);
    let t0 = Instant::now();
    assert_eq!(m.poll(t0, || Ok(two_legs(26))), None);
    let not_due = || -> io::Result<Vec<Link>> { panic!("read before its period") };
    assert_eq!(m.poll(t0 + Duration::from_millis(1999), not_due), None);
    assert_eq!(
        m.poll(t0 + EVERY, || Ok(two_legs(19))),
        rejoin_of(Some(26), Some(19))
    );
}

#[test]
fn keep_rejoins_on_a_change_and_logs_it_at_info() {
    let mut m = Membership::every(OURS, Duration::from_millis(10));
    let t0 = Instant::now();
    let group = SocketAddrV4::new(Ipv4Addr::new(224, 0, 0, 251), 5353);
    let no_rejoin = || -> io::Result<()> { panic!("rejoined with no change") };
    let text = crate::test_log::capture_at(Level::INFO, || {
        assert!(!m.keep(t0, || Ok(two_legs(26)), group, no_rejoin));
    });
    assert!(text.is_empty(), "{text}");
    let rejoined = Cell::new(0);
    let text = crate::test_log::capture_at(Level::INFO, || {
        let rejoin = || {
            rejoined.set(rejoined.get() + 1);
            Ok(())
        };
        assert!(m.keep(
            t0 + Duration::from_millis(10),
            || Ok(two_legs(19)),
            group,
            rejoin
        ));
    });
    assert_eq!(rejoined.get(), 1);
    assert!(
        text.contains("multicast group re-joined after an interface change"),
        "{text}"
    );
    assert!(text.contains("interface=Ethernet 3"), "{text}");
    assert!(text.contains("old_index=26"), "{text}");
    assert!(text.contains("new_index=19"), "{text}");
    assert!(text.contains("group=224.0.0.251:5353"), "{text}");
    // Nothing changed since: no rejoin, no line.
    let text = crate::test_log::capture_at(Level::INFO, || {
        assert!(!m.keep(
            t0 + Duration::from_millis(20),
            || Ok(two_legs(19)),
            group,
            no_rejoin
        ));
    });
    assert!(text.is_empty(), "{text}");
}

#[test]
fn keep_warns_on_a_failed_rejoin_and_tries_again_at_the_next_period() {
    let every = Duration::from_millis(10);
    let mut m = Membership::every(OURS, every);
    let t0 = Instant::now();
    assert!(!m.keep(t0, || Ok(two_legs(26)), "RX multicast flows", || Ok(())));
    let text = crate::test_log::capture_warnings(|| {
        let failing = || Err(io::Error::other("no such interface"));
        assert!(!m.keep(
            t0 + every,
            || Ok(two_legs(19)),
            "RX multicast flows",
            failing
        ));
    });
    assert!(text.contains("multicast group not re-joined"), "{text}");
    assert!(text.contains("no such interface"), "{text}");
    // The interfaces are as they were: the rejoin is still asked.
    let rejoined = Cell::new(false);
    let rejoin = || {
        rejoined.set(true);
        Ok(())
    };
    assert!(m.keep(
        t0 + every * 2,
        || Ok(two_legs(19)),
        "RX multicast flows",
        rejoin
    ));
    assert!(rejoined.get());
}

#[test]
fn rejoin_leaves_and_joins_the_group_again_on_the_same_socket() {
    let group = Ipv4Addr::new(239, 255, 77, 82);
    let sock = UdpSocket::bind("0.0.0.0:0").unwrap();
    // Not a member yet: the leave's refusal does not matter.
    rejoin(&sock, group, Ipv4Addr::LOCALHOST).unwrap();
    // A member: left and joined again.
    rejoin(&sock, group, Ipv4Addr::LOCALHOST).unwrap();
    // The join's own error is the rejoin's.
    let not_multicast = Ipv4Addr::new(10, 0, 0, 1);
    assert!(rejoin(&sock, not_multicast, Ipv4Addr::LOCALHOST).is_err());
}

#[test]
fn rejoin_keeps_receiving_the_group() {
    let group = Ipv4Addr::new(239, 255, 77, 83);
    let port = UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let sock = {
        let s = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )
        .unwrap();
        s.set_reuse_address(true).unwrap();
        s.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port).into())
            .unwrap();
        s.join_multicast_v4(&group, &Ipv4Addr::LOCALHOST).unwrap();
        UdpSocket::from(s)
    };
    sock.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    rejoin(&sock, group, Ipv4Addr::LOCALHOST).unwrap();
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket2::SockRef::from(&sender)
        .set_multicast_if_v4(&Ipv4Addr::LOCALHOST)
        .unwrap();
    sender.set_multicast_loop_v4(true).unwrap();
    sender
        .send_to(b"after the rejoin", SocketAddrV4::new(group, port))
        .unwrap();
    let mut buf = [0u8; 64];
    let (n, _) = sock
        .recv_from(&mut buf)
        .expect("the group, after the rejoin");
    assert_eq!(&buf[..n], b"after the rejoin");
}

#[test]
fn links_lists_the_loopback_address_with_its_index() {
    let all = links().unwrap();
    let lo = all
        .iter()
        .find(|l| l.ip == Ipv4Addr::LOCALHOST)
        .expect("127.0.0.1 is listed");
    assert!(lo.index.is_some(), "{lo:?}");
    assert!(!lo.name.is_empty());
}
