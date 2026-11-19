//! Several devices on one address share its info port (dstack #1, the
//! per-PC manager). UDP 8700 is one per address, and Dante Controller reads
//! a device's ARC only after an 0x0061 reply carrying that device's id
//! (dev1, 31.10.2026: a second device whose info requests went to the
//! first was never read). So the process that holds `<ip>`:8700 is the
//! address's manager, and every other device on the address is a member:
//!
//! - A member registers with the manager over loopback ([`share_port`] on
//!   127.0.0.1), again every [`LEASE_EVERY`] as a lease ([`Members`]).
//! - The manager relays each info request it receives to every live member
//!   ([`Msg::Request`]); a member answers with its own identity and hands
//!   the packet back ([`Msg::Send`]), and the manager sends it from 8700,
//!   as it sends its own. A member's heartbeats and notifications go the
//!   same way.
//! - A bridge-figures or clock-figures request (dstack #391, #429,
//!   [`super::bridge`], [`super::clock_figures`]) is relayed with its
//!   sender's address instead ([`Msg::Ask`]): its reply goes back to the
//!   asker alone, so a member hands it over with that address
//!   ([`Msg::SendTo`]) and the manager sends it from 8700 unicast, only a
//!   bridge or clock reply ([`is_bridge_reply`], [`is_clock_reply`]), only
//!   to one host ([`is_unicast`]) it relayed a request from within
//!   [`ASK_HOLD_NS`] ([`Asks`]). An older member or manager reads neither
//!   message and drops it: that device answers no bridge figures until
//!   both run a dstack that knows them; a manager older than dstack #429
//!   relays a clock-figures request as a plain [`Msg::Request`] (which a
//!   member answers with nothing) and drops a member's clock reply.
//! - When the manager ends, 8700 frees; a member that binds it becomes the
//!   manager, and the others' leases reach it on the same loopback port.
//!
//! This module holds the messages and the members' table (pure); the
//! loops are in [`super::serve_info`] and [`super::serve_member`].

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

/// A member registers again this often.
pub const LEASE_EVERY: Duration = Duration::from_secs(1);
/// A member not heard for this long is gone.
pub const LEASE_NS: u64 = 3_000_000_000;
/// Members kept at most.
pub const MAX_MEMBERS: usize = 32;
/// The loopback ports are this one up ([`share_port`]).
pub const SHARE_BASE: u16 = 18700;

/// The loopback port of the address `ip`'s manager: [`SHARE_BASE`] plus the
/// address's low 10 bits, so two addresses on one PC rarely meet.
pub fn share_port(ip: Ipv4Addr) -> u16 {
    SHARE_BASE + (u32::from(ip) & 0x3ff) as u16
}

/// Where a member's packet goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dest {
    /// The info group (replies, board and product info, notifications).
    Info,
    /// The heartbeat group.
    Heartbeat,
}

/// A message between a member and its manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    /// Member to manager: its device id, again every [`LEASE_EVERY`].
    Register([u8; 8]),
    /// Manager to member: an info request the manager received.
    Request(Vec<u8>),
    /// Member to manager: a packet to send from 8700.
    Send(Dest, Vec<u8>),
    /// Manager to member: a bridge-figures or clock-figures request the
    /// manager received from that address and port.
    Ask(SocketAddrV4, Vec<u8>),
    /// Member to manager: a reply to send from 8700 to that address and
    /// port alone.
    SendTo(SocketAddrV4, Vec<u8>),
}

const REGISTER: u8 = b'R';
const REQUEST: u8 = b'Q';
const SEND_INFO: u8 = b'I';
const SEND_HEARTBEAT: u8 = b'H';
const ASK: u8 = b'A';
const SEND_TO: u8 = b'U';
/// An address and a port, before the datagram of [`Msg::Ask`] and
/// [`Msg::SendTo`].
const ADDR_LEN: usize = 6;

/// `kind`, `at` (its address, then its port, big-endian) and `body`.
fn addressed(kind: u8, at: &SocketAddrV4, body: &[u8]) -> Vec<u8> {
    [
        &[kind][..],
        &at.ip().octets(),
        &at.port().to_be_bytes(),
        body,
    ]
    .concat()
}

/// The address and the datagram after it; `None` when shorter than an
/// address.
fn address_of(body: &[u8]) -> Option<(SocketAddrV4, Vec<u8>)> {
    let (a, rest) = body.split_first_chunk::<ADDR_LEN>()?;
    let at = SocketAddrV4::new(
        Ipv4Addr::new(a[0], a[1], a[2], a[3]),
        u16::from_be_bytes([a[4], a[5]]),
    );
    Some((at, rest.to_vec()))
}

impl Msg {
    pub fn encode(&self) -> Vec<u8> {
        let (kind, body): (u8, &[u8]) = match self {
            Msg::Ask(at, d) => return addressed(ASK, at, d),
            Msg::SendTo(at, p) => return addressed(SEND_TO, at, p),
            Msg::Register(id) => (REGISTER, id),
            Msg::Request(d) => (REQUEST, d),
            Msg::Send(Dest::Info, p) => (SEND_INFO, p),
            Msg::Send(Dest::Heartbeat, p) => (SEND_HEARTBEAT, p),
        };
        let mut out = Vec::with_capacity(1 + body.len());
        out.push(kind);
        out.extend_from_slice(body);
        out
    }

