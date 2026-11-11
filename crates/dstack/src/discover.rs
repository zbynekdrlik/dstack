// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! Device discovery for a controller (dstack #16, step 1): every Dante
//! device on one interface, with live changes, not tied to our own device.
//!
//! It keeps no standing mDNS browse (dstack #141). A browse's daemon
//! refreshes every record it learned at 80 % of its TTL from port 5353, so
//! each device answers by multicast, and every host that browses refreshed
//! the same records in the same second: a burst of 100 to 300 datagrams
//! flooded to every port every 96 s, which an UltimoX2 receiver read as
//! late packets. Instead:
//!
//! - **A sweep** ([`sweep_query`]): one PTR query of `_netaudio-arc`,
//!   `_netaudio-dbc` and `_netaudio-cmc` every [`SWEEP`] plus a jitter
//!   ([`sweep_after`]), sent from a port of our own (legacy unicast: the
//!   answers come back to us alone).
//! - **Listening**: the mDNS group's traffic on the interface, as a
//!   passive member (nothing sent from it): announcements, goodbyes and
//!   others' answers. A device that answers no PTR query (a wireless mic
//!   receiver, dstack #1) is still heard when it announces.
//! - **Each instance's record** ([`crate::subscribe::instance`]): SRV,
//!   TXT and the target's A by direct legacy unicast queries, asked again
//!   before the answer is [`crate::subscribe::instance::MAX_AGE_NS`] old;
//!   every datagram the sweep or the group brings refreshes it too.
//!
//! [`Devices`] (pure) lists a device once its `_netaudio-arc` record is
//! read: its name (as it announces it), the record's lowest IPv4 address
//! and port, the record's TXT strings (`mf`, `model`, `router_info`, ...),
//! the `_netaudio-dbc` record's address when there is one, and the device
//! id its `_netaudio-cmc` record's `id` gives (16 hex digits: the id its
//! heartbeat carries, [`crate::latency`]; several devices may share an
//! address). A device leaves the list at its goodbye, or when its record
//! has not been answered for [`crate::subscribe::instance::MAX_AGE_NS`].
//!
//! [`Discovery`]: the threads on the interface with the address `ip` only,
//! [`Discovery::devices`] for the list now and a channel of every
//! [`Change`] for a live list. [`Discovery::stop`] ends them.
//!
//! It only listens and asks mDNS; it sends nothing to any device. Logs
//! carry counts and events, never a name or an address.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use simple_dns::rdata::RData;
use simple_dns::{CLASS, Name, Packet, QCLASS, QTYPE, Question, TYPE};
use tracing::{debug, info, trace, warn};

use crate::announce::{ARC_SERVICE, CMC_SERVICE};
use crate::subscribe::directory::DBC_SERVICE;
use crate::subscribe::instance::{Instances, MDNS, Srv};

/// Devices kept at most; a record of a further device is ignored.
pub const MAX_DEVICES: usize = 256;
/// Instances known at most: the three records of each of [`MAX_DEVICES`].
pub const MAX_NAMES: usize = 3 * MAX_DEVICES;
// The three records of each device: a name per service.
const _: () = assert!(MAX_NAMES == SERVICES.len() * MAX_DEVICES);
/// TXT strings kept of a record at most.
pub const MAX_TXT: usize = 32;
/// How long [`Discovery::stop`] waits for the threads.
pub const STOP_WAIT: Duration = Duration::from_secs(3);
/// The least time between two sweeps.
pub const SWEEP: Duration = Duration::from_secs(30);
/// The most a sweep waits past [`SWEEP`] (spread so that several
/// listings never sweep in step).
pub const SWEEP_JITTER: Duration = Duration::from_secs(30);
/// The thread's round: the group and the sweep's answers read, the list
/// brought up to date.
pub const ROUND: Duration = Duration::from_millis(250);
/// The longest one socket's read waits within a round (the sweep's and the
/// group's sockets take turns).
pub const READ_WAIT: Duration = Duration::from_millis(50);
/// The three services a device is listed by, in their [`Service`] order.
pub const SERVICES: [&str; 3] = [ARC_SERVICE, DBC_SERVICE, CMC_SERVICE];

