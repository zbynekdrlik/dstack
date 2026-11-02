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
//! - When the manager ends, 8700 frees; a member that binds it becomes the
//!   manager, and the others' leases reach it on the same loopback port.
//!
//! This module holds the messages and the members' table (pure); the
//! loops are in [`super::serve_info`] and [`super::serve_member`].

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
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
}

const REGISTER: u8 = b'R';
const REQUEST: u8 = b'Q';
const SEND_INFO: u8 = b'I';
const SEND_HEARTBEAT: u8 = b'H';

impl Msg {
    pub fn encode(&self) -> Vec<u8> {
        let (kind, body): (u8, &[u8]) = match self {
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
            _ => None,
        }
    }
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
