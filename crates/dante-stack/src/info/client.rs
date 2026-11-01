// SPDX-License-Identifier: GPL-3.0-or-later

//! Asking any device for its product and board info over UDP
//! [`REQUEST_PORT`] (8700): the manufacturer, the model and the versions
//! that Dante Controller and netaudio show (dstack#16 step 4).
//!
//! A request is the 32-byte info header with the opcode `07 31 00 c1`
//! (product info) or `07 31 00 61` (board info), netaudio's. The device
//! answers to the info group [`INFO_GROUP`]:[`INFO_PORT`]
//! (224.0.0.231:8702), as our own server does, from a port of its own
//! (2864, 8700 and 1033 seen): netaudio's two sockets on the group each
//! received the same reply. So the client listens on the group and takes
//! the first reply from the device's address, with the device's id when it
//! is given (several devices answer on one address) and the reply's
//! opcode (`.. 00 c0` / `.. 00 60`).
//!
//! The clock status (`07 31 00 21 00 00 00 64` and 60 zeros, netaudio's
//! clock-stats request; reply `.. 00 20`) is parsed by [`parse_clock`]:
//! the clock and servo states, the stratum, the frequency offset, the
//! device's, master's and grandmaster's ids, each PTP port's state, the
//! capabilities, the maximum drift and the word clock, as netaudio decodes
//! them on three devices (a leader and two followers). Our own device
//! answers no clock stats (it has no PTP of its own).
//!
//! Every version is `major u8, minor u8, patch u16`, big-endian.
//!
//! | product info (0xc0) | |
//! |---|---|
//! | 0x00 | manufacturer id, 8 bytes |
//! | 0x08 | product id, 8 bytes |
//! | 0x18 | manufacturer software version |
//! | 0x1c | manufacturer firmware version |
//! | 0x2c | manufacturer name, to 0xac |
//! | 0xac | product name |
//! | 0x12c | product version |
//!
//! | board info (0x60) | |
//! |---|---|
//! | 0x00 | platform software version |
//! | 0x04 | platform hardware version |
//! | 0x08 | API version |
//! | 0x0c | board short name, 8 bytes |
//! | 0x20 | software build, `u32` |
//! | 0x24 | hardware build, `u32` |
//! | 0x28 | ROM boot version |
//! | 0x38 | platform model name |
//!
//! The offsets come from captures of four makes of hardware; each value
//! matched `netaudio device show` for the same device. A version of all
//! zeros is not given (`None`).

use std::fmt;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, Instant};

use super::{HEADER_LEN, INFO_GROUP, INFO_PORT, REQUEST_PORT, START_INFO, VENDOR};

const RECV_BUFFER: usize = 2048;
/// How long an ask waits for a reply before it sends its request again: a
/// Brooklyn card drops a board-info request asked right after another
/// (netaudio sends it three times; dcontroller found it on the site).
pub const RESEND: Duration = Duration::from_millis(250);
/// The requests' sequence, a new one for every request sent (a resend
/// too), as Dante Controller numbers its own: a Brooklyn card drops a
/// request whose sequence it has just answered (dcontroller and netaudio's
/// trace, 1.11.2026).
static SEQ: AtomicU16 = AtomicU16::new(1);

/// Where a name ends: its field's end or its first zero.
const PRODUCT_MANUFACTURER: (usize, usize) = (0x2c, 0xac);
const PRODUCT_NAME: (usize, usize) = (0xac, 0x12c);
const BOARD_SHORT_NAME: (usize, usize) = (0x0c, 0x14);
const BOARD_MODEL_NAME: (usize, usize) = (0x38, 0xb8);
/// The zeros netaudio's clock-stats request carries.
const CLOCK_REQUEST_BODY: usize = 60;
/// The clock status's ports start here, 4 bytes each.
const CLOCK_PORTS: usize = 0x28;
/// Where the word clock state sits past the clock status's extension.
const CLOCK_WORD_CLOCK: usize = 0x17;

/// What to ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    Product,
    Board,
    /// The clock status (Dante's clock stats).
    Clock,
}

impl Ask {
    /// The request opcode's last byte.
    fn request_op(self) -> u8 {
        match self {
            Ask::Product => 0xc1,
            Ask::Board => 0x61,
            Ask::Clock => 0x21,
        }
    }