/// A device as its records announce it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// The device's name, as its `_netaudio-arc` record has it.
    pub name: String,
    /// Its ARC (UDP 4440 on a Dante device): the record's lowest IPv4
    /// address and its port.
    pub arc: SocketAddrV4,
    /// Its flow control (`_netaudio-dbc`), when it publishes it.
    pub dbc: Option<SocketAddrV4>,
    /// Its CMC (UDP 8800 on a Dante device, 8800+k for a device beside
    /// others on one PC): its `_netaudio-cmc` record's address and port.
    /// A meter subscription ([`crate::meter`]) goes here (dstack #180).
    pub cmc: Option<SocketAddrV4>,
    /// Its device id, from its `_netaudio-cmc` record's `id`: the id in its
    /// heartbeat's header ([`crate::latency::Latency::device_id`]).
    pub device_id: Option<[u8; 8]>,
    /// The `_netaudio-arc` record's TXT strings, key to value (`mf`,
    /// `model`, `router_info`, `arcp_vers`, ...).
    pub txt: BTreeMap<String, String>,
}

impl Device {
    /// Whether its 0x2600 flow list states its real TX flow slots
    /// ([`crate::arc::flows::Flows::slots`], dstack #190): every device
    /// except one of ours from before v0.6.84, which wrote the flow count
    /// there (its real limit is [`crate::arc::MAX_FLOWS`]). Ours carry
    /// inferno's model id; since v0.6.84 also [`crate::announce::VERSION_KEY`].
    pub fn states_flow_slots(&self) -> bool {
        self.txt.get("model").map(String::as_str) != Some(crate::announce::MODEL_ID)
            || self.txt.contains_key(crate::announce::VERSION_KEY)
    }

    /// The local status port its ARC TXT names
    /// ([`crate::announce::STATUS_PORT_KEY`], `dstack_ctl=<port>`): only our
    /// devices carry it. `None` without the key, or with a value that is no
    /// port (0 included). Its readers: dtray (dstack #104) and a device's
    /// traffic watch ([`crate::traffic`], dstack #203).
    pub fn status_port(&self) -> Option<u16> {
        self.txt
            .get(crate::announce::STATUS_PORT_KEY)
            .and_then(|v| v.parse::<u16>().ok())
            .filter(|&p| p != 0)
    }
}

/// What changed for the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A device is listed now.
    Added(Device),
    /// A listed device's address, port, TXT or flow control changed.
    Changed(Device),
    /// A listed device is gone: its goodbye, or no answer for its record
    /// (its name).
    Removed(String),
}

/// One device's records.
#[derive(Debug, Clone, Default)]
struct Entry {
    name: String,
    arc: Option<SocketAddrV4>,
    txt: BTreeMap<String, String>,
    dbc: Option<SocketAddrV4>,
    /// The `_netaudio-cmc` record's address, and its id when it reads.
    cmc: Option<(SocketAddrV4, Option<[u8; 8]>)>,
}

impl Entry {
    fn device(&self) -> Option<Device> {
        Some(Device {
            name: self.name.clone(),
            arc: self.arc?,
            dbc: self.dbc,
            cmc: self.cmc.map(|(at, _)| at),
            device_id: self.cmc.and_then(|(_, id)| id),
            txt: self.txt.clone(),
        })
    }
}

/// Which of the three services a record is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Service {
    Arc,
    Dbc,
    Cmc,
}

impl Service {
    /// Its type, `_netaudio-…._udp.local.`.
    pub fn ty(self) -> &'static str {
        match self {
            Service::Arc => SERVICES[0],
            Service::Dbc => SERVICES[1],
            Service::Cmc => SERVICES[2],
        }
    }

    /// The service of type `ty` (case aside, a dot at the end or not).
    pub fn of(ty: &str) -> Option<Service> {
        let ty = ty.trim_end_matches('.');
        [Service::Arc, Service::Dbc, Service::Cmc]
            .into_iter()
            .find(|s| s.ty().trim_end_matches('.').eq_ignore_ascii_case(ty))
    }
}

