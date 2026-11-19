// SPDX-License-Identifier: MIT OR Apache-2.0

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};

use super::*;

#[test]
fn slots_count_up_from_dante_s_defaults() {
    assert_eq!(
        slot(0),
        (
            Ports {
                arc: 4440,
                cmc: 8800,
                flows: 4455
            },
            0
        )
    );
    assert_eq!(
        slot(2),
        (
            Ports {
                arc: 4442,
                cmc: 8802,
                flows: 4457
            },
            2
        )
    );
    // The last slot's ARC stays below the first flows port.
    assert!(slot(SLOTS - 1).0.arc < slot(0).0.flows);
}

/// dasio's FOH: dlisten holds slot 0, band slot 1; dsend gets slot 2.
#[test]
fn the_first_slot_whose_ports_are_free_is_taken() {
    let taken = [4440, 8800, 4455, 4441, 8801, 4456];
    let got = first_free(|p| ![p.arc, p.cmc, p.flows].iter().any(|x| taken.contains(x)));
    assert_eq!(got, Some(slot(2)));
    // One port of a slot taken is enough to skip it.
    let got = first_free(|p| p.cmc != 8800);
    assert_eq!(got, Some(slot(1)));
    assert_eq!(first_free(|_| false), None);
}

#[test]
fn a_port_held_on_the_address_does_not_bind() {
    let held = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = held.local_addr().unwrap().port();
    let free = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
    let other = free.local_addr().unwrap().port();
    drop(free);
    let p = Ports {
        arc: other,
        cmc: other,
        flows: other,
    };
    assert!(binds(Ipv4Addr::LOCALHOST, &p));
    let p = Ports {
        arc: other,
        cmc: port,
        flows: other,
    };
    assert!(!binds(Ipv4Addr::LOCALHOST, &p));
}

/// A fake bind refusing `held` (address and port); every other binds.
fn refusing(held: &[(Ipv4Addr, u16)]) -> impl Fn(SocketAddrV4) -> bool + '_ {
    move |at| !held.contains(&(*at.ip(), at.port()))
}

const OURS: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 10);
const ANY: Ipv4Addr = Ipv4Addr::UNSPECIFIED;

/// dstack #200: Audinate's ConMon service (Dante Controller, DVS) holds
/// 0.0.0.0:8700 and 0.0.0.0:8800 on a Windows PC. Windows lets a socket
/// bind the PC's address beside it and hands it the unicast to that
/// address, so a device on slot 0 took ConMon's CMC replies and Dante
/// Controller on the PC read no device. A port held on the wildcard
/// address is used: the slot is skipped.
#[test]
fn a_port_held_on_the_wildcard_address_skips_the_slot() {
    let conmon = [(ANY, 8700), (ANY, 8800)];
    assert!(!binds_with(OURS, &slot(0).0, refusing(&conmon)));
    assert_eq!(
        first_free(|p| binds_with(OURS, p, refusing(&conmon))),
        Some(slot(1))
    );
    // Each of the slot's ports counts, on the wildcard address.
    for port in [4440, 8800, 4455] {
        assert!(
            !binds_with(OURS, &slot(0).0, refusing(&[(ANY, port)])),
            "{port}"
        );
    }
}

/// Our own devices hold their ports on the address: slots 0, 1, 2... as
/// before; with nothing held, slot 0 (Dante's defaults).
#[test]
fn our_devices_on_the_address_still_count_up_from_slot_0() {
    assert_eq!(
        first_free(|p| binds_with(OURS, p, refusing(&[]))),
        Some(slot(0))
    );
    let first = [(OURS, 4440), (OURS, 8800), (OURS, 4455)];
    assert_eq!(
        first_free(|p| binds_with(OURS, p, refusing(&first))),
        Some(slot(1))
    );
    // One port of the slot held on the address is enough.
    assert!(!binds_with(OURS, &slot(0).0, refusing(&[(OURS, 8800)])));
    // Another address's holder is not ours to avoid.
    let other = Ipv4Addr::new(192, 0, 2, 11);
    assert!(binds_with(OURS, &slot(0).0, refusing(&[(other, 8800)])));
}

/// On the real sockets: a port held on the wildcard address is not free
/// for the device on loopback either (Linux refuses that bind itself;
/// Windows needs the wildcard probe).
#[test]
fn a_port_held_on_the_wildcard_address_does_not_bind() {
    let held = UdpSocket::bind(SocketAddrV4::new(ANY, 0)).unwrap();
    let port = held.local_addr().unwrap().port();
    let p = Ports {
        arc: port,
        cmc: port,
        flows: port,
    };
    assert!(!binds(Ipv4Addr::LOCALHOST, &p));
}

#[test]
fn given_ports_win_and_a_named_interface_keeps_the_defaults() {
    let given = slot(5);
    assert_eq!(
        pick(&Place::Ip(Ipv4Addr::LOCALHOST), Some(given.0), Some(5)),
        (Some(given.0), Some(5))
    );
    assert_eq!(
        pick(&Place::Ip(Ipv4Addr::LOCALHOST), None, Some(3)),
        (None, Some(3))
    );
    assert_eq!(pick(&Place::Iface("eth1".into()), None, None), (None, None));
}

#[test]
fn on_an_address_the_first_free_slot_is_picked() {
    // Slot 0's ARC held on loopback: slot 0 is skipped unless it was free.
    let held = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 4440));
    let (p, id) = pick(&Place::Ip(Ipv4Addr::LOCALHOST), None, None);
    let (p, id) = (p.expect("a free slot"), id.expect("its id"));
    assert_eq!(slot(id), (p, id));
    if held.is_ok() {
        assert!(id >= 1, "slot 0's ARC is held");
    }
}

#[test]
fn the_place_is_waited_for_before_its_ports_are_probed() {
    // dstack #225: probed before the address is on an interface, every
    // slot fails to bind and the device keeps Dante's defaults, beside
    // another device of the PC once the address is there.
    let given = slot(5);
    let place = Place::Ip(Ipv4Addr::LOCALHOST);
    let waited = std::cell::Cell::new(0);
    let got = pick_when_there(&place, Some(given.0), Some(5), |p| {
        assert_eq!(p, &place);
        waited.set(waited.get() + 1);
        Ok(())
    });
    assert_eq!(got, Ok((Some(given.0), Some(5))));
    assert_eq!(waited.get(), 1);
    let iface = Place::Iface("eth1".into());
    let got = pick_when_there(&iface, None, None, |p| {
        assert_eq!(p, &iface);
        waited.set(waited.get() + 1);
        Ok(())
    });
    assert_eq!(got, Ok((None, None)));
    assert_eq!(waited.get(), 2);
    // A wait that ends in an error (a place refused) is the pick's.
    assert_eq!(
        pick_when_there(&place, None, None, |_| Err("refused".to_string())),
        Err("refused".to_string())
    );
}
