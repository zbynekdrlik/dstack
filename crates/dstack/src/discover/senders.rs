// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! Who sends how much mDNS on the Dante leg (dstack #289): a flood is named
//! within a minute, not after a night of tracing (#285: one program sent
//! about 40 datagrams a second to the group, and every host had to take
//! them).
//!
//! - [`Senders`] (pure, fake times): the datagrams of the minute counted
//!   per source address, at most [`MAX_SOURCES`] sources (a new one past
//!   that takes the place of the one with the fewest, so a flood that
//!   starts late is still named); [`Senders::roll`] closes the minute.
//! - [`Minute`]: one closed minute: its total, the [`TOP`] loudest sources
//!   and every source over [`CAP_PER_S`]; [`Minute::loud`] says whether
//!   its line is worth logging (over [`LINE_FLOOR_PER_S`] in all).
//! - [`report`]: the minute's lines: INFO `mDNS senders in the last
//!   minute` (only a loud minute) and a WARN `an mDNS sender is over the
//!   cap` per source over the cap.
//! - [`Meter`]: the process's passive listener on one address (one per
//!   process and address, shared by every device there; its own thread,
//!   nothing sent) and the last minute for the devices' status. It counts
//!   the group as joined on that address's interface only: Linux is told
//!   so (`IP_MULTICAST_ALL` off; it would hand over every membership of the
//!   host), and Windows delivers by the socket's own membership (checked
//!   on a two-leg PC, 14.11.2026: 20 s, every source on the Dante subnet,
//!   none from the LAN's mDNS). On Windows the socket is bound to the
//!   wildcard address (`discover`'s group listener), so it may also take
//!   a unicast datagram to port 5353 meant for another wildcard socket
//!   there, as a [`super::Discovery`] does; mDNS answers go to the group.

use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tracing::{debug, info, trace, warn};

use crate::membership::{Link, Membership, links, rejoin};
use crate::subscribe::instance::MDNS;

/// Sources counted at most in a minute.
pub const MAX_SOURCES: usize = 256;
/// The sources a minute's line and the status name.
pub const TOP: usize = 5;
/// A minute's datagrams a second, all sources, over which its line is
/// logged (a quiet network stays quiet).
pub const LINE_FLOOR_PER_S: u64 = 10;
/// One source's datagrams a second, averaged over the minute, over which
/// it is warned about: the switch's and the host filters' cap (#286, #288).
pub const CAP_PER_S: u64 = 50;
/// The minute, ns.
pub const MINUTE_NS: u64 = 60_000_000_000;

/// One source's datagrams in a minute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    pub source: Ipv4Addr,
    pub packets: u64,
}

/// One closed minute (see the module).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Minute {
    /// Its length, ns (at least [`MINUTE_NS`] in the meter).
    pub ns: u64,
    /// Every datagram, also those of sources no longer listed.
    pub total: u64,
    /// The loudest [`TOP`] sources, loudest first (equal counts by
    /// address).
    pub top: Vec<Rate>,
    /// Every source over [`CAP_PER_S`], loudest first.
    pub over_cap: Vec<Rate>,
}

/// `packets` in `ns` over `per_s` a second: `packets / ns > per_s / 1e9`.
fn over(packets: u64, ns: u64, per_s: u64) -> bool {
    u128::from(packets) * 1_000_000_000 > u128::from(per_s) * u128::from(ns)
}

impl Minute {
    /// `packets` a second over the minute (0 for an empty one).
    pub fn per_s(&self, packets: u64) -> f64 {
        if self.ns == 0 {
            return 0.0;
        }
        packets as f64 * 1e9 / self.ns as f64
    }

    /// Over [`LINE_FLOOR_PER_S`] in all.
    pub fn loud(&self) -> bool {
        over(self.total, self.ns, LINE_FLOOR_PER_S)
    }

