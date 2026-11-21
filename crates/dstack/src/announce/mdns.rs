// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The process's one mDNS socket on an address (dstack #442). Up to
//! v0.6.192 every device had a daemon socket of its own, the process an
//! mDNS meter's beside it and, on Windows, every device a responder's
//! too, each joined to 224.0.0.251: the kernel delivered each datagram to
//! every one of them, on Windows inside the NIC's receive DPC (dasio#190,
//! dsg: 0.1-0.8 ms of DPC a datagram, late SoundGrid callbacks).
//!
//! - [`Mdns`]: the vendored daemon (`vendor/mdns-sd`, one IPv4 socket, only
//!   the address's interface enabled), one per process and address, shared
//!   by every device there ([`Mdns::shared`]); its last holder's drop shuts
//!   it down. A device withdraws its own records ([`Mdns::withdraw`]), so a
//!   device that stops never takes another's records down.
//! - Its socket is bound to `<address>:5353` exactly where the OS hands a
//!   joined address-bound socket the group ([`binds_to_address`],
//!   [`GROUP_TO_BOUND_SOCKET`]: Windows; dstack #444, #451), else to
//!   `0.0.0.0:5353` (Linux delivers no group to a bound socket, dev1
//!   19.11.2026). Bound, every send leaves from the address, so Dante
//!   Controller reads each answer once and from the device's own address
//!   (a SkipAsSource address's answers would leave from the primary one on
//!   the wildcard, dstack #1 problem E); up to v0.6.206 a separate responder
//!   socket per Windows device answered from the address beside the
//!   wildcard daemon (`announce::responder`'s answers now serve the tests'
//!   loopback responders only).
//! - The daemon's thread hands every datagram it reads to [`Taps::feed`]
//!   (`mdns_sd::ServiceDaemon::new_tapped_on`), which copies it into each
//!   [`Listener`]'s bounded channel that takes it ([`Heard`]): the
//!   address's mDNS meter ([`crate::discover::senders::Meter`], every
//!   datagram) and a device's discovery of our other devices on the PC
//!   while it runs (`traffic::neighbours`, every datagram,
//!   [`Mdns::listen_on`]). A full channel drops the datagram (counted,
//!   [`Taps::dropped`]): the daemon's thread never waits for a reader.
//! - The group membership is the daemon's (dstack #282's watch, its own
//!   form): its IP check, run every [`crate::membership::EVERY`] here as
//!   the watches' (mdns-sd's default is 5 s), leaves an address that is
//!   gone and joins it again when it comes back, also on another interface
//!   index (`check_ip_changes`). The watches' known limit holds: a re-add
//!   on the same index between two checks is not seen.
//! - A bound daemon also has a watch of its own (`dstack-mdns-bound`, the
//!   #282 watch every 2 s, [`Mdns::reopens`]). On Windows a socket whose
//!   address was removed and added again heard nothing more (mbc,
//!   19.11.2026: `mdns.per_s` 0.0 for 2.5 min with a bound socket; 20.11.2026
//!   the wildcard daemon with its responder answered nothing until a
//!   restart, #451): whether the socket went deaf or the IP check's join on
//!   it failed and left the interface out (both at debug) was not read. So
//!   once the address is back the watch asks the daemon to open its socket
//!   again (`mdns_sd::ServiceDaemon::reopen`: a new bind, the group joined
//!   on every enabled interface, every record announced once; records and
//!   taps kept) and logs [`BOUND_REOPENED`] with the interfaces joined and
//!   the records announced (mbc step 4a, 20.11.2026: `joined_interfaces=1`
//!   5 s after the add, answered again). The wildcard daemon has no such
//!   watch.
//! - The daemon's commands wait out a full queue (`announce::retried`): a
//!   device with more records than its queue holds (100) fills it at a
//!   withdrawal too.
//!
//! The subscriptions' direct queries (`subscribe::instance`) read their
//! legacy unicast answers on a port of their own, never on 5353. A
//! [`crate::discover::Discovery`] in a device process (its
//! `traffic::neighbours` starts one only while its NIC is loud past our
//! audio) reads the group from here too
//! ([`crate::discover::Discovery::start_heard`]); one in a process with no
//! device (dtray, a controller) joins a socket of its own.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{
    Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError, channel, sync_channel,
};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mdns_sd::{DaemonStatus, IfKind, IfPredicate, Metrics, Reopened, ServiceDaemon};
use tracing::{debug, info, warn};

