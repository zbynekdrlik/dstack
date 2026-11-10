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
