// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The network floor (dstack #321): the lowest dstack version still
//! allowed on the network, and the check every dstack process makes of the
//! dstack binaries it meets.
//!
//! A dtray built 3.11.2026 (about v0.6.20), from before the standing mDNS
//! browse was removed (#141), ran 12 days on two PCs and made the whole
//! network burst every 96 s (#319). Each application moves its own dstack
//! pin, but nothing compared the dstack binaries on the network against
//! anything. Now:
//!
//! - [`NETWORK_FLOOR`]: compiled in. A release that fixes a network-wide
//!   defect raises it (`.claude/rules/release.md`). It starts at 0.6.58,
//!   the first build with no standing browse in any binary (0.6.57 removed
//!   the devices' own browses, 0.6.58 the network listing's, which dtray
//!   runs).
//! - Peers: a dstack device states its version in its ARC record's TXT
//!   ([`crate::announce::VERSION_KEY`], since v0.6.84). The process's mDNS
//!   meter reads it from every answer it hears
//!   ([`crate::discover::senders::Meter`], [`arc_versions`]), and a
//!   [`crate::discover::Discovery`] checks each device it lists or sees
//!   change, and its whole list at each sweep ([`below_of`],
//!   [`crate::discover::Discovery::below_floor`]). A device with no such key is not ours and is
//!   never named.
//! - Local clients: dtray and dtray-cli send `status <name> <version>` to
//!   a device's local endpoint ([`crate::ctl::probe::status_as`]); the
//!   device lists its clients ([`Clients`]) and checks them alike.
//! - [`check`]: one WARN `a dstack device below the network floor` per
//!   binary per [`HOUR_NS`] in a process ([`Warned`], process-wide, so a
//!   process that sees one peer two ways warns once), with its kind
//!   (`peer` or `client`), name, address (a peer's) and version, and the
//!   floor. The device's status lists them under `network.below_floor`
//!   ([`crate::ctl::status`](fn@crate::ctl::status)); dcontroller relays
//!   either as an alarm.
//!
//! A binary below the floor that states no version (anything before
//! v0.6.84 as a peer, any client before this release) is not seen here:
//! the floor names binaries below a floor raised later.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::net::Ipv4Addr;
use std::sync::{LazyLock, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Instant;

use simple_dns::Packet;
use simple_dns::rdata::RData;
use tracing::{debug, warn};

use crate::discover::{Device, Service, record_of};

/// A dstack version, `major.minor.patch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

impl Version {
    pub const fn new(major: u16, minor: u16, patch: u16) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    /// `major.minor.patch` (each a decimal number), a `-` or `+` suffix
    /// after it ignored (`0.6.144-dev`); `None` for anything else.
    pub fn parse(text: &str) -> Option<Version> {
        let core = text.split(['-', '+']).next()?;
        let mut parts = core.split('.');
        let mut part = || -> Option<u16> {
            let p = parts.next()?;
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            p.parse().ok()
        };
        let v = Version::new(part()?, part()?, part()?);
        parts.next().is_none().then_some(v)
    }

    /// Whether it is below [`NETWORK_FLOOR`].
    pub fn below_floor(self) -> bool {
        self < NETWORK_FLOOR
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The lowest dstack version allowed on the network (see the module).
pub const NETWORK_FLOOR: Version = Version::new(0, 6, 58);

/// How long a binary below the floor stays listed after it was last seen,
/// and the least time between two warnings of one binary, ns.
pub const HOUR_NS: u64 = 3_600_000_000_000;
/// Binaries below the floor a [`Guard`] lists at most (a new one takes the
/// place of the one seen longest ago).
pub const MAX_BELOW: usize = 64;
/// Local clients a device lists at most (the same rule).
pub const MAX_CLIENTS: usize = 16;
/// Binaries [`Warned`] remembers at most (the same rule).
pub const MAX_WARNED: usize = 256;
/// The longest client name a status request may carry, bytes.
pub const MAX_CLIENT_NAME: usize = 32;

/// What a binary below the floor is to the process that saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// A dstack device on the network (its ARC record).
    Peer,
    /// A local client of the device's status endpoint (dtray).
    Client,
}

impl Kind {
    /// Its word in the status and the log.
    pub fn word(self) -> &'static str {
        match self {
            Kind::Peer => "peer",
            Kind::Client => "client",
        }
    }
}

/// One binary below the floor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Below {
    pub kind: Kind,
    /// A peer's Dante name, or a client's own name.
    pub name: String,
    /// A peer's address (none for a local client).
    pub address: Option<Ipv4Addr>,
    pub version: Version,
    /// When it was last seen, ns ([`now_ns`]'s clock).
    pub seen_ns: u64,
}