use super::{AGAIN_TRIES, Announcer, retried};
use crate::discover::senders::{MINUTE_NS, Meter, browsers::Browsers};
use crate::membership::{Link, Lister, Membership};

/// mDNS's port.
pub const PORT: u16 = 5353;
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
    /// Queries only: the DNS header's QR bit clear.
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

/// Whether the OS hands a group datagram to a socket bound to a unicast
/// address that joined the group: Windows does (mbc, #442), Linux does not
/// (dev1, dstack #444); assumed not on any other OS, where it was never
/// measured (BSD stacks deliver only to the wildcard or the group).
pub const GROUP_TO_BOUND_SOCKET: bool = cfg!(windows);

/// Whether the address's daemon binds its socket to the address, given
/// whether the OS hands such a socket the group ([`GROUP_TO_BOUND_SOCKET`];
/// dstack #444, #451): exactly then. Bound, every answer leaves from the
/// address (one answer, no responder beside it) and the daemon's own watch
/// opens the socket again after an address cycle, where the wildcard daemon
/// on Windows went deaf until a restart (mbc, 20.11.2026). Where a bound
/// socket hears no group (Linux) it stays the wildcard's.
pub fn binds_to_address(group_to_bound: bool) -> bool {
    group_to_bound
}

/// Whether `intf` holds `ip`: the daemon's interface selection, asked at
/// each of its IP checks (dstack #451). mdns-sd turns an `IfKind::Addr`
/// selection into the interface's INDEX when it is set
/// (`resolve_addr_to_index`), so an interface re-created under a new index
/// with the same address (a USB NIC's reset; CI's `recreate` job) was never
/// selected again, and the daemon heard and answered nothing on it until a
/// restart. Matched by the address alone, as the device's address is what
/// names the Dante leg; another address on the same interface is not ours.
pub(crate) fn holds_address(intf: &if_addrs::Interface, ip: Ipv4Addr) -> bool {
    intf.ip() == IpAddr::V4(ip)
}

/// The INFO line of a bound daemon whose socket was opened again (dstack
/// #444).
pub const BOUND_REOPENED: &str =
    "mdns: the bound daemon's socket opened again after its address came back";

/// How long a bound daemon's watch waits for the daemon's answer to a
/// reopen (a bind, the group's join and one announcement a record). A
/// reopen answered later still happened; the watch counts it failed and
/// the next look reopens once more (one more announcement, harmless).
pub const REOPEN_WAIT: Duration = Duration::from_secs(1);

/// One look of a bound daemon's watch on `ip` at `now` (dstack #444, the
/// #282 watch): the list read with `read` at the watch's period and, when
/// the address is there again after a look found it gone (or on another
/// interface index), `reopen` run. True when it reopened (INFO
/// [`BOUND_REOPENED`], with the interfaces the new socket joined and the
/// records announced: 0 interfaces is a reopen that heard nothing yet);
/// a failed reopen is warned about and asked again at the next look.
fn keep_bound(
    m: &mut Membership,
    now: Instant,
    read: impl FnOnce() -> io::Result<Vec<Link>>,
    ip: Ipv4Addr,
    reopen: impl FnOnce() -> Result<Reopened, String>,
) -> bool {
    let Some(r) = m.poll(now, read) else {
        return false;
    };
    match reopen() {
        Ok(done) => {
            info!(
                %ip,
                interface = %r.interface,
                old_index = r.old_index,
                new_index = r.new_index,
                joined_interfaces = done.interfaces,
                announced_records = done.services,
                "{BOUND_REOPENED}"
            );
            true
        }
        Err(e) => {
            warn!(
                %ip,
                interface = %r.interface,
                error = %e,
                "mdns: the bound daemon's socket not opened again after its address came \
                 back; tried again"
            );
            m.failed();
            false
        }
    }
}

