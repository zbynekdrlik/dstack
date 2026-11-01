// SPDX-License-Identifier: GPL-3.0-or-later
// The request and frame layouts follow netaudio (chris-ritsen/
// network-audio-controller, Unlicense): its Dante Controller capture of a
// meter subscription (tests/fixtures/metering/) and its metering.rs.

//! Any device's meters (dstack #16 step 6): a subscription on the
//! device's CMC port ([`crate::cmc::PORT`], 8800) that makes it send its
//! channels' levels to our UDP port, about as often as Dante Controller's
//! meters move. The subscription is renewed every [`RENEW`] (3 s, as
//! netaudio's daemon does; a device drops one that is not renewed) until we
//! stop it.
//!
//! **The start** (Dante Controller's own, captured; the controller's
//! address is 192.0.2.10 there):
//!
//! | bytes | |
//! |---|---|
//! | `12 00`, length, transaction, `30 10`, `00 00` | the CMC header |
//! | `02 00`, the controller's MAC | |
//! | `00 00 00 04`, name end + 0x0a, `00 02`, name end + 0x0c, `00 0a` | |
//! | the controller's name, NUL, padded to even | |
//! | `00 01 00 01`, tail offset, `00 01`, port, `00 01 00 00` | |
//! | IPv4, port, six zeros, port, two zeros | the destination (16 bytes) |
//!
//! **The stop** is the same packet with a new transaction and its
//! destination zeroed (the capture's stop). Two fields differ from
//! netaudio's own builder; the capture is followed: the word before the
//! MAC is 0x0200 (netaudio: 0), and the name's pad byte is 0 (Dante
//! Controller leaves garbage there).
//!
//! **A frame**, to our port: `ff ff`, length, sequence, `00 00`, the
//! device's id (8 bytes), `Audinate`, version 2, the TX channels' count,
//! the RX channels' count, a level byte per channel (TX, then RX), a 0.
//!
//! **A level byte**: 0 clipping; 1 to 253 is −(raw − 1)/2 dBFS (0 to
//! −126); 254 muted; 255 a framing marker ([`dbfs`], [`Level`]).
//!
//! Logs name counts and ports, never an address or a name.

use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tracing::{info, warn};

/// The CMC start code.
pub const CMC_START: u16 = 0x1200;
/// The meter subscription's opcode.
pub const OP_METERING: u16 = 0x3010;
/// The meter frame's version byte.
pub const FRAME_VERSION: u8 = 2;
/// The frame header, 27 bytes: start, length, sequence and 0 (2 each),
/// the device id and the vendor (8 each), the version and the two counts.
const FRAME_HEADER: usize = 27;
/// The destination at the start's end.
const DESTINATION_LEN: usize = 16;
/// How long the listener waits on its socket before it looks at its stop
/// and its renewal.
const POLL: Duration = Duration::from_millis(200);
/// How often a subscription is renewed (the start sent again with a new
/// transaction): netaudio's daemon renews every 3 s, and a device drops a
/// subscription that is not renewed.
pub const RENEW: Duration = Duration::from_secs(3);

/// A channel's level as a meter byte says it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Level {
    Clipping,
    Dbfs(f64),
    Muted,
    /// 255: no level (a framing marker).
    Marker,
}

/// What the level byte `raw` says.
pub fn level(raw: u8) -> Level {
    match raw {
        0 => Level::Clipping,
        254 => Level::Muted,
        255 => Level::Marker,
        r => Level::Dbfs(-(f64::from(r) - 1.0) / 2.0),
    }
}

/// The level byte `raw` in dBFS (0 for clipping), `None` muted or a marker.
pub fn dbfs(raw: u8) -> Option<f64> {
    match level(raw) {
        Level::Clipping => Some(0.0),
        Level::Dbfs(d) => Some(d),
        Level::Muted | Level::Marker => None,
    }
}

