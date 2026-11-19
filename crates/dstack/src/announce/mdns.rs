// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The process's one mDNS socket on an address (dstack #442). Up to
//! v0.6.192 every device had a daemon socket of its own, the process an
//! mDNS meter's beside it and, on Windows, every device a responder's
//! too, each joined to 224.0.0.251: the kernel delivered each datagram to
//! every one of them, on Windows inside the NIC's receive DPC (dasio#190,
//! dsg: 0.1-0.8 ms of DPC a datagram, late SoundGrid callbacks).
//!
//! - [`Mdns`]: the vendored daemon (`vendor/mdns-sd`, its IPv4 socket on
//!   `0.0.0.0:5353`, only the address's interface enabled), one per process
//!   and address, shared by every device there ([`Mdns::shared`]); its
//!   last holder's drop shuts it down. A device withdraws its own records
//!   ([`super::Announcer::announce`] of none, then [`Mdns::settle`]), so a
//!   device that stops never takes another's records down.
//! - The daemon's thread hands every datagram it reads to [`Taps::feed`]
//!   (`mdns_sd::ServiceDaemon::new_tapped`), which copies it into each
//!   [`Listener`]'s bounded channel that takes it ([`Heard`]): the
//!   address's mDNS meter ([`crate::discover::senders::Meter`], every
//!   datagram) and, on Windows, each device's responder on its own address
//!   ([`super::responder::Responder`], queries only). A full channel drops
//!   the datagram (counted, [`Taps::dropped`]): the daemon's thread never
//!   waits for a reader.
//! - The group membership is the daemon's: its IP check (every 5 s) leaves
//!   an address that is gone and joins it again when it comes back, also on
//!   another interface index (`check_ip_changes`).
//!
//! The subscriptions' direct queries (`subscribe::instance`) read their
//! legacy unicast answers on a port of their own, never on 5353.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use mdns_sd::{DaemonStatus, IfKind, Metrics, ServiceDaemon};
use tracing::{debug, info, warn};

use super::Announcer;
use crate::discover::senders::{MINUTE_NS, Meter, browsers::Browsers};

/// How long a withdrawal or the daemon's shutdown is waited for.
pub const GOODBYE_WAIT: Duration = Duration::from_secs(3);
/// The datagrams a listener's channel holds; past that one is dropped.
pub const QUEUE: usize = 256;

/// One datagram the daemon's socket received, and its source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Datagram {
    pub bytes: Vec<u8>,
    pub from: SocketAddrV4,
}

/// What a [`Listener`] takes of the datagrams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heard {
    /// Every datagram (the meter).
    All,
    /// Queries only: the DNS header's QR bit clear (a responder).
    Queries,
}

/// `bytes` is a DNS query: a header whose QR bit (byte 2's top bit) is
/// clear. Too short for a header, it is none.
pub fn is_query(bytes: &[u8]) -> bool {
    bytes.get(2).is_some_and(|b| *b < 0x80)
}

impl Heard {
    /// It takes the datagram `bytes`.
    pub fn takes(self, bytes: &[u8]) -> bool {
        match self {
            Heard::All => true,
            Heard::Queries => is_query(bytes),
        }
    }
}

/// The `n`th dropped datagram gets a warning: the 1st, 2nd, 4th, 8th, ...
/// (a reader that stalls is named, and the log stays bounded).
pub fn warn_at(n: u64) -> bool {
    n.is_power_of_two()
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One listener's channel.
struct Sub {
    id: u64,
    heard: Heard,
    to: SyncSender<Datagram>,
}

/// The listeners of one daemon's socket (see the module).
#[derive(Default)]
pub struct Taps {
    subs: Mutex<Vec<Sub>>,
    next: AtomicU64,
    dropped: AtomicU64,
}

impl Taps {
    /// A new listener taking `heard` of every datagram fed from now on.
    pub fn listen(self: &Arc<Self>, heard: Heard) -> Listener {
        let (to, from) = sync_channel(QUEUE);
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        lock(&self.subs).push(Sub { id, heard, to });
        Listener {
            taps: Arc::clone(self),
            id,
            from,
        }
    }

    /// The datagram `bytes` from `from` handed to every listener that takes
    /// it; an IPv6 source is none of ours (the daemon has no IPv6 socket).
    pub fn feed(&self, bytes: &[u8], from: SocketAddr) {
        let SocketAddr::V4(from) = from else {
            return;
        };
        for sub in lock(&self.subs).iter().filter(|s| s.heard.takes(bytes)) {
            let d = Datagram {
                bytes: bytes.to_vec(),
                from,
            };
            if let Err(TrySendError::Full(_)) = sub.to.try_send(d) {
                let n = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                if warn_at(n) {
                    warn!(
                        dropped = n,
                        queue = QUEUE,
                        "an mDNS datagram not handed on: its reader's queue is full"
                    );
                }
            }
        }
    }

    /// The datagrams dropped so far for a full channel.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// The listeners now.
    pub fn listeners(&self) -> usize {
        lock(&self.subs).len()
    }
}

/// A listener on a daemon's socket ([`Taps::listen`]); its drop ends it.
pub struct Listener {
    taps: Arc<Taps>,
    id: u64,
    from: Receiver<Datagram>,
}

impl std::fmt::Debug for Listener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Listener").field("id", &self.id).finish()
    }
}