/// Its key: kind, name (case aside) and address.
type Key = (Kind, String, Option<Ipv4Addr>);

fn key_of(kind: Kind, name: &str, address: Option<Ipv4Addr>) -> Key {
    (kind, name.to_ascii_lowercase(), address)
}

/// Makes room for `key` in `map` (at most `max` entries): when it is full
/// and `key` is new, the entry seen longest ago (`seen`) is dropped.
fn evict<K: Ord + Clone, V>(
    map: &mut BTreeMap<K, V>,
    key: &K,
    max: usize,
    seen: impl Fn(&V) -> u64,
) {
    if map.len() < max || map.contains_key(key) {
        return;
    }
    if let Some(oldest) = map
        .iter()
        .min_by_key(|(_, v)| seen(v))
        .map(|(k, _)| k.clone())
    {
        map.remove(&oldest);
    }
}

/// The binaries below the floor one observer saw within [`HOUR_NS`]
/// (pure, fake times).
#[derive(Debug, Default)]
pub struct Guard {
    below: BTreeMap<Key, Below>,
}

impl Guard {
    /// `name` (at `address`) seen at `now_ns` with `version`: listed (or
    /// its time renewed) when it is below the floor, taken off when it is
    /// not (it was upgraded). The listed entry, when it is below.
    pub fn see(
        &mut self,
        kind: Kind,
        name: &str,
        address: Option<Ipv4Addr>,
        version: Version,
        now_ns: u64,
    ) -> Option<Below> {
        let key = key_of(kind, name, address);
        if !version.below_floor() {
            self.below.remove(&key);
            return None;
        }
        evict(&mut self.below, &key, MAX_BELOW, |b| b.seen_ns);
        let b = Below {
            kind,
            name: name.to_string(),
            address,
            version,
            seen_ns: now_ns,
        };
        self.below.insert(key, b.clone());
        Some(b)
    }

    /// Every binary below the floor seen within [`HOUR_NS`] before
    /// `now_ns`; older ones are dropped.
    pub fn list(&mut self, now_ns: u64) -> Vec<Below> {
        self.below
            .retain(|_, b| now_ns.saturating_sub(b.seen_ns) < HOUR_NS);
        self.below.values().cloned().collect()
    }
}

/// One local client of a device's status endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub name: String,
    pub version: Version,
    /// When it last asked, ns ([`now_ns`]'s clock).
    pub seen_ns: u64,
}

/// A device's local clients that asked within [`HOUR_NS`] (pure, fake
/// times), by name (case aside).
#[derive(Debug, Default)]
pub struct Clients {
    by_name: BTreeMap<String, Client>,
}

impl Clients {
    /// `name` asked at `now_ns` with `version`.
    pub fn see(&mut self, name: &str, version: Version, now_ns: u64) {
        let key = name.to_ascii_lowercase();
        evict(&mut self.by_name, &key, MAX_CLIENTS, |c| c.seen_ns);
        self.by_name.insert(
            key,
            Client {
                name: name.to_string(),
                version,
                seen_ns: now_ns,
            },
        );
    }

    /// Every client that asked within [`HOUR_NS`] before `now_ns`; older
    /// ones are dropped.
    pub fn list(&mut self, now_ns: u64) -> Vec<Client> {
        self.by_name
            .retain(|_, c| now_ns.saturating_sub(c.seen_ns) < HOUR_NS);
        self.by_name.values().cloned().collect()
    }
}

/// When each binary was last warned about (pure, fake times): at most once
/// per [`HOUR_NS`].
#[derive(Debug, Default)]
pub struct Warned {
    at: HashMap<Key, u64>,
}

