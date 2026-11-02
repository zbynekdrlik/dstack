use super::*;

use std::net::SocketAddrV4;

const S: u64 = 1_000_000_000;

fn lo(port: u16) -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
}

#[test]
fn every_message_round_trips_and_junk_is_none() {
    for m in [
        Msg::Register([1, 2, 3, 4, 5, 6, 7, 8]),
        Msg::Request(vec![0xff, 0xff, 0, 0x20]),
        Msg::Request(Vec::new()),
        Msg::Send(Dest::Info, vec![9, 9]),
        Msg::Send(Dest::Heartbeat, vec![7]),
    ] {
        assert_eq!(Msg::decode(&m.encode()), Some(m.clone()), "{m:?}");
    }
    assert_eq!(Msg::decode(&[]), None);
    assert_eq!(Msg::decode(b"X123"), None);
    assert_eq!(Msg::decode(b"R1234567"), None, "an id of 7 bytes");
    assert_eq!(Msg::decode(b"R123456789"), None, "an id of 9 bytes");
}

#[test]
fn the_loopback_port_follows_the_address_s_low_bits() {
    assert_eq!(
        share_port(Ipv4Addr::new(10, 77, 7, 112)),
        SHARE_BASE + 0x370
    );
    assert_eq!(share_port(Ipv4Addr::new(192, 168, 0, 0)), SHARE_BASE);
    assert_eq!(share_port(Ipv4Addr::new(10, 0, 3, 255)), SHARE_BASE + 0x3ff);
}

#[test]
fn a_member_lives_while_it_renews_its_lease() {
    let mut m = Members::default();
    assert!(m.register(lo(5000), [1; 8], 0));
    assert!(m.register(lo(5001), [2; 8], S));
    assert!(m.is_live(&lo(5000), LEASE_NS - 1));
    assert!(!m.is_live(&lo(5000), LEASE_NS), "its lease ran out");
    assert_eq!(m.live(LEASE_NS), [lo(5001)], "the gone one forgotten");
    assert!(!m.is_live(&lo(5000), LEASE_NS));
    // A renewal keeps it.
    m.register(lo(5001), [2; 8], LEASE_NS);
    assert_eq!(m.live(2 * LEASE_NS - 1), [lo(5001)]);
    assert!(!m.is_live(&lo(5002), 0), "never registered");
}

#[test]
fn only_loopback_senders_register_and_at_most_32() {
    let mut m = Members::default();
    let far = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 9), 5000));
    assert!(!m.register(far, [1; 8], 0));
    for p in 0..MAX_MEMBERS as u16 {
        assert!(m.register(lo(6000 + p), [3; 8], 0));
    }
    assert!(!m.register(lo(7000), [4; 8], 0), "full");
    assert!(
        m.register(lo(6000), [3; 8], 1),
        "a member renews at the bound"
    );
    assert_eq!(m.live(1).len(), MAX_MEMBERS);
}
