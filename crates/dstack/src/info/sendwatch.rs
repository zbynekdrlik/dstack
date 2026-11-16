// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The info server's watch on its multicast sends (dstack #340).
//!
//! On a Windows PC after a reboot (dsg, 15.-16.11.2026) every heartbeat and
//! info packet the info server sent to its groups failed with
//! WSAEADDRNOTAVAIL (10049) for about 24 hours, while the device's address
//! and unicast worked; a new socket did not clear it, an adapter restart
//! did. The cause inside Windows is unproven. The server logged one WARN a
//! packet (11 678 lines) and nothing else happened.
//!
//! [`SendWatch`] (pure) takes each send's result and the time:
//!
//! - the log: the first failure is reported at once, later ones at most
//!   once a [`REPORT_EVERY`] ([`Report`]: the failures since the last
//!   report and the last OS error); [`SendWatch::flush`] reports the rest
//!   once due, so no failure goes uncounted and no packet gets a line of
//!   its own;
//! - the recovery, after [`membership`](crate::membership)'s pattern (the
//!   owner acts on its own socket): [`REAPPLY_AT`] failures in a row ask
//!   the multicast interface named again ([`Recover::Reapply`]),
//!   [`REOPEN_AT`] in a row the socket reopened ([`Recover::Reopen`]), each
//!   at most once a [`RECOVER_EVERY`];
//! - [`SendWatch::sent`]: a send that went out after a run of failures with
//!   a recovery in it ends the run: the server logs that sends work again.
//!
//! [`index_form`] is the interface index as `IP_MULTICAST_IF` takes it on
//! Windows (an address in 0.0.0.0/8), [`interface_by_index`] the re-apply
//! that falls back to it.

use std::io;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use crate::membership::Link;

/// The least time between two reports of failed sends.
pub const REPORT_EVERY: Duration = Duration::from_secs(60);
/// Failures in a row that ask the multicast interface named again.
pub const REAPPLY_AT: u32 = 3;
/// Failures in a row that ask the socket reopened.
pub const REOPEN_AT: u32 = 10;
/// The least time between two recoveries of one kind.
pub const RECOVER_EVERY: Duration = Duration::from_secs(60);

/// A recovery asked of the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recover {
    /// Name the socket's multicast interface again (by the address, else
    /// by its interface's index now).
    Reapply,
    /// Close the socket and bind its address again.
    Reopen,
}

/// The failed sends since the last report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub failures: u64,
    /// The last failure's error, as text.
    pub error: String,
}

/// What one failed send asks of the server.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Failed {
    pub report: Option<Report>,
    pub recover: Option<Recover>,
}

/// The watch on the multicast sends of one server (see the module).
#[derive(Debug, Default)]
pub struct SendWatch {
    /// Failures in a row since the last send that went out or reopen.
    run: u32,
    /// Failures in a row since the last send that went out.
    outage: u64,
    /// A recovery was asked during this outage.
    recovered: bool,
    /// Failures not reported yet.
    unreported: u64,
    error: String,
    /// When the next report may go (`None`: at once).
    next_report: Option<Instant>,
    last_reapply: Option<Instant>,
    last_reopen: Option<Instant>,
}

/// `last` plus `every` is past at `now` (or nothing was done yet).
fn due(last: Option<Instant>, every: Duration, now: Instant) -> bool {
    due_at(last.map(|t| t + every), now)
}

impl SendWatch {
    /// A send failed with `error` at `now`.
    pub fn failed(&mut self, now: Instant, error: &io::Error) -> Failed {
        self.run += 1;
        self.outage += 1;
        self.unreported += 1;
        self.error = error.to_string();
        let report = self.flush(now);
        let recover = if self.run >= REOPEN_AT && due(self.last_reopen, RECOVER_EVERY, now) {
            self.last_reopen = Some(now);
            self.run = 0;
            Some(Recover::Reopen)
        } else if self.run >= REAPPLY_AT && due(self.last_reapply, RECOVER_EVERY, now) {
            self.last_reapply = Some(now);
            Some(Recover::Reapply)
        } else {
            None
        };
        self.recovered |= recover.is_some();
        Failed { report, recover }
    }

    /// The failures not reported yet, once a report is due at `now`.
    pub fn flush(&mut self, now: Instant) -> Option<Report> {
        if self.unreported == 0 || !due_at(self.next_report, now) {
            return None;
        }
        self.next_report = Some(now + REPORT_EVERY);
        Some(Report {
            failures: std::mem::take(&mut self.unreported),
            error: self.error.clone(),
        })
    }

    /// A send went out: the failures of the outage it ends, when a
    /// recovery was asked in it (the server logs that sends work again).
    pub fn sent(&mut self) -> Option<u64> {
        self.run = 0;
        let outage = std::mem::take(&mut self.outage);
        std::mem::take(&mut self.recovered).then_some(outage)
    }
}

/// `next` is reached at `now` (or none is set).
fn due_at(next: Option<Instant>, now: Instant) -> bool {
    next.is_none_or(|t| now >= t)
}

/// The interface index as `IP_MULTICAST_IF` takes it on Windows: an
/// address in 0.0.0.0/8 is read as an index (in network byte order);
/// `None` past that range.
pub fn index_form(index: u32) -> Option<Ipv4Addr> {
    (index < 1 << 24).then(|| Ipv4Addr::from(index))
}

/// How the multicast interface was named again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Named {
    ByAddress,
    /// By this interface index.
    ByIndex(u32),
}

/// Names the multicast interface of `ip` again with `set`: by the address,
/// and when that fails and `by_index` (Windows), by the index the
/// address's interface has now in `links`. The address's error is the
/// result when no index is tried or found.
pub fn interface_by_index(
    ip: Ipv4Addr,
    by_index: bool,
    mut set: impl FnMut(Ipv4Addr) -> io::Result<()>,
    links: impl FnOnce() -> io::Result<Vec<Link>>,
) -> io::Result<Named> {
    let by_address = match set(ip) {
        Ok(()) => return Ok(Named::ByAddress),
        Err(e) => e,
    };
    if !by_index {
        return Err(by_address);
    }
    let index = links()?
        .into_iter()
        .find(|l| l.ip == ip)
        .and_then(|l| l.index)
        .ok_or(by_address)?;
    let form = index_form(index).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("interface index {index} past IP_MULTICAST_IF's index form"),
        )
    })?;
    set(form).map(|()| Named::ByIndex(index))
}

#[cfg(test)]
mod tests;