/// The start request: transaction `seq`, from the controller named `name`
/// with `mac`, asking for frames to `to`. `None` for a name longer than a
/// packet can say.
pub fn start_request(seq: u16, name: &str, mac: [u8; 6], to: SocketAddrV4) -> Option<Vec<u8>> {
    let mut name_bytes = name.as_bytes().to_vec();
    name_bytes.push(0);
    if !name_bytes.len().is_multiple_of(2) {
        name_bytes.push(0);
    }
    let off1 = u16::try_from(name_bytes.len() + 0x0a).ok()?;
    let off2 = u16::try_from(name_bytes.len() + 0x0c).ok()?;
    let tail = off2.checked_add(4)?;
    let port = to.port().to_be_bytes();
    let mut body = Vec::new();
    for w in [OP_METERING, 0, 0x0200] {
        body.extend_from_slice(&w.to_be_bytes());
    }
    body.extend_from_slice(&mac);
    for w in [0u16, 4, off1, 2, off2, 0x000a] {
        body.extend_from_slice(&w.to_be_bytes());
    }
    body.extend_from_slice(&name_bytes);
    for w in [1u16, 1, tail, 1] {
        body.extend_from_slice(&w.to_be_bytes());
    }
    body.extend_from_slice(&port);
    for w in [1u16, 0] {
        body.extend_from_slice(&w.to_be_bytes());
    }
    body.extend_from_slice(&to.ip().octets());
    body.extend_from_slice(&port);
    body.extend_from_slice(&[0; 6]);
    body.extend_from_slice(&port);
    body.extend_from_slice(&[0; 2]);
    let len = u16::try_from(6 + body.len()).ok()?;
    let mut out = Vec::with_capacity(usize::from(len));
    out.extend_from_slice(&CMC_START.to_be_bytes());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&body);
    Some(out)
}

/// The stop request: the start with transaction `seq` and its destination
/// zeroed.
pub fn stop_request(seq: u16, name: &str, mac: [u8; 6], to: SocketAddrV4) -> Option<Vec<u8>> {
    let mut out = start_request(seq, name, mac, to)?;
    let from = out.len() - DESTINATION_LEN;
    out[from..].fill(0);
    Some(out)
}

/// One meter frame: the device's id, its TX and its RX channels' level
/// bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub device_id: [u8; 8],
    pub tx: Vec<u8>,
    pub rx: Vec<u8>,
}

/// The frame in `datagram`; `None` when it is not one (start code,
/// vendor, version) or its counts pass it.
pub fn parse_frame(datagram: &[u8]) -> Option<Frame> {
    let head = datagram.get(..FRAME_HEADER)?;
    if head[0..2] != [0xff, 0xff] || &head[16..24] != b"Audinate" || head[24] != FRAME_VERSION {
        return None;
    }
    let (ntx, nrx) = (usize::from(head[25]), usize::from(head[26]));
    let levels = datagram.get(FRAME_HEADER..FRAME_HEADER + ntx + nrx)?;
    Some(Frame {
        device_id: head[8..16].try_into().ok()?,
        tx: levels[..ntx].to_vec(),
        rx: levels[ntx..].to_vec(),
    })
}

/// The transactions of this process's requests.
static SEQ: AtomicU16 = AtomicU16::new(1);

