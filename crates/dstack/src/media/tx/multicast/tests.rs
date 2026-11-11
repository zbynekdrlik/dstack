use super::*;

use crate::device::DeviceConfig;
use crate::flows::control::{FlowRequest, handle};
use crate::media::tx::{KEEPALIVE_TIMEOUT_NS, TxShared};

const TEST_DEVICE: &str = include_str!("../../../../../../config/test-device.toml");
const OUR_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 10);

/// A group of ours: process 1, slot 2 of 10.0.0.10.
fn group() -> SocketAddrV4 {
    multicast_group(OUR_IP, 1, 2)
}

/// A receiver's unicast flow request to `rx`.
fn unicast(rx: SocketAddrV4) -> FlowRequest {
    FlowRequest {
        rx_host: "peer-device".to_string(),
        rx_flow_name: "flow-1".to_string(),
        sample_rate: 96_000,
        bits: 24,
        channels: vec![Some(1), Some(2)],
        fpp: 32,
        rx,
    }
}

#[test]
fn the_group_is_239_255_our_last_byte_and_process_times_32_plus_slot() {
    assert_eq!(
        multicast_group(OUR_IP, 1, 2),
        SocketAddrV4::new(Ipv4Addr::new(239, 255, 10, 34), MULTICAST_PORT)
    );
    assert_eq!(
        multicast_group(Ipv4Addr::new(192, 168, 70, 23), 0, 0),
        SocketAddrV4::new(Ipv4Addr::new(239, 255, 23, 0), 4321)
    );
    // The process id counts mod 8, the slot under 32: process 7, slot 31
    // is the last group of the /24's host byte.
    assert_eq!(
        multicast_group(OUR_IP, 7, 31).ip(),
        &Ipv4Addr::new(239, 255, 10, 255)
    );
    assert_eq!(multicast_group(OUR_IP, 9, 5), multicast_group(OUR_IP, 1, 5));
    // The same every time, another for another slot or process.
    assert_eq!(multicast_group(OUR_IP, 3, 4), multicast_group(OUR_IP, 3, 4));
    assert_ne!(multicast_group(OUR_IP, 3, 4), multicast_group(OUR_IP, 3, 5));
    assert_ne!(multicast_group(OUR_IP, 3, 4), multicast_group(OUR_IP, 4, 4));
    assert!(group().ip().is_multicast());
}

#[test]
fn a_multicast_flow_takes_its_slot_and_goes_to_its_group() {
    let mut t = TxTable::new(1);
    assert_eq!(t.add_multicast(2, group(), &[3, 1], 32, 3), Ok(()));
    let f = t.flow(2).expect("the flow in slot 2");
    assert_eq!(f.dest, group());
    assert_eq!(f.channels, [Some(3), Some(1)]);
    assert_eq!((f.fpp, f.bps), (32, 3));
    assert!(f.multicast && !f.expired);
    assert!(f.rx_host.is_empty() && f.rx_flow_name.is_empty());
    assert_eq!(t.flows().count(), 1);
    // The TX thread sends it: to the group, with its channels and format.
    let plan = t.plan(2);
    assert_eq!(plan.dest, Some(group()));
    assert_eq!((plan.fpp, plan.bps, plan.nch), (32, 3, 2));
    assert_eq!(plan.channels[..3], [3, 1, 0]);
}

#[test]
fn a_multicast_flow_is_refused_past_the_slots_or_in_a_held_slot() {
    let mut t = TxTable::with_slots(1, 4);
    assert_eq!(
        t.add_multicast(4, group(), &[1], 32, 3),
        Err("a slot past the device's flow slots")
    );
    assert_eq!(t.add_multicast(3, group(), &[1], 32, 3), Ok(()), "the last slot");
    assert_eq!(
        t.add_multicast(3, group(), &[2], 32, 3),
        Err("a slot that holds a flow")
    );
    // A receiver's flow holds its slot too, expired or not.
    let (slot, _) = t.add(&unicast(SocketAddrV4::new(OUR_IP, 4000)), 0).unwrap();
    t.expire(KEEPALIVE_TIMEOUT_NS);
    assert!(t.flow(slot).unwrap().expired);
    assert_eq!(
        t.add_multicast(slot, group(), &[1], 32, 3),
        Err("a slot that holds a flow")
    );
    assert_eq!(t.flow(3).unwrap().channels, [Some(1)], "the first kept");
}

#[test]
fn a_multicast_flow_never_expires_and_no_receiver_s_handle_reaches_it() {
    let mut t = TxTable::new(1);
    t.add_multicast(0, group(), &[1, 2], 32, 3).unwrap();
    assert_eq!(t.expire(10 * KEEPALIVE_TIMEOUT_NS), 0);
    assert!(!t.flow(0).unwrap().expired);
    let cookie = t.flow(0).unwrap().cookie;
    assert_eq!(t.get(&handle(0, cookie)), None);
    // A receiver's flow in the next slot still expires and is reached.
    let (slot, h) = t.add(&unicast(SocketAddrV4::new(OUR_IP, 4000)), 0).unwrap();
    assert_eq!(slot, 1, "the multicast slot is taken");
    assert_eq!(t.get(&h).map(|(s, _)| s), Some(1));
    assert_eq!(t.expire(KEEPALIVE_TIMEOUT_NS), 1 << 1);
}

#[test]
fn a_delete_removes_only_a_multicast_flow() {
    let mut t = TxTable::new(1);
    t.add_multicast(0, group(), &[1], 32, 3).unwrap();
    let (slot, _) = t.add(&unicast(SocketAddrV4::new(OUR_IP, 4000)), 0).unwrap();
    assert!(!t.remove_multicast(slot), "a receiver's flow stays");
    assert!(t.flow(slot).is_some());
    assert!(!t.remove_multicast(5), "an empty slot");
    assert!(!t.remove_multicast(MAX_FLOWS), "past the table");
    assert!(t.remove_multicast(0));
    assert_eq!(t.flow(0), None);
    assert_eq!(t.plan(0).dest, None, "nothing sent");
    assert!(!t.remove_multicast(0), "deleted once");
}

#[test]
fn the_flow_sockets_send_multicast_from_our_address_with_dante_s_ttl() {
    let s = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    multicast_out(&s, Ipv4Addr::LOCALHOST).expect("our address takes multicast");
    let r = socket2::SockRef::from(&s);
    // Audinate's audio packets' TTL.
    assert_eq!(r.multicast_ttl_v4().unwrap(), 32);
    assert_eq!(r.multicast_if_v4().unwrap(), Ipv4Addr::LOCALHOST);
}

#[test]
fn the_receiver_latency_reader_skips_a_multicast_flow() {
    let cfg = DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-device.toml");
    let shared = TxShared::new(cfg, Ipv4Addr::LOCALHOST, Vec::new().into(), 1, &[]).unwrap();
    assert_eq!(shared.ip(), Ipv4Addr::LOCALHOST);
    let rx = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 4000);
    {
        let mut c = shared.lock();
        c.table.add_multicast(0, group(), &[1], 32, 3).unwrap();
        c.table.add(&unicast(rx), 0).unwrap();
        shared.publish(&c);
    }
    assert_eq!(
        shared.receiver_flows(),
        [(1, rx, "peer-device".to_string())]
    );
}
