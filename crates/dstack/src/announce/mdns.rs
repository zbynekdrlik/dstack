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
//!   ([`Mdns::withdraw`]), so a device that stops never takes another's
//!   records down.
//! - The daemon's thread hands every datagram it reads to [`Taps::feed`]
//!   (`mdns_sd::ServiceDaemon::new_tapped`), which copies it into each
//!   [`Listener`]'s bounded channel that takes it ([`Heard`]): the
//!   address's mDNS meter ([`crate::discover::senders::Meter`], every
//!   datagram), on Windows each device's responder on its own address
//!   ([`super::responder::Responder`], queries only), and a device's
//!   discovery of our other devices on the PC while it runs
//!   (`traffic::neighbours`, every datagram, [`Mdns::listen_on`]). A full
//!   channel drops the datagram (counted, [`Taps::dropped`]): the daemon's
//!   thread never waits for a reader.
//! - The group membership is the daemon's (dstack #282's watch, its own
//!   form): its IP check, run every [`crate::membership::EVERY`] here as
//!   the watches' (mdns-sd's default is 5 s), leaves an address that is
//!   gone and joins it again when it comes back, also on another interface
//!   index (`check_ip_changes`). The watches' known limit holds: a re-add
//!   on the same index between two checks is not seen.
//! - The daemon's commands wait out a full queue (`announce::retried`): a device with
//!   more records than its queue holds (100) fills it at a withdrawal too.
//!
//! The subscriptions' direct queries (`subscribe::instance`) read their
//! legacy unicast answers on a port of their own, never on 5353. A
//! [`crate::discover::Discovery`] in a device process (its
//! `traffic::neighbours` starts one only while its NIC is loud past our
//! audio) reads the group from here too
//! ([`crate::discover::Discovery::start_heard`]); one in a process with no
//! device (dtray, a controller) joins a socket of its own.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use mdns_sd::{DaemonStatus, IfKind, Metrics, ServiceDaemon};
use tracing::{debug, info, warn};

use super::{AGAIN_TRIES, Announcer, retried};
use crate::discover::senders::{MINUTE_NS, Meter, browsers::Browsers};

/// How long a withdrawal is waited for.
pub const GOODBYE_WAIT: Duration = Duration::from_secs(3);
/// How long the daemon's shutdown is waited for: after a withdrawal it has
/// nothing left to send, so a device's stop stays within dlink's 5 s.
pub const SHUTDOWN_WAIT: Duration = Duration::from_secs(1);
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
/// (the log stays bounded; each line names the reader's kind).
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
                        reader = ?sub.heard,
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

/// A daemon running in this process: its address, the daemon and its
/// taps (a listener is taken from the taps alone, [`Mdns::listen_on`]).
struct Entry {
    ip: Ipv4Addr,
    mdns: Weak<Mdns>,
    taps: Weak<Taps>,
}

/// The daemons running in this process.
static SHARED: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

/// The entry of the running daemon on `ip` in `all`, if any (dead entries
/// dropped).
fn live(all: &mut Vec<Entry>, ip: Ipv4Addr) -> Option<&Entry> {
    all.retain(|e| e.mdns.strong_count() > 0);
    all.iter().find(|e| e.ip == ip)
}

/// The running daemon on `ip` in `all`, if any.
fn running(all: &mut Vec<Entry>, ip: Ipv4Addr) -> Option<Arc<Mdns>> {
    live(all, ip).and_then(|e| e.mdns.upgrade())
}

/// The process's mDNS daemon on one address (see the module).
pub struct Mdns {
    daemon: ServiceDaemon,
    taps: Arc<Taps>,
    meter: Option<Arc<Meter>>,
}

impl Mdns {
    /// The daemon on the interface with the address `ip`: the one already
    /// running in this process there, or a new one (with the address's
    /// mDNS meter, dstack #289). A new one is opened without the registry's
    /// lock (a failed one waits for its shutdown), so two devices starting
    /// at once may open two: the later is dropped and the first kept.
    pub fn shared(ip: Ipv4Addr) -> Result<Arc<Mdns>, String> {
        if let Some(m) = running(&mut lock(&SHARED), ip) {
            return Ok(m);
        }
        let opened = Arc::new(Mdns::open(ip)?);
        let mut all = lock(&SHARED);
        if let Some(first) = running(&mut all, ip) {
            drop(all);
            return Ok(first);
        }
        all.push(Entry {
            ip,
            mdns: Arc::downgrade(&opened),
            taps: Arc::downgrade(&opened.taps),
        });
        Ok(opened)
    }

    /// A new daemon on `ip`'s interface only, IPv4 (no other network sees
    /// the records; mdns-sd applies the selections in order), its IP check
    /// at the watches' period.
    fn open(ip: Ipv4Addr) -> Result<Mdns, String> {
        let taps = Arc::new(Taps::default());
        let feed = Arc::clone(&taps);
        let daemon = ServiceDaemon::new_tapped(Box::new(move |bytes, from| feed.feed(bytes, from)))
            .map_err(|e| format!("the mDNS daemon: {e}"))?;
        let every = u32::try_from(crate::membership::EVERY.as_secs()).unwrap_or(u32::MAX);
        let selected = daemon
            .disable_interface(IfKind::All)
            .and_then(|()| daemon.enable_interface(IfKind::Addr(IpAddr::V4(ip))))
            .and_then(|()| daemon.set_ip_check_interval(every));
        let meter = Meter::start(taps.listen(Heard::All), MINUTE_NS, Browsers::default());
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

    /// A new listener on this process's running daemon on `ip`, if one
    /// runs there; none is opened. Only the daemon's taps are taken, never
    /// the daemon, so its last device's drop always shuts it down (never
    /// the caller's thread); once it ended, the listener hears nothing
    /// more.
    pub fn listen_on(ip: Ipv4Addr, heard: Heard) -> Option<Listener> {
        let taps = live(&mut lock(&SHARED), ip)?.taps.upgrade()?;
        Some(taps.listen(heard))
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
    /// (a withdrawal's goodbyes): a round trip through its command queue
    /// (a full queue waited out), at most `wait` for the answer. Its counts
    /// of what it sent so far.
    pub fn settle(&self, wait: Duration) -> Result<Metrics, String> {
        let (asked, _waits) = retried(AGAIN_TRIES, || self.daemon.get_metrics());
        asked
            .map_err(|e| format!("the mDNS daemon was not asked: {e}"))?
            .recv_timeout(wait)
            .map_err(|e| format!("not within {} ms: {e}", wait.as_millis()))
    }

    /// A device's goodbye: the records `announcer` registered withdrawn
    /// (every other device's on the daemon kept), waited for until the
    /// daemon sent them, at most `wait`. A name registered again within
    /// 120 ms on a daemon another device keeps running may see the
    /// withdrawal's repeated goodbye after its announcement; the daemon's
    /// own repeat a second later announces it again.
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
    /// still registered; a full queue waited out), waited for at most
    /// [`SHUTDOWN_WAIT`].
    fn drop(&mut self) {
        let (asked, _waits) = retried(AGAIN_TRIES, || self.daemon.shutdown());
        let ended = asked
            .map_err(|e| e.to_string())
            .and_then(|s| s.recv_timeout(SHUTDOWN_WAIT).map_err(|e| e.to_string()));
        match ended {
            Ok(DaemonStatus::Shutdown) => debug!("mDNS daemon stopped"),
            Ok(other) => warn!(?other, "the mDNS daemon's shutdown answered otherwise"),
            Err(e) => warn!(error = %e, "the mDNS daemon's shutdown was not confirmed"),
        }
    }
}

#[cfg(test)]
mod tests;
