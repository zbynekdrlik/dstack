// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The receive path on an OS that is neither Linux nor Windows: `recv_from`,
//! no receive stamp (dstack #234, #381). The RX thread measures no wake
//! delay and no wire lateness here, and the status leaves those keys out.

use std::io;
use std::mem::ManuallyDrop;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::os::fd::{AsRawFd, FromRawFd};

use super::{Received, Stamping, stamping_of};

/// No receive stamp is asked for here.
pub fn stamp_receives(_s: &UdpSocket) -> bool {
    false
}

/// The box's [`Stamping`]: never stamped here.
pub fn stamping(_ip: Ipv4Addr, accepted: bool) -> Stamping {
    stamping_of(false, accepted, None)
}

/// A flow socket's descriptor as one word (dstack #326): plus 1, 0 none.
pub fn handle_of(s: &impl AsRawFd) -> u64 {
    u64::try_from(s.as_raw_fd()).map_or(0, |fd| fd + 1)
}

/// A datagram and its sender as [`Received`], with no receive time.
fn received(len: usize, from: SocketAddr) -> Received {
    Received {
        len,
        from: match from {
            SocketAddr::V4(a) => Some(a),
            SocketAddr::V6(_) => None,
        },
        at_ns: None,
    }
}

/// Reads one datagram of a socket the RX thread's poll registered, with no
/// receive time.
pub fn recv_stamped(s: &mio::net::UdpSocket, buf: &mut [u8]) -> io::Result<Received> {
    let (len, from) = s.recv_from(buf)?;
    Ok(received(len, from))
}

/// Reads one datagram of the socket whose [`handle_of`] is `handle`, with
/// no receive time; an invalid handle (0) is an `InvalidInput` error.
pub fn recv_handle(handle: u64, buf: &mut [u8]) -> io::Result<Received> {
    let fd = handle
        .checked_sub(1)
        .and_then(|h| i32::try_from(h).ok())
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: open for the call (`super::super::handles`), never closed here.
    let s = ManuallyDrop::new(unsafe { UdpSocket::from_raw_fd(fd) });
    let (len, from) = s.recv_from(buf)?;
    Ok(received(len, from))
}
