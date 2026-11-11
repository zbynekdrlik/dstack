// SPDX-License-Identifier: MIT OR Apache-2.0

//! One request to a device's local endpoint ([`super::local`]): TCP
//! to 127.0.0.1 at the port its ARC record names, the request line, the
//! answer line back, all within one deadline of [`WAIT`]: a stuck or slow
//! device holds the reader for a second at most. `status` reads it
//! ([`status`]); `app <words>` asks the app ([`ask`], dstack #139: an app's
//! output from its menu). Its readers: dtray (dstack #104), and a device's
//! traffic watch reading its neighbours on the PC ([`crate::traffic`],
//! dstack #203, with a shorter wait of its own through [`status_at`]).

use std::io::{self, ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::time::{Duration, Instant};

/// How long the whole exchange may take: the connection, the request and
/// the answer together.
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
/// 127.0.0.1, within [`WAIT`].
pub fn status(port: u16) -> Result<String, ProbeError> {
    status_at(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into(), WAIT)
}

/// The answer of the device whose local endpoint is at `port` to the
/// request `line` (without its newline), within [`WAIT`]: `ok` or the
/// app's text; `error: ...` is [`ProbeError::Refused`].
pub fn ask(port: u16, line: &str) -> Result<String, ProbeError> {
    let mut request = line.as_bytes().to_vec();
    request.push(b'\n');
    ask_at(
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into(),
        &request,
        WAIT,
    )
}

/// The time from `now` to `deadline`; `TimedOut` when none is left (a
/// socket timeout of zero would mean none at all).
pub fn time_left(deadline: Instant, now: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(now)
        .filter(|d| !d.is_zero())
        .ok_or_else(|| io::Error::new(ErrorKind::TimedOut, "the probe's time is up"))
}

/// The status line of the endpoint at `addr`, the whole exchange within
/// `wait`.
pub fn status_at(addr: SocketAddr, wait: Duration) -> Result<String, ProbeError> {
    ask_at(addr, REQUEST, wait)
}

/// The endpoint at `addr`'s answer line to `request` (a line with its
/// newline), the whole exchange within `wait`.
pub fn ask_at(addr: SocketAddr, request: &[u8], wait: Duration) -> Result<String, ProbeError> {
    let deadline = Instant::now() + wait;
    let mut stream = TcpStream::connect_timeout(&addr, wait)?;
    stream.set_write_timeout(Some(time_left(deadline, Instant::now())?))?;
    stream.write_all(request)?;
    let mut answer = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        stream.set_read_timeout(Some(time_left(deadline, Instant::now())?))?;
        let n = stream.read(&mut buf)?;
        answer.extend_from_slice(&buf[..n]);
        if let Some(end) = answer.iter().position(|&b| b == b'\n') {
            if end > MAX_ANSWER {
                return Err(ProbeError::TooLong);
            }
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