/// A device id as a CMC record's `id` writes it: 16 hex digits.
pub fn parse_device_id(s: &str) -> Option<[u8; 8]> {
    if s.len() != 16 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 8];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// The service and the device's name (its case kept) of an instance's
/// full name `fullname` (`<device>.<service>`, a dot at the end or not);
/// `None` for another service or no name.
pub fn record_of(fullname: &str) -> Option<(Service, String)> {
    let full = fullname.trim_end_matches('.');
    [Service::Arc, Service::Dbc, Service::Cmc]
        .into_iter()
        .find_map(|service| {
            let ty = service.ty().trim_end_matches('.');
            let cut = full.len().checked_sub(ty.len() + 1)?;
            let (name, rest) = (full.get(..cut)?, full.get(cut..)?);
            let ours =
                !name.is_empty() && rest.starts_with('.') && rest[1..].eq_ignore_ascii_case(ty);
            ours.then(|| (service, name.to_string()))
        })
}

/// What one datagram says about the three services' instances.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Heard {
    /// The instances its PTR answers name.
    pub named: Vec<(Service, String)>,
    /// The instances it withdraws (a PTR or SRV of TTL 0: a goodbye).
    pub gone: Vec<(Service, String)>,
}

/// What datagram `bytes` says about the three services' instances (an
/// empty [`Heard`] for a query or what does not read).
pub fn heard(bytes: &[u8]) -> Heard {
    let mut out = Heard::default();
    let Ok(packet) = Packet::parse(bytes) else {
        return out;
    };
    if !packet.has_flags(simple_dns::PacketFlag::RESPONSE) {
        return out;
    }
    for rr in packet.answers.iter().chain(&packet.additional_records) {
        let instance = match &rr.rdata {
            RData::PTR(ptr) if Service::of(&rr.name.to_string()).is_some() => ptr.0.to_string(),
            RData::SRV(_) => rr.name.to_string(),
            _ => continue,
        };
        let Some(found) = record_of(&instance) else {
            continue;
        };
        if rr.ttl == 0 {
            out.gone.push(found);
        } else if matches!(rr.rdata, RData::PTR(_)) {
            out.named.push(found);
        }
    }
    out
}

/// The sweep's query: a PTR question for each of [`SERVICES`].
pub fn sweep_query(id: u16) -> Vec<u8> {
    let mut p = Packet::new_query(id);
    for ty in SERVICES {
        p.questions.push(Question::new(
            Name::new_unchecked(ty.trim_end_matches('.')),
            QTYPE::TYPE(TYPE::PTR),
            QCLASS::CLASS(CLASS::IN),
            false,
        ));
    }
    // Three short names: a query always builds.
    p.build_bytes_vec().unwrap_or_default()
}

/// The wait before the next sweep, from any `seed` (a clock's ns): at
/// least [`SWEEP`], at most [`SWEEP`] + [`SWEEP_JITTER`].
pub fn sweep_after(seed: u64) -> Duration {
    let jitter = u64::try_from(SWEEP_JITTER.as_millis()).unwrap_or(u64::MAX);
    SWEEP + Duration::from_millis(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) % (jitter + 1))
}

/// The devices on the network (see the module).
#[derive(Debug, Default)]
pub struct Devices {
    /// By name, case aside.
    entries: BTreeMap<String, Entry>,
}

