// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! Multicast group membership kept across an interface change (dstack
//! #282).
//!
//! A socket joins its group once, on the interface its address names. On
//! Windows an address removed and added again (`netsh ... set address`),
//! often with a new interface index, drops that membership, and nothing
//! joins it again: the process runs on, unicast keeps working, but the
//! group's datagrams no longer arrive. A device's mDNS answers stopped and
//! a receiver could not find it again (seen live on a two-leg PC, 14.11.2026).
//!
//! - [`Membership`] (pure): the watch of one joined address. It compares
//!   each read of the interface list ([`Link`]: name, address, index) with
//!   the last one and asks a [`Rejoin`] when the address came back after
//!   it was gone, or its index changed.
//! - [`Membership::poll`]: reads the list at most every [`EVERY`].
//! - [`Membership::keep`]: a poll and, on a change, the owner's rejoin,
//!   logged once ("multicast group re-joined after an interface change");
//!   a failed rejoin is logged and asked again at the next poll.
//! - [`rejoin`]: leaves and joins a group again on the same socket.
//!
//! Known limit: a change is seen only by a read that finds the address
//! gone, or on another index. An address removed and added again on the
//! same index between two reads (under [`EVERY`]) is not seen; on Windows
//! the address is listed again only once its duplicate-address check is
//! done (`if_addrs` lists preferred addresses only), which narrows it.
//!
//! Each socket owner keeps its own [`Membership`] and calls `keep` from its
//! own loop (the mDNS responder, discovery, the latency listener, the
//! daemon's tick for the RX flows). `host` re-exports it as
//! `host::Membership`.

use std::fmt::Display;
use std::io;
use std::net::{Ipv4Addr, UdpSocket};
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

/// How often a watch reads the interface list.
pub const EVERY: Duration = Duration::from_secs(2);

/// One IPv4 address of an interface, with the interface's index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub name: String,
    pub ip: Ipv4Addr,
    /// The OS's interface index (`None` where the OS gives none).
    pub index: Option<u32>,
}

/// The box's IPv4 addresses with their interfaces' indexes.
pub fn links() -> io::Result<Vec<Link>> {
    Ok(if_addrs::get_if_addrs()?
        .into_iter()
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v4) => Some(Link {
                name: i.name,
                ip: v4.ip,
                index: i.index,
            }),
            if_addrs::IfAddr::V6(_) => None,
        })
        .collect())
}

/// A rejoin asked: the interface the address is on now, its index before
/// (when it was known) and now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejoin {
    pub interface: String,
    pub old_index: Option<u32>,
    pub new_index: Option<u32>,
}

/// What the watch saw of its address last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seen {
    /// Nothing read yet.
    Unknown,
    /// On an interface with this index.
    Up(Option<u32>),
    /// Missing; the index it had before (when it was seen).
    Gone(Option<u32>),
}

/// The watch of one joined address (see the module).
#[derive(Debug)]
pub struct Membership {
    ip: Ipv4Addr,
    seen: Seen,
    every: Duration,
    /// When the next poll reads the list (`None`: at once).
    next: Option<Instant>,
}

impl Membership {
    /// The watch of the address `ip`, read every [`EVERY`].
    pub fn new(ip: Ipv4Addr) -> Self {
        Self::every(ip, EVERY)
    }

    /// [`Membership::new`] read every `every`.
    pub fn every(ip: Ipv4Addr, every: Duration) -> Self {
        Membership {
            ip,
            seen: Seen::Unknown,
            every,
            next: None,
        }
    }

    /// Compares `links` with the last look: a [`Rejoin`] when the address
    /// is there again after it was gone (or missing at the first look), or
    /// on another index; `None` otherwise. The first look only learns.
    pub fn look(&mut self, links: &[Link]) -> Option<Rejoin> {
        let now = links.iter().find(|l| l.ip == self.ip);
        let (seen, rejoin) = match (self.seen, now) {
            (Seen::Unknown, Some(l)) => (Seen::Up(l.index), None),
            (Seen::Unknown, None) => (Seen::Gone(None), None),
            (Seen::Up(old), Some(l)) if old == l.index => (Seen::Up(old), None),
            (Seen::Up(old) | Seen::Gone(old), Some(l)) => (
                Seen::Up(l.index),
                Some(Rejoin {
                    interface: l.name.clone(),
                    old_index: old,
                    new_index: l.index,
                }),
            ),
            (Seen::Up(old), None) => {
                debug!(ip = %self.ip, ?old, "a joined address is gone from its interface");
                (Seen::Gone(old), None)
            }
            (Seen::Gone(old), None) => (Seen::Gone(old), None),
        };
        self.seen = seen;
        rejoin
    }

    /// The rejoin last asked failed: the next look asks it again.
    pub fn failed(&mut self) {
        if let Seen::Up(index) = self.seen {
            self.seen = Seen::Gone(index);
        }
    }

    /// At `now`, once the period since the last read has passed (at once
    /// the first time), reads the list with `read` and looks at it. A read
    /// that fails changes nothing (logged at debug).
    pub fn poll(
        &mut self,
        now: Instant,
        read: impl FnOnce() -> io::Result<Vec<Link>>,
    ) -> Option<Rejoin> {
        if self.next.is_some_and(|next| now < next) {
            return None;
        }
        self.next = Some(now + self.every);
        match read() {
            Ok(links) => self.look(&links),
            Err(e) => {
                debug!(error = %e, "the interface list could not be read");
                None
            }
        }
    }

    /// [`Membership::poll`], and on a change `rejoin` run for `group`:
    /// true when it rejoined (logged at INFO). A failed rejoin is logged
    /// as a warning and asked again at the next poll.
    pub fn keep(
        &mut self,
        now: Instant,
        read: impl FnOnce() -> io::Result<Vec<Link>>,
        group: impl Display,
        rejoin: impl FnOnce() -> io::Result<()>,
    ) -> bool {
        let Some(r) = self.poll(now, read) else {
            return false;
        };
        match rejoin() {
            Ok(()) => {
                info!(
                    interface = %r.interface,
                    old_index = r.old_index,
                    new_index = r.new_index,
                    %group,
                    "multicast group re-joined after an interface change"
                );
                true
            }
            Err(e) => {
                warn!(
                    interface = %r.interface,
                    %group,
                    error = %e,
                    "multicast group not re-joined after an interface change; tried again"
                );
                self.failed();
                false
            }
        }
    }
}

/// Leaves the multicast `group` on `ip`'s interface and joins it again, on
/// the same socket. A refused leave (no such membership any more, the old
/// one went with the address) does not matter; the join's error is the
/// result.
pub fn rejoin(sock: &UdpSocket, group: Ipv4Addr, ip: Ipv4Addr) -> io::Result<()> {
    let s = socket2::SockRef::from(sock);
    if let Err(e) = s.leave_multicast_v4(&group, &ip) {
        debug!(error = %e, %group, "the old membership could not be left");
    }
    s.join_multicast_v4(&group, &ip)
}

#[cfg(test)]
mod tests;
