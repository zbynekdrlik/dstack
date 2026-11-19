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
//!   minute` (only a loud minute), a WARN `an mDNS sender is over the
//!   cap` per source over the cap, and a WARN `an mDNS host refreshes
//!   Dante records every 96 s` per [`browsers::Browser`] (dstack #319),
//!   once an hour per host.
//! - [`browsers`]: the hosts that refresh Dante records on the 96 s cycle
//!   of a standing browse, from the queries the meter hears; Dante
//!   hardware (ARC records without `dstack_vers` in its answers) is never
//!   named.
//! - [`Meter`]: the process's passive count on one address (one per
//!   process and address, held by the address's
//!   [`crate::announce::mdns::Mdns`] and every device there; its own
//!   thread, nothing sent) and the last minute for the devices' status.
//!   Since dstack #442 it has no socket: it reads every datagram the
//!   daemon's socket receives ([`crate::announce::mdns::Listener`]), the
//!   group as joined on that address's interface only (the daemon's socket
//!   takes only its own memberships on Linux, `IP_MULTICAST_ALL` off, and
//!   Windows delivers by the socket's own membership: checked on a two-leg
//!   PC, 14.11.2026, 20 s, every source on the Dante subnet, none from the
//!   LAN's mDNS). The socket is bound to the wildcard address, so it may
//!   also take a unicast datagram to port 5353; mDNS answers go to the
//!   group.
//! - The network floor (dstack #321): the meter also reads the dstack
//!   version in every `_netaudio-arc` TXT it hears
//!   ([`crate::network_floor::arc_versions`]) and keeps the peers below the
//!   floor ([`Meter::below_floor`]); [`crate::network_floor::check`] warns.

use std::collections::HashMap;
use std::io;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tracing::{debug, info, trace, warn};

use crate::announce::mdns::Listener;
use crate::network_floor::{self, Below, Guard, Kind};

pub mod browsers;

use browsers::{Browser, Browsers};

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
    /// The hosts refreshing Dante records on the 96 s cycle, by address
    /// ([`browsers`]; set by the meter at the roll).
    pub browsers: Vec<Browser>,
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
            browsers: Vec::new(),
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
    for b in m.browsers.iter().filter(|b| b.warn) {
        warn!(
            source = %b.source,
            period_s = format_args!("{:.1}", b.period_ns as f64 / 1e9),
            "an mDNS host refreshes Dante records every 96 s: a program there \
             browses Dante, and every Dante host answers it at once (dstack #319)"
        );
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The process's mDNS meter on one address (see the module): it ends with
/// its last holder.
#[derive(Debug)]
pub struct Meter {
    last: Arc<Mutex<Option<Minute>>>,
    floor: Arc<Mutex<Guard>>,
    stop: Arc<AtomicBool>,
    heard: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

/// The meter's receive wait (it looks at the stop between two).
const WAIT: Duration = Duration::from_millis(250);

impl Meter {
    /// A meter counting what `heard` receives (the daemon's socket on
    /// `ip`, dstack #442), in minutes of `minute_ns`, the 96 s refreshes
    /// looked for by `browsers`.
    pub fn start(
        heard: Listener,
        ip: Ipv4Addr,
        minute_ns: u64,
        browsers: Browsers,
    ) -> io::Result<Meter> {
        let last = Arc::new(Mutex::new(None));
        let floor = Arc::new(Mutex::new(Guard::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let count = Arc::new(AtomicU64::new(0));
        let run = Run {
            heard,
            minute_ns,
            browsers,
            last: Arc::clone(&last),
            floor: Arc::clone(&floor),
            stop: Arc::clone(&stop),
            heard_count: Arc::clone(&count),
        };
        let thread = std::thread::Builder::new()
            .name("dstack-mdns-meter".to_string())
            .spawn(move || run.run())?;
        info!(%ip, "mDNS meter: started on the Dante leg (passive, nothing sent)");
        Ok(Meter {
            last,
            floor,
            stop,
            heard: count,
            thread: Some(thread),
        })
    }

    /// The datagrams it counted since its start.
    pub fn heard(&self) -> u64 {
        self.heard.load(Ordering::Relaxed)
    }

    /// The last closed minute (`None` before the first).
    pub fn last(&self) -> Option<Minute> {
        lock(&self.last).clone()
    }

    /// The dstack peers below the network floor it heard within the last
    /// hour (dstack #321, [`crate::network_floor::Guard::list`]).
    pub fn below_floor(&self) -> Vec<Below> {
        lock(&self.floor).list(network_floor::now_ns())
    }

    /// A meter with no thread whose last minute is `minute` (a status test).
    #[cfg(test)]
    pub(crate) fn holding(minute: Option<Minute>) -> Meter {
        Meter::holding_floor(minute, Guard::default())
    }

    /// [`Meter::holding`] with the peers below the floor of `floor`.
    #[cfg(test)]
    pub(crate) fn holding_floor(minute: Option<Minute>, floor: Guard) -> Meter {
        Meter {
            last: Arc::new(Mutex::new(minute)),
            floor: Arc::new(Mutex::new(floor)),
            stop: Arc::new(AtomicBool::new(true)),
            heard: Arc::default(),
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
    heard: Listener,
    minute_ns: u64,
    browsers: Browsers,
    last: Arc<Mutex<Option<Minute>>>,
    floor: Arc<Mutex<Guard>>,
    stop: Arc<AtomicBool>,
    heard_count: Arc<AtomicU64>,
}

impl Run {
    fn run(mut self) {
        let origin = Instant::now();
        let now = || u64::try_from(origin.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let mut senders = Senders::new(now());
        while !self.stop.load(Ordering::Relaxed) {
            match self.heard.recv_timeout(WAIT) {
                Ok(d) => {
                    self.heard_count.fetch_add(1, Ordering::Relaxed);
                    senders.count(*d.from.ip());
                    self.arc_records(&d.bytes, *d.from.ip(), now());
                    self.browsers
                        .heard(*d.from.ip(), d.from.port(), &d.bytes, now());
                }
                Err(RecvTimeoutError::Timeout) => trace!("mDNS meter: nothing this wait"),
                // Its listener's channel lives as long as the thread: never.
                Err(RecvTimeoutError::Disconnected) => return,
            }
            if let Some(mut minute) = senders.roll_after(now(), self.minute_ns) {
                minute.browsers = self.browsers.take(now());
                report(&minute);
                *lock(&self.last) = Some(minute);
            }
        }
    }

    /// The ARC records datagram `bytes` from `from` carries at `now_ns`:
    /// each dstack version checked against the network floor (dstack
    /// #321), and each record told to the browser watch, which never names
    /// Dante hardware (a record without `dstack_vers`, dstack #319).
    fn arc_records(&mut self, bytes: &[u8], from: Ipv4Addr, now_ns: u64) {
        for (name, version) in network_floor::arc_records(bytes) {
            self.browsers.arc_from(from, version.is_some(), now_ns);
            if let Some(version) = version {
                network_floor::check(&self.floor, Kind::Peer, &name, Some(from), &version);
            }
        }
    }
}

#[cfg(test)]
mod tests;
