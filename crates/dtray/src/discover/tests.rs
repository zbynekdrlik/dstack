// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddrV4};

use dstack::announce::STATUS_PORT_KEY;
use dstack::daemon::IfAddr;
use dstack::discover::Device;

use super::{Local, local_devices, own_addresses, plan, status_port};

// Documentation addresses (RFC 5737) only.
const OWN: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 10);
const OWN2: Ipv4Addr = Ipv4Addr::new(198, 51, 100, 7);
const OTHER: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 99);

fn device(name: &str, ip: Ipv4Addr, ctl: Option<&str>) -> Device {
    let mut txt = BTreeMap::new();
    txt.insert("model".to_string(), "x".to_string());
    if let Some(v) = ctl {
        txt.insert(STATUS_PORT_KEY.to_string(), v.to_string());
    }
    Device {
        name: name.to_string(),
        arc: SocketAddrV4::new(ip, 4440),
        dbc: None,
        device_id: None,
        txt,
    }
}

fn local(name: &str, port: u16) -> Local {
    Local {
        name: name.to_string(),
        port,
    }
}

fn ifa(name: &str, ip: Ipv4Addr) -> IfAddr {
    IfAddr {
        name: name.to_string(),
        ip,
        netmask: Ipv4Addr::new(255, 255, 255, 0),
    }
}

#[test]
fn own_addresses_leave_out_loopback_and_repeats() {
    let ifs = [
        ifa("lo", Ipv4Addr::LOCALHOST),
        ifa("b", OWN2),
        ifa("a", OWN),
        ifa("a2", OWN),
        ifa("lo2", Ipv4Addr::new(127, 0, 1, 1)),
    ];
    assert_eq!(own_addresses(&ifs), vec![OWN, OWN2]);
    assert!(own_addresses(&[]).is_empty());
}

#[test]
fn the_status_port_is_a_port_other_than_0() {
    assert_eq!(status_port(&device("a", OWN, Some("41234"))), Some(41234));
    assert_eq!(status_port(&device("a", OWN, Some("65535"))), Some(65535));
    assert_eq!(status_port(&device("a", OWN, Some("1"))), Some(1));
    assert_eq!(status_port(&device("a", OWN, Some("0"))), None);
    assert_eq!(status_port(&device("a", OWN, Some("65536"))), None);
    assert_eq!(status_port(&device("a", OWN, Some("x"))), None);
    assert_eq!(status_port(&device("a", OWN, Some(""))), None);
    assert_eq!(status_port(&device("a", OWN, None)), None);
}

#[test]
fn local_devices_are_ours_on_this_pc() {
    let found = [
        device("zeta", OWN, Some("5002")),
        // A foreign Dante device on this PC (no status port).
        device("foreign", OWN, None),
        // Ours on another PC.
        device("elsewhere", OTHER, Some("5003")),
        device("alpha", OWN2, Some("5001")),
        // The same device listed by a second discovery.
        device("zeta", OWN, Some("5002")),
    ];
    assert_eq!(
        local_devices(&found, &[OWN, OWN2]),
        vec![local("alpha", 5001), local("zeta", 5002)]
    );
    assert_eq!(local_devices(&found, &[OWN2]), vec![local("alpha", 5001)]);
    assert!(local_devices(&found, &[]).is_empty());
}

#[test]
fn the_plan_starts_new_addresses_and_stops_gone_ones() {
    let (start, stop) = plan(&[OWN, OTHER], &[OWN, OWN2]);
    assert_eq!(start, vec![OWN2]);
    assert_eq!(stop, vec![OTHER]);
    let (start, stop) = plan(&[], &[OWN]);
    assert_eq!((start, stop), (vec![OWN], vec![]));
    let (start, stop) = plan(&[OWN], &[OWN]);
    assert!(start.is_empty() && stop.is_empty());
}