    /// The request opcode's tail byte and the request body's length:
    /// netaudio's clock-stats request ends `.. 64` and carries 60 zeros.
    fn request_tail(self) -> (u8, usize) {
        match self {
            Ask::Product | Ask::Board => (0, 0),
            Ask::Clock => (0x64, CLOCK_REQUEST_BODY),
        }
    }

    /// The reply opcode's last byte.
    fn reply_op(self) -> u8 {
        match self {
            Ask::Product => 0xc0,
            Ask::Board => 0x60,
            Ask::Clock => 0x20,
        }
    }
}

/// A version: `major.minor.patch`, then `.build` when the device gives one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub major: u8,
    pub minor: u8,
    pub patch: u16,
    pub build: Option<u32>,
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        match self.build {
            Some(b) => write!(f, ".{b}"),
            None => Ok(()),
        }
    }
}

/// The product info: who made the device and its product's versions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Product {
    pub manufacturer_id: [u8; 8],
    pub product_id: [u8; 8],
    pub manufacturer: String,
    pub model: String,
    pub software: Option<Version>,
    pub firmware: Option<Version>,
    pub version: Option<Version>,
}

/// The board info: the Dante platform and its versions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Board {
    /// The board's short name (`Bklyn2`, `UltimoX`, `PCIe`).
    pub board: String,
    /// The platform's model name (`Brooklyn II`, `UltimoX2`).
    pub model: String,
    pub software: Option<Version>,
    pub hardware: Option<Version>,
    pub api: Option<Version>,
    pub boot: Option<Version>,
}

/// A device's clock status (the clock-stats reply). Codes are kept as the
/// device sends them; [`ClockStatus::clock_state_name`] and the others
/// name only the codes seen in captures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockStatus {
    /// 2 undisciplined (a leader), 3 disciplined.
    pub clock_state: u16,
    /// 3 synchronized, 6 none (a leader).
    pub servo_state: u16,
    /// The stratum (the byte at 0x07; the byte before it is 1 at the
    /// leader, 0 at a follower, its meaning unknown).
    pub stratum: u8,
    /// The clock's frequency offset, ppb.
    pub frequency_ppb: i32,
    /// The clock ids (PTPv1 UUIDs, a MAC each): the device's, its
    /// master's and the grandmaster's.
    pub device: [u8; 6],
    pub master: [u8; 6],
    pub grandmaster: [u8; 6],
    /// Each PTP port's state (IEEE 1588: 3 disabled, 6 master, 9 slave).
    pub ports: Vec<u16>,
    pub capabilities: u16,
    pub max_drift_ppb: u32,
    /// The word clock: 0 unknown, 1 none, 3 valid; `None` past the reply.
    pub word_clock: Option<u8>,
}

impl ClockStatus {
    /// Some port leads (state 6, master).
    pub fn is_leader(&self) -> bool {
        self.ports.contains(&6)
    }

    /// The device is the grandmaster.
    pub fn is_grandmaster(&self) -> bool {
        self.device == self.grandmaster
    }

    pub fn clock_state_name(&self) -> Option<&'static str> {
        match self.clock_state {
            2 => Some("undisciplined"),
            3 => Some("disciplined"),
            _ => None,
        }
    }

    pub fn servo_state_name(&self) -> Option<&'static str> {
        match self.servo_state {
            3 => Some("synchronized"),
            6 => Some("none"),
            _ => None,
        }
    }
}

/// An IEEE 1588 port state's name, for the states seen in captures.
pub fn port_state_name(state: u16) -> Option<&'static str> {
    match state {
        3 => Some("disabled"),
        6 => Some("leader"),
        9 => Some("follower"),
        _ => None,
    }
}

/// Why an ask ended without a reply that reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InfoError {
    #[error("info {ask:?}: socket: {kind}")]
    Io { ask: Ask, kind: io::ErrorKind },
    #[error("info {ask:?}: no reply within the wait")]
    Timeout { ask: Ask },
    #[error("info {ask:?}: a reply that does not read")]
    Malformed { ask: Ask },
}

/// The request for `ask`: sequence `seq`, from the device id `from`.
pub fn request(seq: u16, from: [u8; 8], ask: Ask) -> Vec<u8> {
    let (tail, body) = ask.request_tail();
    let mut out = Vec::new();
    out.extend_from_slice(&START_INFO.to_be_bytes());
    out.extend_from_slice(&((HEADER_LEN + body) as u16).to_be_bytes());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&from);
    out.extend_from_slice(&VENDOR);
    out.extend_from_slice(&[0x07, 0x31, 0x00, ask.request_op(), 0, 0, 0, tail]);
    out.resize(HEADER_LEN + body, 0);
    out
}

