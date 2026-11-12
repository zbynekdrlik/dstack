// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The receive path off Linux (Windows): `recv_from` as before, no kernel
//! receive stamp (dstack #234). The RX thread measures no wake delay and no
//! wire lateness here, and the status leaves those keys out.

use std::io;
use std::net::{SocketAddr, UdpSocket};

use super::Received;

/// No receive stamp is asked for off Linux.
pub fn stamp_receives(_s: &UdpSocket) -> bool {
    false
}

/// Reads one datagram of `s` into `buf`, with no receive time.
pub fn recv_stamped(s: &mio::net::UdpSocket, buf: &mut [u8]) -> io::Result<Received> {
    let (len, from) = s.recv_from(buf)?;
    Ok(Received {
        len,
        from: match from {
            SocketAddr::V4(a) => Some(a),
            SocketAddr::V6(_) => None,
        },
        at_ns: None,
    })
}