impl Devices {
    /// Keeps `name`'s record of `service` as read now (`None`: gone or not
    /// answered); what changed for the list, if anything.
    pub fn set(&mut self, service: Service, name: &str, record: Option<&Srv>) -> Option<Change> {
        let key = name.to_ascii_lowercase();
        let at = record.and_then(|r| {
            let ip = r.ips.iter().min()?;
            Some(SocketAddrV4::new(*ip, r.port))
        });
        if at.is_some() && self.entries.len() >= MAX_DEVICES && !self.entries.contains_key(&key) {
            return None;
        }
        if at.is_none() && !self.entries.contains_key(&key) {
            return None;
        }
        let entry = self.entries.entry(key.clone()).or_default();
        let before = entry.device();
        let txt = || {
            record.map_or_else(BTreeMap::new, |r| {
                r.txt
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .take(MAX_TXT)
                    .collect()
            })
        };
        match service {
            Service::Arc => {
                if at.is_some() {
                    name.clone_into(&mut entry.name);
                }
                entry.arc = at;
                entry.txt = txt();
            }
            Service::Dbc => entry.dbc = at,
            Service::Cmc => {
                entry.cmc = at.map(|at| (at, txt().get("id").and_then(|id| parse_device_id(id))));
            }
        }
        let after = entry.device();
        if entry.arc.is_none() && entry.dbc.is_none() && entry.cmc.is_none() {
            self.entries.remove(&key);
        }
        match (before, after) {
            (None, Some(d)) => Some(Change::Added(d)),
            (Some(b), Some(a)) if a != b => Some(Change::Changed(a)),
            (Some(b), None) => Some(Change::Removed(b.name)),
            _ => None,
        }
    }

    /// Every listed device, by name (case aside).
    pub fn list(&self) -> Vec<Device> {
        self.entries.values().filter_map(Entry::device).collect()
    }
}

/// How long an instance stays known with neither a PTR naming it nor a
/// fresh record: a device that is gone stops being asked for.
pub const FORGET_NS: u64 = 300_000_000_000;

/// The instances known, by service and name (case aside, the announced
/// case kept), at most three for each of [`MAX_DEVICES`], each with the
/// last time (ns) a PTR named it or its record was fresh, and the last
/// time a PTR named it.
#[derive(Debug, Default)]
pub struct Names {
    known: BTreeMap<(Service, String), (String, u64, u64)>,
}

impl Names {
    /// Takes what a datagram said at `now`; the number of names new to it.
    /// A goodbye forgets the name (the caller drops its record).
    pub fn take(&mut self, heard: &Heard, now: u64) -> usize {
        let mut new = 0;
        for (service, name) in &heard.gone {
            self.known.remove(&(*service, name.to_ascii_lowercase()));
        }
        for (service, name) in &heard.named {
            let key = (*service, name.to_ascii_lowercase());
            if self.known.len() >= MAX_NAMES && !self.known.contains_key(&key) {
                continue;
            }
            if self.known.insert(key, (name.clone(), now, now)).is_none() {
                new += 1;
            }
        }
        new
    }

    /// `name`'s record of `service` was fresh at `now`: it stays known.
    pub fn touch(&mut self, service: Service, name: &str, now: u64) {
        if let Some(seen) = self.known.get_mut(&(service, name.to_ascii_lowercase())) {
            seen.1 = now;
        }
    }

    /// Whether a PTR named `name`'s instance of `service` within
    /// [`crate::subscribe::instance::MAX_AGE_NS`] before `now`: the device
    /// answers the sweeps, so a record of it that went unanswered for a
    /// while does not take it off the list (dstack #141).
    pub fn named_lately(&self, service: Service, name: &str, now: u64) -> bool {
        self.known
            .get(&(service, name.to_ascii_lowercase()))
            .is_some_and(|(_, _, named)| {
                now.saturating_sub(*named) < crate::subscribe::instance::MAX_AGE_NS
            })
    }

    /// Every instance known at `now`: its service and name. One with
    /// nothing heard of it for [`FORGET_NS`] is forgotten.
    pub fn all(&mut self, now: u64) -> Vec<(Service, String)> {
        self.known
            .retain(|_, (_, seen, _)| now.saturating_sub(*seen) < FORGET_NS);
        self.known
            .iter()
            .map(|((service, _), (name, _, _))| (*service, name.clone()))
            .collect()
    }
}

