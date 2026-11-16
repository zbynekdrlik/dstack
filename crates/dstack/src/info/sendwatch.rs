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
//! - the log: a failure is reported at once when no report went within the
//!   last [`REPORT_EVERY`], else with the next due one ([`Report`]: the
//!   failures since the last report and the last OS error);
//!   [`SendWatch::flush`] reports the rest once due, so no failure goes
//!   uncounted and no packet gets a line of its own, even when failures
//!   and sends alternate;
//! - the recovery, after [`membership`](crate::membership)'s pattern (the
//!   owner acts on its own socket): [`REAPPLY_AT`] failures in a row ask
//!   the multicast interface named again ([`Recover::Reapply`]),
//!   [`REOPEN_AT`] in a row the socket reopened ([`Recover::Reopen`]). Each
//!   kind waits a period since its last: [`RECOVER_EVERY`] at first,
//!   doubled at each reopen up to [`RECOVER_CAP`] while the failures go on
//!   (dsg's state lasted a day: a reopen a minute would have been 1440
//!   rebinds and their lines);
//! - [`SendWatch::sent`]: a send that went out after a run of failures with
//!   a recovery in it ends the outage (the period starts again at
//!   [`RECOVER_EVERY`]) and takes the failures not reported yet: the server
//!   logs that sends work again, with the outage's count.
//!
//! The watch is the socket's, not a group's: when only one group fails
//! while the other's sends go out, each send resets the run, so no
//! recovery is asked (dsg's state failed both groups).
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
// The start's two sends (board and product info) never ask a reopen.
const _: () = assert!(REOPEN_AT > 2);
/// The least time between two recoveries of one kind, at first.
pub const RECOVER_EVERY: Duration = Duration::from_secs(60);
/// The longest the period between recoveries grows to.
pub const RECOVER_CAP: Duration = Duration::from_secs(3600);

/// The period between recoveries after `reopens` reopens in one outage:
/// [`RECOVER_EVERY`] doubled each time, at most [`RECOVER_CAP`].
pub fn recover_period(reopens: u32) -> Duration {
    RECOVER_EVERY
        .saturating_mul(1u32.checked_shl(reopens).unwrap_or(u32::MAX))
        .min(RECOVER_CAP)
}

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
    /// Reopens in this outage (the recovery period's doublings).
    reopens: u32,
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
        let period = recover_period(self.reopens);
        let recover = if self.run >= REOPEN_AT && due(self.last_reopen, period, now) {
            self.last_reopen = Some(now);
            self.run = 0;
            self.reopens += 1;
            Some(Recover::Reopen)
        } else if self.run >= REAPPLY_AT && due(self.last_reapply, period, now) {
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
    /// recovery was asked in it (the server logs that sends work again);
    /// those not reported yet are taken with it.
    pub fn sent(&mut self) -> Option<u64> {
        self.run = 0;
        self.reopens = 0;
        let outage = std::mem::take(&mut self.outage);
        if !std::mem::take(&mut self.recovered) {
            return None;
        }
        self.unreported = 0;
        Some(outage)
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
