// SPDX-License-Identifier: MIT OR Apache-2.0

use std::net::{Ipv4Addr, SocketAddrV4};

use super::call;

#[test]
fn the_call_runs_on_a_socket_from_the_arc_s_address_and_gives_its_result() {
    // Port 9 (discard): the closure sends nothing; the socket only opens.
    let arc = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9);
    assert_eq!(call(arc, |_| Ok(7)), Ok(7));
    assert_eq!(
        call::<u8>(arc, |_| Err("refused".to_string())),
        Err("refused".to_string())
    );
}

#[test]
fn an_address_not_on_this_pc_opens_no_socket() {
    // A documentation address (RFC 5737): no interface of the box has it.
    let arc = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 1), 4440);
    let err = call(arc, |_| Ok(7)).unwrap_err();
    assert!(err.starts_with("no socket: "), "{err}");
}