/// Why discovery did not start or stop cleanly.
#[derive(Debug, thiserror::Error)]
pub enum DiscoverError {
    #[error("a discovery socket or thread: {0}")]
    Io(#[from] io::Error),
    #[error("discovery did not stop within {} s", STOP_WAIT.as_secs())]
    StopTimeout,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Discovery running on one interface (see the module).
pub struct Discovery {
    devices: Arc<Mutex<Devices>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// The records' queries; its thread ends when the last holder drops.
    _records: Arc<Instances>,
}

/// The mDNS group's traffic on the interface with the address `ip`, as a
/// passive member: bound to the mDNS port with address reuse beside the
/// host's own responders (Linux: on the group itself, so no unicast
/// datagram to the port comes in; elsewhere on any address).
fn group_listener(ip: Ipv4Addr) -> io::Result<UdpSocket> {
    let sock = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    sock.set_reuse_address(true)?;
    #[cfg(all(unix, not(target_os = "solaris"), not(target_os = "illumos")))]
    sock.set_reuse_port(true)?;
    let bind = if cfg!(target_os = "linux") {
        *MDNS.ip()
    } else {
        Ipv4Addr::UNSPECIFIED
    };
    sock.bind(&SocketAddrV4::new(bind, MDNS.port()).into())?;
    sock.join_multicast_v4(MDNS.ip(), &ip)?;
    let sock: UdpSocket = sock.into();
    sock.set_read_timeout(Some(READ_WAIT))?;
    Ok(sock)
}

impl Discovery {
    /// Starts discovery on the interface with the address `ip`: the devices
    /// listed so far through [`Discovery::devices`], every change on the
    /// returned channel. The group's traffic is listened to when the mDNS
    /// port can be shared (a warning otherwise: the sweeps still list).
    pub fn start(ip: Ipv4Addr) -> Result<(Self, Receiver<Change>), DiscoverError> {
        let listener = match group_listener(ip) {
            Ok(sock) => Some(sock),
            Err(e) => {
                warn!(%e, "discovery: the mDNS group is not listened to; sweeps only");
                None
            }
        };
        Self::start_to(ip, MDNS, listener)
    }

    /// [`Discovery::start`] with the sweeps and the records' queries sent
    /// to `mdns` (mDNS's group, or a test's responder) and the group's
    /// traffic read from `listener` when there is one.
    pub fn start_to(
        ip: Ipv4Addr,
        mdns: SocketAddrV4,
        listener: Option<UdpSocket>,
    ) -> Result<(Self, Receiver<Change>), DiscoverError> {
        let sweeper = UdpSocket::bind(SocketAddrV4::new(ip, 0))?;
        sweeper.set_read_timeout(Some(READ_WAIT))?;
        if mdns.ip().is_multicast() {
            socket2::SockRef::from(&sweeper).set_multicast_if_v4(&ip)?;
        }
        let mut records = Instances::with_max(MAX_NAMES);
        records.start_to(ip, mdns)?;
        let records = Arc::new(records);
        let devices = Arc::new(Mutex::new(Devices::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let run = Run {
            sweeper,
            listener,
            mdns,
            records: Arc::clone(&records),
            devices: Arc::clone(&devices),
            stop: Arc::clone(&stop),
            tx,
        };
        let thread = std::thread::Builder::new()
            .name("dstack-discover".to_string())
            .spawn(move || run.run())?;
        info!("discovery: started on one interface (sweeps, no standing browse)");
        Ok((
            Discovery {
                devices,
                stop,
                thread: Some(thread),
                _records: records,
            },
            rx,
        ))
    }

    /// The devices listed now.
    pub fn devices(&self) -> Vec<Device> {
        lock(&self.devices).list()
    }

    /// Ends the thread, waiting at most [`STOP_WAIT`].
    pub fn stop(mut self) -> Result<(), DiscoverError> {
        self.stop.store(true, Ordering::Relaxed);
        let deadline = Instant::now() + STOP_WAIT;
        if let Some(thread) = self.thread.take() {
            while !thread.is_finished() {
                if Instant::now() >= deadline {
                    warn!("discovery: its thread did not end in time");
                    return Err(DiscoverError::StopTimeout);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ended = thread.join();
        }
        info!("discovery: stopped");
        Ok(())
    }
}

impl Drop for Discovery {
    /// The thread ends at its next round.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// ns since `origin`.
fn now_ns(origin: Instant) -> u64 {
    u64::try_from(origin.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Reads `sock` while it has datagrams and `until` has not passed, handing
/// each to `take`. A read that waits out the socket's timeout (nothing
/// arrived within it) or fails ends it. A busy group is read as fast
/// as it comes (dstack #206: one datagram a pass read about 20 a second
/// against dev1's 30, and the kernel dropped the rest, goodbyes among
/// them).
fn drain(sock: &UdpSocket, buf: &mut [u8], until: Instant, mut take: impl FnMut(&[u8])) {
    while !until.saturating_duration_since(Instant::now()).is_zero() {
        let Ok((n, _)) = sock.recv_from(buf) else {
            break;
        };
        take(&buf[..n]);
    }
}

/// The thread's state.
struct Run {
    sweeper: UdpSocket,
    listener: Option<UdpSocket>,
    mdns: SocketAddrV4,
    records: Arc<Instances>,
    devices: Arc<Mutex<Devices>>,
    stop: Arc<AtomicBool>,
    tx: Sender<Change>,
}

impl Run {
    /// Sweeps when due, reads the sweep's answers and the group, and brings
    /// the list up to date every [`ROUND`], until the stop.
    fn run(self) {
        let origin = Instant::now();
        let mut names = Names::default();
        let mut next_sweep = Duration::ZERO;
        let mut id: u16 = 0;
        let mut buf = vec![0u8; 9000];
        while !self.stop.load(Ordering::Relaxed) {
            if origin.elapsed() >= next_sweep {
                id = id.wrapping_add(1);
                if let Err(e) = self.sweeper.send_to(&sweep_query(id), self.mdns) {
                    debug!(%e, "discovery: a sweep not sent");
                }
                let seed = u64::try_from(origin.elapsed().as_nanos()).unwrap_or(0);
                next_sweep = origin.elapsed() + sweep_after(seed);
            }
            let round = Instant::now() + ROUND;
            while !round.saturating_duration_since(Instant::now()).is_zero() {
                for sock in std::iter::once(&self.sweeper).chain(self.listener.as_ref()) {
                    drain(sock, &mut buf, round, |bytes| {
                        self.take(bytes, &mut names, now_ns(origin));
                    });
                }
            }
            self.update(&mut names, now_ns(origin));
        }
    }

    /// One datagram at `now`: its goodbyes, its names (each wanted at once,
    /// so the records it carries are kept), its records.
    fn take(&self, bytes: &[u8], names: &mut Names, now: u64) {
        let heard = heard(bytes);
        for (service, name) in &heard.gone {
            self.records.forget(name, service.ty());
            let change = lock(&self.devices).set(*service, name, None);
            self.send(change);
        }
        let new = names.take(&heard, now);
        trace!(new, named = heard.named.len(), "discovery: instances named");
        for (service, name) in &heard.named {
            self.records.record(name, service.ty());
        }
        self.records.answer(bytes);
    }

    /// Every known instance's record as the queries know it at `now`.
    fn update(&self, names: &mut Names, now: u64) {
        let mut changes = Vec::new();
        let read: BTreeSet<(Service, String)> = names.all(now).into_iter().collect();
        for (service, name) in read {
            let record = self.records.record(&name, service.ty());
            if record.is_some() {
                names.touch(service, &name, now);
            } else if names.named_lately(service, &name, now) {
                // It answers the sweeps: its record's queries will catch up.
                continue;
            }
            changes.push(lock(&self.devices).set(service, &name, record.as_ref()));
        }
        for change in changes {
            self.send(change);
        }
    }

    /// A change on the channel (none left listening: the list still
    /// follows).
    fn send(&self, change: Option<Change>) {
        let Some(change) = change else {
            return;
        };
        debug!(
            kind = match &change {
                Change::Added(_) => "added",
                Change::Changed(_) => "changed",
                Change::Removed(_) => "removed",
            },
            "discovery: a device"
        );
        let _listened = self.tx.send(change);
    }
}

#[cfg(test)]
mod tests;