/// Asks `daemon` to open its bound socket again and waits for its answer
/// (a full queue waited out), at most [`REOPEN_WAIT`].
fn reopen(daemon: &ServiceDaemon) -> Result<Reopened, String> {
    let (asked, _waits) = retried(AGAIN_TRIES, || daemon.reopen());
    asked
        .map_err(|e| format!("the mDNS daemon was not asked: {e}"))?
        .recv_timeout(REOPEN_WAIT)
        .map_err(|e| format!("not within {} ms: {e}", REOPEN_WAIT.as_millis()))?
}

/// A bound daemon's watch of its address (dstack #444): a thread looking
/// at the interface list at its period ([`keep_bound`]), stopped and
/// joined by the daemon's drop before its shutdown.
struct BoundWatch {
    /// Never sent on: its drop is the stop.
    stop: Sender<()>,
    thread: JoinHandle<()>,
}

impl BoundWatch {
    /// The watch of `ip` reading `lister`, asking `daemon` to reopen and
    /// counting each reopen in `reopens`.
    fn start(
        daemon: ServiceDaemon,
        ip: Ipv4Addr,
        lister: Lister,
        reopens: Arc<AtomicU64>,
    ) -> io::Result<BoundWatch> {
        let (stop, stopped) = channel::<()>();
        let thread = std::thread::Builder::new()
            .name("dstack-mdns-bound".to_string())
            .spawn(move || {
                let Lister { mut read, every } = lister;
                let mut m = Membership::every(ip, every);
                loop {
                    if keep_bound(&mut m, Instant::now(), &mut read, ip, || reopen(&daemon)) {
                        reopens.fetch_add(1, Ordering::Relaxed);
                    }
                    // Nothing is ever sent: the sender's drop is the stop.
                    if stopped.recv_timeout(every) != Err(RecvTimeoutError::Timeout) {
                        break;
                    }
                }
            })?;
        Ok(BoundWatch { stop, thread })
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
    /// Its socket is bound to the address, not the wildcard (dstack #444).
    bound: bool,
    /// A bound daemon's watch of its address (dstack #444); none for the
    /// wildcard's.
    watch: Option<BoundWatch>,
    /// How often the bound socket was opened again.
    reopens: Arc<AtomicU64>,
}

impl Mdns {
    /// The daemon on the interface with the address `ip`: the one already
    /// running in this process there, or a new one (with the address's
    /// mDNS meter, dstack #289), its socket bound to `ip` where
    /// [`binds_to_address`] says so (Windows), else to the wildcard. A new
    /// one is opened without the registry's lock (a failed one waits for
    /// its shutdown), so two devices starting at once may open two: the
    /// later is dropped and the first kept. A bound socket is opened before
    /// this returns: its failed bind is this call's error.
    pub fn shared(ip: Ipv4Addr) -> Result<Arc<Mdns>, String> {
        Mdns::shared_bound(ip, binds_to_address(GROUP_TO_BOUND_SOCKET))
    }

    /// [`Mdns::shared`], a new daemon's socket bound to `ip` when `bound`,
    /// else to the wildcard: the tests pin either form on any OS. The
    /// daemon already running on `ip` is shared whatever it was opened with
    /// (one socket an address); a caller asking the other form is told so
    /// in a warning.
    pub(crate) fn shared_bound(ip: Ipv4Addr, bound: bool) -> Result<Arc<Mdns>, String> {
        Mdns::shared_listed(ip, bound, Lister::default())
    }