/// A running meter subscription to one device.
pub struct Meter {
    sock: UdpSocket,
    device: SocketAddrV4,
    name: String,
    mac: [u8; 6],
    to: SocketAddrV4,
    latest: Arc<Mutex<Option<Frame>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Meter {
    /// [`Meter::start_at`] on our address `iface`, a port the OS chooses.
    pub fn start(
        iface: Ipv4Addr,
        device: SocketAddrV4,
        name: &str,
        mac: [u8; 6],
    ) -> io::Result<(Meter, Receiver<Frame>)> {
        Self::start_at(SocketAddrV4::new(iface, 0), device, name, mac)
    }

    /// Subscribes to the meters of the device at `device` (its CMC port),
    /// as the controller `name` with `mac`, from our socket bound at `bind`
    /// (a port of 0: the OS chooses): the start sent, a thread keeping the
    /// latest frame from that address (any source port: devices send the
    /// frames from another port than 8800) and sending every frame on the
    /// returned channel (dropped when nobody reads it). A fixed port lets a
    /// firewall that admits only established flows name it (dcontroller's
    /// box: the frames are a new flow to conntrack).
    pub fn start_at(
        bind: SocketAddrV4,
        device: SocketAddrV4,
        name: &str,
        mac: [u8; 6],
    ) -> io::Result<(Meter, Receiver<Frame>)> {
        let sock = UdpSocket::bind(bind)?;
        crate::winsock::no_connreset(&sock)?;
        sock.set_read_timeout(Some(POLL))?;
        let to = match sock.local_addr()? {
            SocketAddr::V4(a) => a,
            SocketAddr::V6(_) => return Err(io::Error::other("an IPv6 socket")),
        };
        let start = start_request(SEQ.fetch_add(1, Ordering::Relaxed), name, mac, to)
            .ok_or_else(|| io::Error::other("a controller name too long for a packet"))?;
        sock.send_to(&start, device)?;
        info!(port = to.port(), "meter subscription sent");
        let latest = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (frames, rx) = mpsc::sync_channel(64);
        let thread = {
            let sock = sock.try_clone()?;
            let latest = Arc::clone(&latest);
            let stop = Arc::clone(&stop);
            let name = name.to_string();
            std::thread::Builder::new()
                .name("dante-meter".to_string())
                .spawn(move || {
                    let renew =
                        || start_request(SEQ.fetch_add(1, Ordering::Relaxed), &name, mac, to);
                    listen(&sock, device, &latest, &stop, &frames, renew);
                })?
        };
        Ok((
            Meter {
                sock,
                device,
                name: name.to_string(),
                mac,
                to,
                latest,
                stop,
                thread: Some(thread),
            },
            rx,
        ))
    }

    /// The latest frame received, if any.
    pub fn latest(&self) -> Option<Frame> {
        self.latest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Sends the stop and ends the listener.
    pub fn stop(mut self) -> io::Result<()> {
        let stop = stop_request(
            SEQ.fetch_add(1, Ordering::Relaxed),
            &self.name,
            self.mac,
            self.to,
        )
        .ok_or_else(|| io::Error::other("a controller name too long for a packet"))?;
        let sent = self.sock.send_to(&stop, self.device).map(|_| ());
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take()
            && t.join().is_err()
        {
            warn!("the meter listener panicked");
        }
        info!(port = self.to.port(), "meter subscription stopped");
        sent
    }
}

/// The listener: frames from the device kept in `latest` and sent on
/// `frames`, the subscription renewed every [`RENEW`] (`renew` builds the
/// start), until `stop`.
fn listen(
    sock: &UdpSocket,
    device: SocketAddrV4,
    latest: &Mutex<Option<Frame>>,
    stop: &AtomicBool,
    frames: &mpsc::SyncSender<Frame>,
    renew: impl Fn() -> Option<Vec<u8>>,
) {
    let mut buf = [0u8; 2048];
    let mut next = Instant::now() + RENEW;
    while !stop.load(Ordering::Acquire) {
        if Instant::now() >= next {
            if let Some(start) = renew()
                && let Err(e) = sock.send_to(&start, device)
            {
                warn!(error = %e, "meter subscription not renewed");
            }
            next += RENEW;
        }
        match sock.recv_from(&mut buf) {
            Ok((n, SocketAddr::V4(a))) if a.ip() == device.ip() => {
                if let Some(f) = parse_frame(&buf[..n]) {
                    *latest.lock().unwrap_or_else(PoisonError::into_inner) = Some(f.clone());
                    let _full_or_gone = frames.try_send(f);
                }
            }
            // Another sender, or the read timeout's end (the stop and the
            // renewal are looked at every POLL): nothing to keep.
            Ok(_) | Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests;
