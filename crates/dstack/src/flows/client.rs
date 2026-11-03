// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.
// Protocol facts (byte layouts, opcodes, behaviour) checked against
// inferno (https://github.com/teodly/inferno) as a reference; the code is
// our own.

//! Our side of flow control (Gen 3 phase B, plan Task 5): the requests our
//! RX sends to a transmitter's UDP 4455, [`Client::request_flow`] for a
//! flow of its channels to one of our flow sockets and
//! [`Client::stop_flow`] to end it.
//!
//! Every exchange is one datagram from a socket of its own, bound to our
//! address and connected to the transmitter (only its datagrams are read,
//! and a closed port is an error at once), and one wait for the reply: the
//! datagram whose opcode and sequence number are the request's (inferno's
//! match; replies to anything else are skipped). A reply's status 1 is done,
//! any other is [`ClientError::Refused`]. Nothing here ever asks again: when
//! to ask again is the subscription manager's call
//! ([`crate::subscribe::Manager`]: at most one request per transmitter in
//! flight and per 9 s, longer after each failure).
//!
//! The start code is always [`super::control::START`] (inferno's client
//! sends it whatever the transmitter's `dbcp1` record says).

use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use tokio::net::UdpSocket;
use tracing::debug;

use super::control::{FlowRequest, Handle, build_request, build_stop, op, reply_handle, status};
use crate::wire::HEADER_LEN;

/// How long a flow request waits for its reply (inferno's 3 s).
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a stop waits for its reply: a stop never needs one (a flow
/// without our keepalives expires at its transmitter), and the device's
/// shutdown ends every flow within this.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(1);
/// A reply read from the socket; anything longer is cut, which no reply of
/// flow control is.
const RECV_BUFFER: usize = 2048;

/// Why an exchange gave no answer we can use.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The socket failed: no address, a closed port (port unreachable), a
    /// send or receive error.
    #[error("socket: {0}")]
    Io(#[from] io::Error),
    /// The transmitter answered with this status (inferno's 0x0103, 0x0301,
    /// 0x0302, 0x0315, or another).
    #[error("refused with status {0:#06x}")]
    Refused(u16),
    /// No reply within the wait.
    #[error("no reply within the wait")]
    Timeout,
    /// A reply of done without the 6-byte handle.
    #[error("a reply of done without a handle")]
    NoHandle,
}

/// Our flow-control client: our address and the sequence numbers.
#[derive(Debug)]
pub struct Client {
    ip: Ipv4Addr,
    seq: AtomicU16,
    reply_timeout: Duration,
    stop_timeout: Duration,
}

impl Client {
    /// The client of the device at `ip`, waiting [`REPLY_TIMEOUT`] for a
    /// request's reply and [`STOP_TIMEOUT`] for a stop's.
    pub fn new(ip: Ipv4Addr) -> Self {
        Self::with_timeouts(ip, REPLY_TIMEOUT, STOP_TIMEOUT)
    }

    /// The client with other waits (tests).
    pub fn with_timeouts(ip: Ipv4Addr, reply: Duration, stop: Duration) -> Self {
        Client {
            ip,
            seq: AtomicU16::new(1),
            reply_timeout: reply,
            stop_timeout: stop,
        }
    }

    /// Asks the transmitter at `to` (its 4455) for the flow `req` and
    /// returns the flow's handle.
    pub async fn request_flow(
        &self,
        to: SocketAddrV4,
        req: &FlowRequest,
    ) -> Result<Handle, ClientError> {
        let body = self
            .ask(to, op::REQUEST, self.reply_timeout, |seq| {
                build_request(seq, req)
            })
            .await?;
        reply_handle(&body).ok_or(ClientError::NoHandle)
    }

    /// Asks the transmitter at `to` to end the flow `h`.
    pub async fn stop_flow(&self, to: SocketAddrV4, h: &Handle) -> Result<(), ClientError> {
        self.ask(to, op::STOP, self.stop_timeout, |seq| build_stop(seq, h))
            .await
            .map(drop)
    }

    /// One request of opcode `op1` to `to`: the datagram `build` makes for
    /// the next sequence number (from 1, wrapping), sent once; the body of
    /// its reply of done within `wait`.
    async fn ask(
        &self,
        to: SocketAddrV4,
        op1: u16,
        wait: Duration,
        build: impl FnOnce(u16) -> Vec<u8>,
    ) -> Result<Vec<u8>, ClientError> {
        let seq = self.next_seq();
        let sock = self.connect_to(to).await?;
        sock.send(&build(seq)).await?;
        let outcome = first_reply(&sock, seq, op1, wait).await;
        if let Err(e) = &outcome {
            debug!(op = format_args!("{op1:#06x}"), seq, error = %e, "flow-control request failed");
        }
        outcome
    }

    /// The next sequence number (from 1, wrapping).
    fn next_seq(&self) -> u16 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    /// A socket on our address (any port) that talks to `to` only.
    async fn connect_to(&self, to: SocketAddrV4) -> io::Result<UdpSocket> {
        let sock = UdpSocket::bind(SocketAddr::V4(SocketAddrV4::new(self.ip, 0))).await?;
        sock.connect(SocketAddr::V4(to)).await?;
        Ok(sock)
    }
}

/// The reply to (`seq`, `op1`) on `sock` within `wait`, other datagrams
/// passed over: its body for done, else its status.
async fn first_reply(
    sock: &UdpSocket,
    seq: u16,
    op1: u16,
    wait: Duration,
) -> Result<Vec<u8>, ClientError> {
    let mut buf = [0u8; RECV_BUFFER];
    let read = async {
        loop {
            let n = sock.recv(&mut buf).await?;
            if let Some(answer) = reply_of(&buf[..n], seq, op1) {
                return answer.map(<[u8]>::to_vec).map_err(ClientError::Refused);
            }
        }
    };
    tokio::time::timeout(wait, read)
        .await
        .unwrap_or(Err(ClientError::Timeout))
}

/// What `d` says to the request (`seq`, `op1`): `None` when it is not its
/// reply (another opcode or sequence number, a request with op2 0, shorter
/// than the header), else its body for status done or the status. The start
/// code and the length field are not read.
pub fn reply_of(d: &[u8], seq: u16, op1: u16) -> Option<Result<&[u8], u16>> {
    let (head, body) = d.split_first_chunk::<HEADER_LEN>()?;
    let field = |at: usize| u16::from_be_bytes([head[at], head[at + 1]]);
    if field(4) != seq || field(6) != op1 {
        return None;
    }
    match field(8) {
        0 => None,
        status::OK => Some(Ok(body)),
        refused => Some(Err(refused)),
    }
}

#[cfg(test)]
mod tests;