/// The body of `datagram` when it is the reply to `ask` from the device
/// id `id` (any id with `None`).
pub fn reply_body(datagram: &[u8], ask: Ask, id: Option<[u8; 8]>) -> Option<&[u8]> {
    let (head, body) = datagram.split_first_chunk::<HEADER_LEN>()?;
    let opcode = &head[24..32];
    let ours = head[0..2] == START_INFO.to_be_bytes()
        && opcode[0] == 0x07
        && opcode[2] == 0x00
        && opcode[3] == ask.reply_op()
        && opcode[4..8] == [0, 0, 0, 0]
        && id.is_none_or(|id| head[8..16] == id);
    ours.then_some(body)
}

/// The version at `at`, `None` past the body or all zeros.
fn version_at(body: &[u8], at: usize, build: Option<usize>) -> Option<Version> {
    let v: [u8; 4] = body.get(at..at + 4)?.try_into().ok()?;
    if v == [0; 4] {
        return None;
    }
    let build = match build {
        Some(b) => Some(u32::from_be_bytes(body.get(b..b + 4)?.try_into().ok()?)),
        None => None,
    };
    Some(Version {
        major: v[0],
        minor: v[1],
        patch: u16::from_be_bytes([v[2], v[3]]),
        build,
    })
}

/// The name in `body[from..to]`, cut at its first zero.
fn name_at(body: &[u8], (from, to): (usize, usize)) -> Option<String> {
    let field = body.get(from..to)?;
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    Some(String::from_utf8_lossy(&field[..end]).into_owned())
}

fn id_at(body: &[u8], at: usize) -> Option<[u8; 8]> {
    body.get(at..at + 8)?.try_into().ok()
}

/// A product info body; `None` when it is shorter than its last field.
pub fn parse_product(body: &[u8]) -> Option<Product> {
    Some(Product {
        manufacturer_id: id_at(body, 0x00)?,
        product_id: id_at(body, 0x08)?,
        manufacturer: name_at(body, PRODUCT_MANUFACTURER)?,
        model: name_at(body, PRODUCT_NAME)?,
        software: version_at(body, 0x18, None),
        firmware: version_at(body, 0x1c, None),
        version: version_at(body.get(..0x130)?, 0x12c, None),
    })
}

/// A board info body; `None` when it is shorter than its last field.
pub fn parse_board(body: &[u8]) -> Option<Board> {
    let body = body.get(..BOARD_MODEL_NAME.1)?;
    Some(Board {
        board: name_at(body, BOARD_SHORT_NAME)?,
        model: name_at(body, BOARD_MODEL_NAME)?,
        software: version_at(body, 0x00, Some(0x20)),
        hardware: version_at(body, 0x04, Some(0x24)),
        api: version_at(body, 0x08, None),
        boot: version_at(body, 0x28, None),
    })
}

fn u16_at(body: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(body.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(body: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(body.get(at..at + 4)?.try_into().ok()?))
}

fn mac_at(body: &[u8], at: usize) -> Option<[u8; 6]> {
    body.get(at..at + 6)?.try_into().ok()
}

/// A clock-stats reply body; `None` when it is shorter than its ports and
/// the fields after them.
pub fn parse_clock(body: &[u8]) -> Option<ClockStatus> {
    let n = usize::from(u16_at(body, 0x24)?);
    let extension = usize::from(u16_at(body, 0x26)?);
    let ports = (0..n)
        .map(|p| u16_at(body, CLOCK_PORTS + 4 * p))
        .collect::<Option<Vec<_>>>()?;
    let after = CLOCK_PORTS + 4 * n;
    Some(ClockStatus {
        clock_state: u16_at(body, 0x00)?,
        servo_state: u16_at(body, 0x02)?,
        stratum: *body.get(0x07)?,
        frequency_ppb: i32::from_be_bytes(body.get(0x08..0x0c)?.try_into().ok()?),
        device: mac_at(body, 0x0c)?,
        master: mac_at(body, 0x14)?,
        grandmaster: mac_at(body, 0x1c)?,
        ports,
        capabilities: u16_at(body, after)?,
        max_drift_ppb: u32_at(body, after + 4)?,
        word_clock: body.get(extension + CLOCK_WORD_CLOCK).copied(),
    })
}

/// Sends the request for `ask` from `sock` to `device` (a device's
/// [`REQUEST_PORT`]), and again every [`RESEND`] until its reply comes
/// (each with a sequence of its own),
/// for up to `wait` in all: its reply from that address, any port (and the
/// device id `id`, when given); the reply's body. Other datagrams on
/// `sock` are skipped.
pub fn ask(
    sock: &UdpSocket,
    device: SocketAddrV4,
    id: Option<[u8; 8]>,
    ask: Ask,
    wait: Duration,
) -> Result<Vec<u8>, InfoError> {
    let io = |e: io::Error| InfoError::Io {
        ask,
        kind: e.kind(),
    };
    let start = Instant::now();
    let deadline = start + wait;
    let mut next = start;
    let mut buf = [0u8; RECV_BUFFER];
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(InfoError::Timeout { ask });
        }
        if now >= next {
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            sock.send_to(&request(seq, [0; 8], ask), device)
                .map_err(io)?;
            next = now + RESEND;
        }
        sock.set_read_timeout(Some(deadline.min(next) - now))
            .map_err(io)?;
        match sock.recv_from(&mut buf) {
            Ok((n, SocketAddr::V4(from))) if from.ip() == device.ip() => {
                if let Some(body) = reply_body(&buf[..n], ask, id) {
                    return Ok(body.to_vec());
                }
            }
            Ok(_) => {}
            Err(e) => match recv_error(ask, &e) {
                InfoError::Timeout { .. } => {}
                other => return Err(other),
            },
        }
    }
}