    /// `rates` as `<address> <n>/s` items, comma-separated.
    pub fn list(&self, rates: &[Rate]) -> String {
        rates
            .iter()
            .map(|r| format!("{} {:.1}/s", r.source, self.per_s(r.packets)))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The minute's counts (see the module).
#[derive(Debug)]
pub struct Senders {
    start_ns: u64,
    counts: HashMap<Ipv4Addr, u64>,
    total: u64,
}

impl Senders {
    /// A minute starting at `now_ns`.
    pub fn new(now_ns: u64) -> Self {
        Senders {
            start_ns: now_ns,
            counts: HashMap::with_capacity(MAX_SOURCES),
            total: 0,
        }
    }

    /// One datagram from `source`.
    pub fn count(&mut self, source: Ipv4Addr) {
        self.total += 1;
        if let Some(n) = self.counts.get_mut(&source) {
            *n += 1;
            return;
        }
        if self.counts.len() == MAX_SOURCES {
            // The fewest leave (equal counts: the highest address).
            let quietest = self
                .counts
                .iter()
                .min_by_key(|(a, n)| (**n, std::cmp::Reverse(**a)))
                .map(|(a, _)| *a);
            if let Some(a) = quietest {
                self.counts.remove(&a);
            }
        }
        self.counts.insert(source, 1);
    }

    /// The sources counted now.
    pub fn sources(&self) -> usize {
        self.counts.len()
    }

    /// The minute closed at `now_ns` once it is [`MINUTE_NS`] old, the
    /// next one started; `None` before. A clock that went back starts a
    /// new minute and drops the counts.
    pub fn roll(&mut self, now_ns: u64) -> Option<Minute> {
        self.roll_after(now_ns, MINUTE_NS)
    }

    /// [`Senders::roll`] with a minute of `minute_ns`.
    pub fn roll_after(&mut self, now_ns: u64, minute_ns: u64) -> Option<Minute> {
        if now_ns < self.start_ns {
            *self = Senders::new(now_ns);
            return None;
        }
        let ns = now_ns - self.start_ns;
        if ns < minute_ns {
            return None;
        }
        let mut rates: Vec<Rate> = self
            .counts
            .drain()
            .map(|(source, packets)| Rate { source, packets })
            .collect();
        rates.sort_by_key(|r| (std::cmp::Reverse(r.packets), r.source));
        let over_cap = rates
            .iter()
            .copied()
            .filter(|r| over(r.packets, ns, CAP_PER_S))
            .collect();
        rates.truncate(TOP);
        let minute = Minute {
            ns,
            total: self.total,
            top: rates,
            over_cap,
        };
        self.start_ns = now_ns;
        self.total = 0;
        Some(minute)
    }
}

/// The minute's lines (see the module).
pub fn report(m: &Minute) {
    if m.loud() {
        info!(
            per_s = format_args!("{:.1}", m.per_s(m.total)),
            top = %m.list(&m.top),
            "mDNS senders in the last minute"
        );
    }
    for r in &m.over_cap {
        warn!(
            source = %r.source,
            per_s = format_args!("{:.1}", m.per_s(r.packets)),
            cap_per_s = CAP_PER_S,
            "an mDNS sender is over the cap"
        );
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The meters running in this process, by address.
static METERS: Mutex<Vec<(Ipv4Addr, Weak<Meter>)>> = Mutex::new(Vec::new());

/// The process's mDNS meter on one address (see the module): it ends with
/// its last holder.
#[derive(Debug)]
pub struct Meter {
    last: Arc<Mutex<Option<Minute>>>,
    stop: Arc<AtomicBool>,
    rejoins: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

/// How the meter's socket keeps its group (dstack #282): the watch of the
/// address and the interface list it reads ([`links`]; a test: its own).
pub struct MeterWatch {
    pub membership: Membership,
    pub read: Box<dyn FnMut() -> io::Result<Vec<Link>> + Send>,
}

/// The meter's socket's receive wait (it looks at the stop between two).
const WAIT: Duration = Duration::from_millis(250);

impl Meter {
    /// The meter on the interface with the address `ip`: the one already
    /// running in this process there, or a new one listening to the mDNS
    /// group as a passive member (`discover`'s group listener, joined
    /// again after an interface change, dstack #282). One per process
    /// and address, so its lines are logged once whatever the devices.
    pub fn shared(ip: Ipv4Addr) -> io::Result<Arc<Meter>> {
        Meter::shared_with(ip, || Meter::open(ip, MINUTE_NS))
    }

    /// A new meter listening to the mDNS group on the interface with the
    /// address `ip`, in minutes of `minute_ns` ([`Meter::shared`]'s, not
    /// registered).
    pub fn open(ip: Ipv4Addr, minute_ns: u64) -> io::Result<Meter> {
        let sock = super::group_listener(ip)?;
        // Only the group as joined on this socket's interface: Linux
        // otherwise hands it every membership of the host (the LAN's).
        #[cfg(target_os = "linux")]
        socket2::SockRef::from(&sock).set_multicast_all_v4(false)?;
        sock.set_read_timeout(Some(WAIT))?;
        let watch = MeterWatch {
            membership: Membership::new(ip),
            read: Box::new(links),
        };
        Meter::start(sock, Some(watch), ip, minute_ns)
    }

    /// [`Meter::shared`] with the meter `open` makes when none runs on `ip`.
    pub fn shared_with(
        ip: Ipv4Addr,
        open: impl FnOnce() -> io::Result<Meter>,
    ) -> io::Result<Arc<Meter>> {
        let mut meters = lock(&METERS);
        meters.retain(|(_, w)| w.strong_count() > 0);
        if let Some(m) = meters
            .iter()
            .find(|(a, _)| *a == ip)
            .and_then(|(_, w)| w.upgrade())
        {
            return Ok(m);
        }
        let m = Arc::new(open()?);
        meters.push((ip, Arc::downgrade(&m)));
        info!("mDNS meter: started on the Dante leg (passive, nothing sent)");
        Ok(m)
    }

    /// A meter counting what `sock` receives, in minutes of `minute_ns`,
    /// its mDNS group membership on `ip` kept by `watch` when there is one.
    pub fn start(
        sock: UdpSocket,
        watch: Option<MeterWatch>,
        ip: Ipv4Addr,
        minute_ns: u64,
    ) -> io::Result<Meter> {
        let last = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let rejoins = Arc::new(AtomicU64::new(0));
        let run = Run {
            sock,
            watch,
            ip,
            minute_ns,
            last: Arc::clone(&last),
            stop: Arc::clone(&stop),
            rejoins: Arc::clone(&rejoins),
        };
        let thread = std::thread::Builder::new()
            .name("dstack-mdns-meter".to_string())
            .spawn(move || run.run())?;
        Ok(Meter {
            last,
            stop,
            rejoins,
            thread: Some(thread),
        })
    }

    /// How often its socket joined the group again after an interface
    /// change.
    pub fn rejoins(&self) -> u64 {
        self.rejoins.load(Ordering::Relaxed)
    }

    /// The last closed minute (`None` before the first).
    pub fn last(&self) -> Option<Minute> {
        lock(&self.last).clone()
    }

    /// A meter with no thread whose last minute is `minute` (a status test).
    #[cfg(test)]
    pub(crate) fn holding(minute: Option<Minute>) -> Meter {
        Meter {
            last: Arc::new(Mutex::new(minute)),
            stop: Arc::new(AtomicBool::new(true)),
            rejoins: Arc::default(),
            thread: None,
        }
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ended = t.join();
        }
        debug!("mDNS meter: stopped");
    }
}

/// The meter's thread.
struct Run {
    sock: UdpSocket,
    watch: Option<MeterWatch>,
    ip: Ipv4Addr,
    minute_ns: u64,
    last: Arc<Mutex<Option<Minute>>>,
    stop: Arc<AtomicBool>,
    rejoins: Arc<AtomicU64>,
}

impl Run {
    fn run(mut self) {
        let origin = Instant::now();
        let now = || u64::try_from(origin.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let mut senders = Senders::new(now());
        let mut buf = vec![0u8; 9000];
        while !self.stop.load(Ordering::Relaxed) {
            if let Some(w) = self.watch.as_mut() {
                let (sock, ip) = (&self.sock, self.ip);
                if w.membership.keep(Instant::now(), &mut w.read, MDNS, || {
                    rejoin(sock, *MDNS.ip(), ip)
                }) {
                    self.rejoins.fetch_add(1, Ordering::Relaxed);
                }
            }
            match self.sock.recv_from(&mut buf) {
                Ok((_, SocketAddr::V4(from))) => senders.count(*from.ip()),
                Ok(_) => {}
                Err(e) => trace!(kind = ?e.kind(), "mDNS meter: nothing this wait"),
            }
            if let Some(minute) = senders.roll_after(now(), self.minute_ns) {
                report(&minute);
                *lock(&self.last) = Some(minute);
            }
        }
    }
}

#[cfg(test)]
mod tests;
