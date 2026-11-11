// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddrV4};

use dstack::announce::STATUS_PORT_KEY;
use dstack::daemon::IfAddr;
use dstack::discover::Device;

use super::{Local, Remote, local_devices, own_addresses, plan, read, remote};

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
        cmc: None,
        device_id: None,
        txt,
    }
}

fn local(name: &str, port: u16, ip: Ipv4Addr) -> Local {
    Local {
        name: name.to_string(),
        port,
        arc: SocketAddrV4::new(ip, 4440),
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
        vec![local("alpha", 5001, OWN2), local("zeta", 5002, OWN)]
    );
    assert_eq!(
        local_devices(&found, &[OWN2]),
        vec![local("alpha", 5001, OWN2)]
    );
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

#[test]
fn read_gives_each_device_s_status_by_name_in_order() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("a connection");
        let mut buf = [0u8; 16];
        let _ = s.read(&mut buf);
        s.write_all(b"{\"clock\":{}}\n").expect("the answer");
    });
    // A port nothing listens on: a listener bound and dropped.
    let closed = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port();
    let got = read(&[local("up", port, OWN), local("down", closed, OWN)]);
    server.join().expect("the server");
    assert_eq!(got.len(), 2);
    assert_eq!(got[0], ("up".to_string(), Ok("{\"clock\":{}}".to_string())));
    assert_eq!(got[1].0, "down");
    assert!(got[1].1.is_err(), "{got:?}");
    assert!(read(&[]).is_empty());
}

#[test]
fn a_remote_device_is_found_by_name_with_the_address_that_found_it() {
    let found = vec![
        (OWN, vec![device("Here", OWN, Some("5000"))]),
        (
            OWN2,
            vec![
                device("Elsewhere", OTHER, None),
                device("Here", OWN, Some("5000")),
            ],
        ),
    ];
    assert_eq!(
        remote(&found, "elsewhere"),
        Ok(Remote {
            from: OWN2,
            arc: SocketAddrV4::new(OTHER, 4440),
        })
    );
    // Listed by both: the first address that lists it.
    assert_eq!(
        remote(&found, "HERE"),
        Ok(Remote {
            from: OWN,
            arc: SocketAddrV4::new(OWN, 4440),
        })
    );
    for name in ["Elsewhere ", "Else", ""] {
        assert_eq!(
            remote(&found, name),
            Err(format!("the device {name} is not found on the network")),
            "{name:?}"
        );
    }
    assert!(remote(&[], "Here").is_err());
}