    /// The message `bytes` carries; `None` for anything else.
    pub fn decode(bytes: &[u8]) -> Option<Msg> {
        let (&kind, body) = bytes.split_first()?;
        match kind {
            REGISTER => Some(Msg::Register(body.try_into().ok()?)),
            REQUEST => Some(Msg::Request(body.to_vec())),
            SEND_INFO => Some(Msg::Send(Dest::Info, body.to_vec())),
            SEND_HEARTBEAT => Some(Msg::Send(Dest::Heartbeat, body.to_vec())),
            ASK => address_of(body).map(|(at, d)| Msg::Ask(at, d)),
            SEND_TO => address_of(body).map(|(at, p)| Msg::SendTo(at, p)),
            _ => None,
        }
    }
}

/// How long the manager takes a member's reply to an asker
/// ([`Msg::SendTo`]) after it relayed that asker's request ([`Asks`]).
pub const ASK_HOLD_NS: u64 = 1_000_000_000;
/// Askers kept at most ([`Asks`]).
pub const MAX_ASKS: usize = 32;

/// The askers whose bridge-figures or clock-figures request the manager
/// relayed lately: a member's [`Msg::SendTo`] goes out only to one of
/// them, so no process on the PC can send from the Dante info port to any
/// address it names.
#[derive(Debug, Default)]
pub struct Asks {
    at: BTreeMap<SocketAddrV4, u64>,
}

impl Asks {
    /// A request from `asker` relayed at `now` (ns of the manager's clock).
    /// The askers past [`ASK_HOLD_NS`] are forgotten; a new one is not kept
    /// while [`MAX_ASKS`] are.
    pub fn note(&mut self, asker: SocketAddrV4, now: u64) {
        self.at
            .retain(|_, &mut t| now.saturating_sub(t) < ASK_HOLD_NS);
        if self.at.len() < MAX_ASKS || self.at.contains_key(&asker) {
            self.at.insert(asker, now);
        }
    }

    /// Whether `asker`'s request was relayed less than [`ASK_HOLD_NS`]
    /// before `now`.
    pub fn holds(&self, asker: &SocketAddrV4, now: u64) -> bool {
        self.at
            .get(asker)
            .is_some_and(|&t| now.saturating_sub(t) < ASK_HOLD_NS)
    }
}

/// Whether `to` is one host's address and port: no multicast, broadcast
/// or unspecified address, no port 0. Only there does a bridge-figures or
/// clock-figures reply go.
pub fn is_unicast(to: &SocketAddrV4) -> bool {
    let ip = to.ip();
    !(ip.is_multicast() || ip.is_broadcast() || ip.is_unspecified() || to.port() == 0)
}

/// Whether `packet` is a bridge-figures reply ([`super::bridge`]): its
/// length (a version 2 body, or a version 1 body from a member older than
/// dstack #419) and its opcode. A member's [`Msg::SendTo`] carries nothing
/// else.
pub fn is_bridge_reply(packet: &[u8]) -> bool {
    let body = packet.len().checked_sub(super::HEADER_LEN);
    matches!(
        body,
        Some(super::bridge::FIGURES_LEN | super::bridge::FIGURES_LEN_V2)
    ) && packet[24..32] == super::bridge::REPLY_OPCODE
}

/// Whether `packet` is a clock-figures reply ([`super::clock_figures`],
/// dstack #429): its length (a version 1 body) and its opcode. A member's
/// [`Msg::SendTo`] carries this or a bridge reply, nothing else.
pub fn is_clock_reply(packet: &[u8]) -> bool {
    packet.len().checked_sub(super::HEADER_LEN) == Some(super::clock_figures::FIGURES_LEN)
        && packet[24..32] == super::clock_figures::REPLY_OPCODE
}

/// The manager's members, by their loopback address.
#[derive(Debug, Default)]
pub struct Members {
    leases: BTreeMap<SocketAddr, ([u8; 8], u64)>,
}

impl Members {
    /// A member's registration from `from` at `now`; `false` when the
    /// table is full and `from` is not in it, or `from` is no loopback
    /// address (nothing off this PC is a member).
    pub fn register(&mut self, from: SocketAddr, id: [u8; 8], now: u64) -> bool {
        if !from.ip().is_loopback() {
            return false;
        }
        if self.leases.len() >= MAX_MEMBERS && !self.leases.contains_key(&from) {
            return false;
        }
        self.leases.insert(from, (id, now));
        true
    }

    /// Whether `from` is a member whose lease holds at `now`.
    pub fn is_live(&self, from: &SocketAddr, now: u64) -> bool {
        self.leases
            .get(from)
            .is_some_and(|&(_, at)| now.saturating_sub(at) < LEASE_NS)
    }

    /// The members whose lease holds at `now`; the others are forgotten.
    pub fn live(&mut self, now: u64) -> Vec<SocketAddr> {
        self.leases
            .retain(|_, &mut (_, at)| now.saturating_sub(at) < LEASE_NS);
        self.leases.keys().copied().collect()
    }
}

#[cfg(test)]
mod tests;
