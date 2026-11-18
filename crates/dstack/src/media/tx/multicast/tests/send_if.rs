// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The TX flow sockets' multicast send interface across an interface
//! re-create (dstack #411, as #409's queries): each flow socket's
//! `IP_MULTICAST_IF` named an interface index that a re-created NIC no
//! longer has, so our own multicast flows sent nothing (ENODEV) until a
//! restart. The test moves a fake list's index and reads the option back.

use super::*;

use std::time::Instant;

use crate::membership::{EVERY, Link, Membership};
use crate::test_log::{capture_at, undecided};

/// The flow sockets of the test device on loopback.
fn shared() -> TxShared {
    let cfg = DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-device.toml");
    TxShared::new(cfg, Ipv4Addr::LOCALHOST, Vec::new().into(), 1, &[]).unwrap()
}

/// Each flow socket's multicast send interface.
fn send_ifs(shared: &TxShared) -> Vec<Ipv4Addr> {
    (shared.flow_sockets().iter())
        .map(|s| socket2::SockRef::from(s).multicast_if_v4().unwrap())
        .collect()
}

/// The option as a re-created interface leaves it: naming nothing.
fn spoil(shared: &TxShared) {
    for s in shared.flow_sockets() {
        socket2::SockRef::from(s)
            .set_multicast_if_v4(&Ipv4Addr::UNSPECIFIED)
            .unwrap();
    }
}

/// The OS's list: loopback's address on the interface at `index`.
fn at(index: u32) -> impl FnOnce() -> std::io::Result<Vec<Link>> {
    move || {
        Ok(vec![Link {
            name: "lo".to_string(),
            ip: Ipv4Addr::LOCALHOST,
            index: Some(index),
        }])
    }
}

#[test]
fn after_an_interface_re_create_every_flow_socket_names_its_send_interface_again() {
    let shared = shared();
    let n = shared.flow_sockets().len();
    assert!(n > 1, "a device with several flow slots");
    let ours = vec![Ipv4Addr::LOCALHOST; n];
    let none = vec![Ipv4Addr::UNSPECIFIED; n];
    assert_eq!(send_ifs(&shared), ours, "set at the start");
    let mut watch = Membership::every(Ipv4Addr::LOCALHOST, EVERY);
    let t0 = Instant::now();
    assert!(
        !shared.keep_send_interface(&mut watch, t0, at(4)),
        "the first look only learns"
    );
    spoil(&shared);
    assert!(!shared.keep_send_interface(&mut watch, t0 + EVERY, at(4)));
    assert_eq!(send_ifs(&shared), none, "nothing while the index holds");
    undecided();
    let log = capture_at(tracing::Level::INFO, || {
        assert!(shared.keep_send_interface(&mut watch, t0 + EVERY * 2, at(9)));
    });
    assert_eq!(send_ifs(&shared), ours, "every socket named again");
    assert!(
        log.contains("multicast send interface named again") && log.contains(FLOW_SOCKETS),
        "{log}"
    );
    // Once per change.
    spoil(&shared);
    assert!(!shared.keep_send_interface(&mut watch, t0 + EVERY * 3, at(9)));
    assert_eq!(send_ifs(&shared), none);
}

#[test]
fn every_flow_socket_is_named_again_on_our_address() {
    let shared = shared();
    spoil(&shared);
    shared.send_on_again().expect("our address takes multicast");
    assert_eq!(
        send_ifs(&shared),
        vec![Ipv4Addr::LOCALHOST; shared.flow_sockets().len()]
    );
}
