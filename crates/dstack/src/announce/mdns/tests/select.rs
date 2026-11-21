// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #451: the daemon's interface selection by address.

use super::*;

/// An interface named `name` with index `index` and address `ip`.
fn interface(name: &str, index: u32, ip: Ipv4Addr) -> if_addrs::Interface {
    if_addrs::Interface {
        name: name.into(),
        addr: if_addrs::IfAddr::V4(if_addrs::Ifv4Addr {
            ip,
            netmask: Ipv4Addr::new(255, 255, 255, 0),
            prefixlen: 24,
            broadcast: None,
        }),
        index: Some(index),
        oper_status: if_addrs::IfOperStatus::Up,
        is_p2p: false,
        #[cfg(windows)]
        adapter_name: String::new(),
    }
}

/// dstack #451: the daemon's selection holds the interface with the
/// address whatever its index or name, so the one re-created under a new
/// index (a USB NIC's reset; CI's `recreate` job: rig0 2 -> 3, the device
/// answered nothing for 60 s) is its interface again; never another
/// address, also not one on the same interface. The pure match only: its
/// wiring into the daemon (`Mdns::open`'s `IfPredicate`) needs an interface
/// re-created, which only CI's root `recreate` job does.
#[test]
fn holds_address_matches_the_address_whatever_the_index_or_name() {
    let ip = Ipv4Addr::new(10, 71, 0, 1);
    assert!(holds_address(&interface("rig0", 2, ip), ip), "the first");
    assert!(holds_address(&interface("rig0", 3, ip), ip), "re-created");
    assert!(holds_address(&interface("rig1", 9, ip), ip), "renamed");
    let other = Ipv4Addr::new(10, 71, 0, 2);
    assert!(
        !holds_address(&interface("rig0", 3, other), ip),
        "another address"
    );
}
