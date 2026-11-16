// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The receive path off Linux (Windows): `recv_from` as before, no kernel
//! receive stamp (dstack #234). The RX thread measures no wake delay and no
//! wire lateness here, and the status leaves those keys out.

use std::io;
use std::mem::ManuallyDrop;
use std::net::{SocketAddr, UdpSocket};

use super::Received;

/// No receive stamp is asked for off Linux.
pub fn stamp_receives(_s: &UdpSocket) -> bool {
    false
}

/// A flow socket's OS handle as one word for the writer's handle table
/// (dstack #326): the handle plus 1, so 0 is none.
#[cfg(windows)]
pub fn handle_of(s: &impl std::os::windows::io::AsRawSocket) -> u64 {
    s.as_raw_socket().wrapping_add(1)
}

/// A flow socket's descriptor as one word (dstack #326): plus 1, 0 none.
#[cfg(not(windows))]
pub fn handle_of(s: &impl std::os::fd::AsRawFd) -> u64 {
    u64::try_from(s.as_raw_fd()).map_or(0, |fd| fd + 1)
}

/// The socket of a [`handle_of`] handle, borrowed: never closed here.
#[cfg(windows)]
fn borrow(raw: u64) -> io::Result<ManuallyDrop<UdpSocket>> {
    use std::os::windows::io::FromRawSocket;
    // SAFETY: `raw` is a socket the RX thread keeps open until the writer
    // has moved past its swap (`super::super::handles`); `ManuallyDrop`
    // never closes it.
    Ok(ManuallyDrop::new(unsafe {
        UdpSocket::from_raw_socket(raw)
    }))
}

/// The socket of a [`handle_of`] handle, borrowed: never closed here.
#[cfg(not(windows))]
fn borrow(raw: u64) -> io::Result<ManuallyDrop<UdpSocket>> {
    use std::os::fd::FromRawFd;
    let fd = i32::try_from(raw).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: as the Windows twin: open for the call, never closed here.
    Ok(ManuallyDrop::new(unsafe { UdpSocket::from_raw_fd(fd) }))
}

/// Reads one datagram of the socket whose [`handle_of`] is `handle`, with
/// no receive time; an invalid handle (0) is an `InvalidInput` error.
pub fn recv_handle(handle: u64, buf: &mut [u8]) -> io::Result<Received> {
    let raw = handle
        .checked_sub(1)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let s = borrow(raw)?;
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
