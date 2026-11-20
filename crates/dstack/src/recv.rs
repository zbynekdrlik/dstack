// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! A UDP read that waits at most a while (dstack #454): every UDP read loop
//! of the stack that must also look at something else (a stop flag, a
//! resend, a deadline) reads through [`within`].
//!
//! A read with nothing within its wait is `Ok(None)`, never an error, so a
//! loop no longer tells a timeout from a failure by the error's kind (a
//! would-block on Windows, a timed-out read elsewhere).
//!
//! On Windows no read ever times out inside the receive: the socket is
//! non-blocking and waited on with `WSAPoll` (`recv/windows.rs`). A
//! blocking receive that ends on `SO_RCVTIMEO` can lose the datagram that
//! completes into it as the timeout fires (Microsoft: "If a blocking
//! receive call times out, the connection is in an indeterminate state");
//! under load (windows-2025, 8 busy threads) such reads lost 0.5-6 % of
//! the timeouts that fired, multicast and unicast alike, and the poll
//! none (#454's probe, run 44661855528). The socket stays non-blocking
//! after the first read, sends on it included. Elsewhere the wait is the
//! socket's read timeout, then one read: on Linux that loses nothing under
//! the same load (`tests/recv_under_load.rs` runs on every OS).

use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows::wait_read;

/// One read of `sock` by `read`, waiting at most `wait` for a datagram:
/// `Ok(None)` when none came within it (and at once for a zero wait),
/// an error when the read failed. `read` is the receive itself
/// (`recv_from`, or `recv` on a connected socket).
///
/// The socket must be a blocking one when given. On Windows it is left
/// non-blocking after the call (a later plain `recv_from` on it returns
/// would-block at once, and a send can when its buffer is full);
/// elsewhere its read timeout is left set to `wait`.
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

/// The longest wait taken as asked: a longer one (up to `Duration::MAX`)
/// waits this long, about 24.8 days, `WSAPoll`'s longest timeout.
#[cfg_attr(not(windows), allow(dead_code))]
const MAX_WAIT: Duration = Duration::from_millis(2_147_483_647);
const _: () = assert!(MAX_WAIT.as_millis() == i32::MAX as u128);

/// When a wait of `wait` from `now` ends: at most [`MAX_WAIT`] away, so a
/// huge wait never overflows the clock.
#[cfg_attr(not(windows), allow(dead_code))]
fn until_of(now: Instant, wait: Duration) -> Instant {
    now + wait.min(MAX_WAIT)
}

/// The wait as `WSAPoll`'s timeout: whole ms rounded up (a wait never
/// ends before the time asked), at most `i32::MAX`.
#[cfg_attr(not(windows), allow(dead_code))]
fn poll_ms(wait: Duration) -> i32 {
    i32::try_from(wait.as_micros().div_ceil(1000)).unwrap_or(i32::MAX)
}

/// Reads with `read` (a non-blocking receive) until it gives a datagram or
/// fails; after a read with nothing yet, waits with `ready` for the time
/// left before `until` (true: the socket is readable, or has an error the
/// next read gives), reading again only after a wait that found it
/// readable. `None` once `until` passed, never before: `WSAPoll` can end a
/// wait with nothing before its timeout (windows-2025: a 10 ms wait after
/// 9.28 ms, a 50 ms one after 49.58 ms, #454 run 44666301837), so such a
/// wait is waited again for what is left. No receive is pending between the waits, so none is
/// lost.
#[cfg_attr(not(windows), allow(dead_code))]
fn read_when_ready<T>(
    until: Instant,
    mut read: impl FnMut() -> io::Result<T>,
    mut ready: impl FnMut(Duration) -> io::Result<bool>,
) -> io::Result<Option<T>> {
    let mut readable = true;
    loop {
        if readable {
            match read_of(read()) {
                Read::Nothing => {}
                done => return result_of(done),
            }
        }
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(None);
        }
        readable = ready(left)?;
    }
}

/// The wait as a read timeout (`SO_RCVTIMEO`), then one read (every OS but
/// Windows).
#[cfg(not(windows))]
fn wait_read<T>(
    sock: &UdpSocket,
    wait: Duration,
    mut read: impl FnMut(&UdpSocket) -> io::Result<T>,
) -> io::Result<Option<T>> {
    sock.set_read_timeout(Some(wait))?;
    result_of(read_of(read(sock)))
}

/// Timeout reads (`attempt` takes the time left as its timeout) until one
/// gives a datagram or fails, or `until` passed: the stub of the RED test.
#[cfg(not(windows))]
#[cfg_attr(not(test), allow(dead_code))]
fn timeout_reads<T>(
    _until: Instant,
    mut attempt: impl FnMut(Duration) -> io::Result<T>,
) -> io::Result<Option<T>> {
    result_of(read_of(attempt(Duration::from_millis(1))))
}

#[cfg(test)]
mod tests;
