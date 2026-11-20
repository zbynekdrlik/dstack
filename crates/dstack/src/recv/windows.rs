// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! [`super::within`]'s wait on Windows (dstack #454): a non-blocking
//! socket waited on with `WSAPoll`, never `SO_RCVTIMEO`. Thin glue: the
//! decisions are `super::read_when_ready` and `super::poll_ms`, tested on
//! every OS.

use std::io;
use std::net::UdpSocket;
use std::os::windows::io::AsRawSocket;
use std::time::{Duration, Instant};

use windows_sys::Win32::Networking::WinSock::{
    POLLRDNORM, SOCKET, SOCKET_ERROR, WSAPOLLFD, WSAPoll,
};

/// `read` on `sock` made non-blocking, waited on for at most `wait`.
pub(super) fn wait_read<T>(
    sock: &UdpSocket,
    wait: Duration,
    mut read: impl FnMut(&UdpSocket) -> io::Result<T>,
) -> io::Result<Option<T>> {
    sock.set_nonblocking(true)?;
    super::read_when_ready(
        Instant::now() + wait,
        || read(sock),
        |left| readable(sock, left),
    )
}

/// Whether `sock` has a datagram (or an error) to read within `wait`.
fn readable(sock: &UdpSocket, wait: Duration) -> io::Result<bool> {
    let mut fd = WSAPOLLFD {
        fd: sock.as_raw_socket() as SOCKET,
        events: POLLRDNORM,
        revents: 0,
    };
    // SAFETY: one valid WSAPOLLFD, alive for the whole call, and its count
    // 1; the socket outlives the call (borrowed).
    let n = unsafe { WSAPoll(&mut fd, 1, super::poll_ms(wait)) };
    if n == SOCKET_ERROR {
        Err(io::Error::last_os_error())
    } else {
        Ok(n > 0)
    }
}