impl Warned {
    /// Whether `b` is to be warned about at `now_ns` (and marks it so).
    pub fn due(&mut self, b: &Below, now_ns: u64) -> bool {
        let key = key_of(b.kind, &b.name, b.address);
        if let Some(&at) = self.at.get(&key)
            && now_ns.saturating_sub(at) < HOUR_NS
        {
            return false;
        }
        if self.at.len() >= MAX_WARNED && !self.at.contains_key(&key) {
            self.at.retain(|_, at| now_ns.saturating_sub(*at) < HOUR_NS);
            if self.at.len() >= MAX_WARNED {
                return false;
            }
        }
        self.at.insert(key, now_ns);
        true
    }
}

/// The process's clock for the floor, ns since its first read.
pub fn now_ns() -> u64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    let origin = ORIGIN.get_or_init(Instant::now);
    u64::try_from(origin.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The process-wide [`Warned`].
static WARNED: LazyLock<Mutex<Warned>> = LazyLock::new(Mutex::default);

/// Warns about `b` once per [`HOUR_NS`] in this process.
pub fn warn_below(b: &Below) {
    if !lock(&WARNED).due(b, now_ns()) {
        return;
    }
    warn!(
        kind = b.kind.word(),
        name = %b.name,
        address = %b.address.map_or_else(|| "local".to_string(), |a| a.to_string()),
        version = %b.version,
        floor = %NETWORK_FLOOR,
        "a dstack device below the network floor: upgrade it"
    );
}

/// `name` (at `address`) seen now stating the version `text`: kept in
/// `guard` and warned about ([`warn_below`]) when it is below the floor.
/// A text that is no version is ignored.
pub fn check(guard: &Mutex<Guard>, kind: Kind, name: &str, address: Option<Ipv4Addr>, text: &str) {
    let Some(version) = Version::parse(text) else {
        debug!(
            kind = kind.word(),
            "network floor: a version that does not read"
        );
        return;
    };
    let below = lock(guard).see(kind, name, address, version, now_ns());
    if let Some(b) = below {
        warn_below(&b);
    }
}

/// The devices of `devices` that state a dstack version below the floor
/// ([`crate::announce::VERSION_KEY`]), as peers at their ARC address, seen
/// at `now_ns`. A device without the key is not ours: never named.
pub fn below_of(devices: &[Device], now_ns: u64) -> Vec<Below> {
    devices
        .iter()
        .filter_map(|d| {
            let version = Version::parse(d.txt.get(crate::announce::VERSION_KEY)?)?;
            version.below_floor().then(|| Below {
                kind: Kind::Peer,
                name: d.name.clone(),
                address: Some(*d.arc.ip()),
                version,
                seen_ns: now_ns,
            })
        })
        .collect()
}

/// Warns ([`warn_below`]) about each device of `devices` below the floor.
pub fn warn_devices(devices: &[Device]) {
    for b in below_of(devices, now_ns()) {
        warn_below(&b);
    }
}

/// The dstack versions an mDNS datagram's `_netaudio-arc` TXT records
/// state: each device's name and the [`crate::announce::VERSION_KEY`]
/// value. Responses only: a query's known answers are the asker's, not the
/// device's (dstack #141). Empty for anything else.
pub fn arc_versions(bytes: &[u8]) -> Vec<(String, String)> {
    let Ok(packet) = Packet::parse(bytes) else {
        return Vec::new();
    };
    if !packet.has_flags(simple_dns::PacketFlag::RESPONSE) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for rr in packet.answers.iter().chain(&packet.additional_records) {
        let RData::TXT(txt) = &rr.rdata else {
            continue;
        };
        let Some((Service::Arc, name)) = record_of(&rr.name.to_string()) else {
            continue;
        };
        if let Some(Some(v)) = txt.attributes().remove(crate::announce::VERSION_KEY) {
            out.push((name, v));
        }
    }
    out
}

/// A status request's client: its name (1 to [`MAX_CLIENT_NAME`] bytes of
/// letters, digits, `.`, `_` and `-`) and version; `None` for anything
/// else.
pub fn client_of(name: &str, version: &str) -> Option<(String, Version)> {
    let fits = (1..=MAX_CLIENT_NAME).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    fits.then(|| Some((name.to_string(), Version::parse(version)?)))
        .flatten()
}

#[cfg(test)]
mod tests;