impl Listener {
    /// The next datagram, waiting at most `wait`.
    pub fn recv_timeout(&self, wait: Duration) -> Result<Datagram, RecvTimeoutError> {
        self.from.recv_timeout(wait)
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        lock(&self.taps.subs).retain(|s| s.id != self.id);
    }
}

/// The daemons running in this process, by address.
static SHARED: Mutex<Vec<(Ipv4Addr, Weak<Mdns>)>> = Mutex::new(Vec::new());

/// The process's mDNS daemon on one address (see the module).
pub struct Mdns {
    daemon: ServiceDaemon,
    taps: Arc<Taps>,
    meter: Option<Arc<Meter>>,
}

impl std::fmt::Debug for Mdns {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mdns").finish_non_exhaustive()
    }
}

impl Mdns {
    /// The daemon on the interface with the address `ip`: the one already
    /// running in this process there, or a new one (with the address's
    /// mDNS meter, dstack #289).
    pub fn shared(ip: Ipv4Addr) -> Result<Arc<Mdns>, String> {
        let mut all = lock(&SHARED);
        all.retain(|(_, w)| w.strong_count() > 0);
        if let Some(m) = all
            .iter()
            .find(|(a, _)| *a == ip)
            .and_then(|(_, w)| w.upgrade())
        {
            return Ok(m);
        }
        let m = Arc::new(Mdns::open(ip)?);
        all.push((ip, Arc::downgrade(&m)));
        Ok(m)
    }

    /// A new daemon on `ip`'s interface only, IPv4 (no other network sees
    /// the records; mdns-sd applies the selections in order).
    fn open(ip: Ipv4Addr) -> Result<Mdns, String> {
        let taps = Arc::new(Taps::default());
        let feed = Arc::clone(&taps);
        let daemon = ServiceDaemon::new_tapped(Box::new(move |bytes, from| feed.feed(bytes, from)))
            .map_err(|e| format!("the mDNS daemon: {e}"))?;
        let selected = daemon
            .disable_interface(IfKind::All)
            .and_then(|()| daemon.enable_interface(IfKind::Addr(IpAddr::V4(ip))));
        let meter = Meter::start(taps.listen(Heard::All), ip, MINUTE_NS, Browsers::default());
        let m = Mdns {
            daemon,
            taps,
            meter: match meter {
                Ok(meter) => Some(Arc::new(meter)),
                Err(e) => {
                    warn!(%e, "the mDNS meter is not started: no mDNS senders in the status");
                    None
                }
            },
        };
        // A failed selection drops `m`, which shuts its daemon down.
        selected.map_err(|e| format!("the mDNS interface: {e}"))?;
        info!(
            meter = m.meter.is_some(),
            "mDNS: one socket on the address's interface, shared by its devices"
        );
        Ok(m)
    }

    /// The daemon: a device's records go to it through its own
    /// [`super::Announcer`] (each device withdraws only what it
    /// registered).
    pub fn daemon(&self) -> &ServiceDaemon {
        &self.daemon
    }

    /// The address's mDNS meter, when it started.
    pub fn meter(&self) -> Option<Arc<Meter>> {
        self.meter.clone()
    }

    /// A new listener on the daemon's socket.
    pub fn listen(&self, heard: Heard) -> Listener {
        self.taps.listen(heard)
    }

    /// The listeners on its socket now.
    pub fn listeners(&self) -> usize {
        self.taps.listeners()
    }

    /// Waits until the daemon has run every command sent before this call
    /// (a withdrawal's goodbyes): a round trip through its command queue,
    /// at most `wait`. Its counts of what it sent so far.
    pub fn settle(&self, wait: Duration) -> Result<Metrics, String> {
        let asked = self
            .daemon
            .get_metrics()
            .map_err(|e| format!("the mDNS daemon was not asked: {e}"))?;
        asked
            .recv_timeout(wait)
            .map_err(|e| format!("not within {} ms: {e}", wait.as_millis()))
    }

    /// A device's goodbye: the records `announcer` registered withdrawn
    /// (every other device's on the daemon kept), waited for until the
    /// daemon sent them, at most `wait`.
    pub fn withdraw(
        &self,
        announcer: &mut Announcer<ServiceDaemon>,
        wait: Duration,
    ) -> Result<(), String> {
        announcer
            .announce(&[])
            .map_err(|e| format!("the mDNS withdrawal: {e}"))?;
        self.settle(wait)
            .map(drop)
            .map_err(|e| format!("the mDNS goodbyes were not confirmed: {e}"))
    }
}

impl Drop for Mdns {
    /// The last holder's end: the daemon shut down (goodbyes for any record
    /// still registered), waited for at most [`GOODBYE_WAIT`].
    fn drop(&mut self) {
        let ended = self
            .daemon
            .shutdown()
            .map_err(|e| e.to_string())
            .and_then(|s| s.recv_timeout(GOODBYE_WAIT).map_err(|e| e.to_string()));
        match ended {
            Ok(DaemonStatus::Shutdown) => debug!("mDNS daemon stopped"),
            Ok(other) => warn!(?other, "the mDNS daemon's shutdown answered otherwise"),
            Err(e) => warn!(error = %e, "the mDNS daemon's shutdown was not confirmed"),
        }
    }
}

#[cfg(test)]
mod tests;
