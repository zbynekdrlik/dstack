// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! A UDP read that waits at most a while (dstack #454): every UDP read loop
//! of the stack that must also look at something else (a stop flag, a
//! resend, a deadline) reads through [`within`].
//!
//! A read with nothing within its wait is `Ok(None)`, never an error, so a
//! loop no longer tells a timeout from a failure by the error's kind (a
//! would-block on Windows, a timed-out read elsewhere).

use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

/// One read of `sock` by `read`, waiting at most `wait` for a datagram:
/// `Ok(None)` when none came within it (and at once for a zero wait),
/// an error when the read failed. `read` is the receive itself
/// (`recv_from`, or `recv` on a connected socket).
pub fn within<T>(
    sock: &UdpSocket,
    wait: Duration,
    read: impl FnMut(&UdpSocket) -> io::Result<T>,
) -> io::Result<Option<T>> {
    if wait.is_zero() {
        return Ok(None);
    }
    wait_read(sock, wait, read)
}

/// [`within`] with `recv_from` into `buf`: the datagram's length and
/// sender.
pub fn recv_from_within(
    sock: &UdpSocket,
    buf: &mut [u8],
    wait: Duration,
) -> io::Result<Option<(usize, SocketAddr)>> {
    within(sock, wait, |s| s.recv_from(buf))
}

/// [`within`] with `recv` into `buf` (a connected socket): the datagram's
/// length.
pub fn recv_within(sock: &UdpSocket, buf: &mut [u8], wait: Duration) -> io::Result<Option<usize>> {
    within(sock, wait, |s| s.recv(buf))
}

/// What one read gave: a datagram, nothing yet (the read would block or
/// its timeout ended), or a failure.
#[derive(Debug)]
enum Read<T> {
    Got(T),
    Nothing,
    Failed(io::Error),
}

/// The read `r` judged: a would-block or a timed-out read is nothing yet,
/// any other error a failure.
fn read_of<T>(r: io::Result<T>) -> Read<T> {
    match r {
        Ok(t) => Read::Got(t),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            Read::Nothing
        }
        Err(e) => Read::Failed(e),
    }
}

/// The read's result as [`within`] gives it.
fn result_of<T>(read: Read<T>) -> io::Result<Option<T>> {
    match read {
        Read::Got(t) => Ok(Some(t)),
        Read::Nothing => Ok(None),
        Read::Failed(e) => Err(e),
    }
}

/// The wait as a read timeout (`SO_RCVTIMEO`), then one read.
fn wait_read<T>(
    sock: &UdpSocket,
    wait: Duration,
    mut read: impl FnMut(&UdpSocket) -> io::Result<T>,
) -> io::Result<Option<T>> {
    sock.set_read_timeout(Some(wait))?;
    result_of(read_of(read(sock)))
}

#[cfg(test)]
mod tests;
