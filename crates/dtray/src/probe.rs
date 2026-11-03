// SPDX-License-Identifier: MIT OR Apache-2.0

//! One `status` request to a device's local endpoint
//! ([`dstack::ctl::local`]): TCP to 127.0.0.1 at the port its ARC record
//! names, the request line, the JSON line back. Every step is bounded by
//! [`WAIT`], so one stuck device holds the tray for a second at most.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::time::Duration;

/// How long the connection, the request and the answer may each take.
pub const WAIT: Duration = Duration::from_secs(1);
/// The longest answer read, bytes: a status line is a few kB; a longer
/// answer is no status of ours.
pub const MAX_ANSWER: usize = 1 << 20;
/// The request.
pub const REQUEST: &[u8] = b"status\n";

/// Why there is no status line.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("no answer: {0}")]
    Io(#[from] io::Error),
    #[error("the device answered: {0}")]
    Refused(String),
    #[error("the answer is longer than {MAX_ANSWER} bytes")]
    TooLong,
    #[error("the answer is not text")]
    NotText,
    #[error("the answer is empty")]
    Empty,
}

/// The status line of the device whose local endpoint is at `port` on
/// 127.0.0.1, within [`WAIT`] a step.
pub fn status(port: u16) -> Result<String, ProbeError> {
    status_at(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into(), WAIT)
}

/// The status line of the endpoint at `addr`, within `wait` a step.
pub fn status_at(addr: SocketAddr, wait: Duration) -> Result<String, ProbeError> {
    let mut stream = TcpStream::connect_timeout(&addr, wait)?;
    stream.set_read_timeout(Some(wait))?;
    stream.set_write_timeout(Some(wait))?;
    stream.write_all(REQUEST)?;
    let mut answer = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf)?;
        answer.extend_from_slice(&buf[..n]);
        if let Some(end) = answer.iter().position(|&b| b == b'\n') {
            answer.truncate(end);
            break;
        }
        if answer.len() > MAX_ANSWER {
            return Err(ProbeError::TooLong);
        }
        if n == 0 {
            break;
        }
    }
    line_of(answer)
}

/// The answer's line as the status: an `error: ...` line is the device's
/// refusal, an empty one no answer.
pub fn line_of(answer: Vec<u8>) -> Result<String, ProbeError> {
    let line = String::from_utf8(answer).map_err(|_| ProbeError::NotText)?;
    let line = line.trim_end_matches('\r').to_string();
    if line.is_empty() {
        return Err(ProbeError::Empty);
    }
    match line.strip_prefix("error: ") {
        Some(reason) => Err(ProbeError::Refused(reason.to_string())),
        None => Ok(line),
    }
}

#[cfg(test)]
mod tests;