/// What a receive error ends an ask with: the read timeout's end (a
/// would-block or a timed-out read, by platform) is a timeout, anything
/// else a socket error.
fn recv_error(ask: Ask, e: &io::Error) -> InfoError {
    match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => InfoError::Timeout { ask },
        kind => InfoError::Io { ask, kind },
    }
}

/// A socket on the info group, joined on the interface at `iface`: it
/// shares [`INFO_PORT`] with any other listener on the host.
pub fn group_socket(iface: Ipv4Addr) -> io::Result<UdpSocket> {
    let sock = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    sock.set_reuse_address(true)?;
    #[cfg(unix)]
    sock.set_reuse_port(true)?;
    sock.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, INFO_PORT).into())?;
    sock.join_multicast_v4(&INFO_GROUP, &iface)?;
    sock.set_multicast_if_v4(&iface)?;
    let sock: UdpSocket = sock.into();
    crate::winsock::no_connreset(&sock)?;
    Ok(sock)
}

fn on_group(iface: Ipv4Addr, a: Ask) -> Result<UdpSocket, InfoError> {
    group_socket(iface).map_err(|e| InfoError::Io {
        ask: a,
        kind: e.kind(),
    })
}

/// The product info of the device at `device` (with the device id `id`,
/// when given), asked from our interface at `iface`.
pub fn product_info(
    iface: Ipv4Addr,
    device: Ipv4Addr,
    id: Option<[u8; 8]>,
    wait: Duration,
) -> Result<Product, InfoError> {
    let sock = on_group(iface, Ask::Product)?;
    let body = ask(
        &sock,
        SocketAddrV4::new(device, REQUEST_PORT),
        id,
        Ask::Product,
        wait,
    )?;
    parse_product(&body).ok_or(InfoError::Malformed { ask: Ask::Product })
}

/// The board info of the device at `device` (with the device id `id`,
/// when given), asked from our interface at `iface`.
pub fn board_info(
    iface: Ipv4Addr,
    device: Ipv4Addr,
    id: Option<[u8; 8]>,
    wait: Duration,
) -> Result<Board, InfoError> {
    let sock = on_group(iface, Ask::Board)?;
    let body = ask(
        &sock,
        SocketAddrV4::new(device, REQUEST_PORT),
        id,
        Ask::Board,
        wait,
    )?;
    parse_board(&body).ok_or(InfoError::Malformed { ask: Ask::Board })
}

/// The clock status of the device at `device` (with the device id `id`,
/// when given), asked from our interface at `iface`.
pub fn clock_status(
    iface: Ipv4Addr,
    device: Ipv4Addr,
    id: Option<[u8; 8]>,
    wait: Duration,
) -> Result<ClockStatus, InfoError> {
    let sock = on_group(iface, Ask::Clock)?;
    let body = ask(
        &sock,
        SocketAddrV4::new(device, REQUEST_PORT),
        id,
        Ask::Clock,
        wait,
    )?;
    parse_clock(&body).ok_or(InfoError::Malformed { ask: Ask::Clock })
}

#[cfg(test)]
mod tests;