    /// [`Mdns::shared_bound`] whose new bound daemon's watch reads `lister`
    /// (the OS's list every 2 s; a test's own).
    fn shared_listed(ip: Ipv4Addr, bound: bool, lister: Lister) -> Result<Arc<Mdns>, String> {
        if let Some(m) = running(&mut lock(&SHARED), ip) {
            return Ok(m.kept_for(bound));
        }
        let opened = Arc::new(Mdns::open(ip, bound, lister)?);
        let mut all = lock(&SHARED);
        if let Some(first) = running(&mut all, ip) {
            drop(all);
            return Ok(first.kept_for(bound));
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
    /// at the watches' period; a bound one with its watch on `lister`
    /// (dstack #444).
    fn open(ip: Ipv4Addr, bound: bool, lister: Lister) -> Result<Mdns, String> {
        let taps = Arc::new(Taps::default());
        let feed = Arc::clone(&taps);
        let tap: mdns_sd::Tap = Box::new(move |bytes, from| feed.feed(bytes, from));
        let bind = if bound { ip } else { Ipv4Addr::UNSPECIFIED };
        let daemon =
            ServiceDaemon::new_tapped_on(tap, bind).map_err(|e| format!("the mDNS daemon: {e}"))?;
        let every = u32::try_from(crate::membership::EVERY.as_secs()).unwrap_or(u32::MAX);
        let selected = daemon
            .disable_interface(IfKind::All)
            .and_then(|()| {
                daemon.enable_interface(IfKind::Predicate(IfPredicate::new(move |intf| {
                    holds_address(intf, ip)
                })))
            })
            .and_then(|()| daemon.set_ip_check_interval(every));
        let meter = Meter::start(taps.listen(Heard::All), MINUTE_NS, Browsers::default());
        let mut m = Mdns {
            daemon,
            taps,
            meter: match meter {
                Ok(meter) => Some(Arc::new(meter)),
                Err(e) => {
                    warn!(%e, "the mDNS meter is not started: no mDNS senders in the status");
                    None
                }
            },
            bound,
            watch: None,
            reopens: Arc::new(AtomicU64::new(0)),
        };
        // A failed selection drops `m`, which shuts its daemon down.
        selected.map_err(|e| format!("the mDNS interface: {e}"))?;
        info!(
            meter = m.meter.is_some(),
            bound, "mDNS: one socket on the address's interface, shared by its devices"
        );
        if bound {
            let watch = BoundWatch::start(m.daemon.clone(), ip, lister, Arc::clone(&m.reopens))
                .map_err(|e| format!("the bound mDNS daemon's watch: {e}"))?;
            m.watch = Some(watch);
        }
        Ok(m)
    }

    /// This running daemon, for a caller that asked for a socket `bound`
    /// to the address or not: one asking the other form gets it as it is,
    /// with a warning (one socket an address). Only tests ask both forms on
    /// one address: every production caller goes through [`Mdns::shared`].
    fn kept_for(self: Arc<Self>, bound: bool) -> Arc<Self> {
        if bound != self.bound {
            warn!(
                asked_bound = bound,
                running_bound = self.bound,
                "the mDNS socket's form asked differs from the running daemon's on the \
                 address: the running one is shared"
            );
        }
        self
    }

    /// Its socket is bound to the address (dstack #444), not the
    /// wildcard.
    pub fn bound(&self) -> bool {
        self.bound
    }

    /// How often the bound daemon's socket was opened again after its
    /// address came back (dstack #444); always 0 for the wildcard's.
    pub fn reopens(&self) -> u64 {
        self.reopens.load(Ordering::Relaxed)
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
    /// [`SHUTDOWN_WAIT`]. A bound daemon's watch ends first, so it asks
    /// the daemon nothing more.
    fn drop(&mut self) {
        if let Some(watch) = self.watch.take() {
            drop(watch.stop);
            let _ended = watch.thread.join();
        }
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
