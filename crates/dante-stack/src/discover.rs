// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! Device discovery for a controller (dstack #16, step 1): every Dante
//! device on one interface, with live changes, not tied to our own device.
//!
//! - [`Devices`] (pure): the devices kept from the browse events of
//!   `_netaudio-arc` and `_netaudio-dbc` (at most [`MAX_DEVICES`]). A device
//!   is listed once its `_netaudio-arc` record is resolved: its name (as it
//!   announces it), the record's lowest IPv4 address and port, the TXT
//!   strings of that record (`mf`, `model`, `router_info`, ...), and the
//!   `_netaudio-dbc` record's address when there is one, and the device id
//!   its `_netaudio-cmc` record's `id` gives (16 hex digits: the id its
//!   heartbeat carries, [`crate::latency`]; several devices may share an
//!   address). [`Devices::apply`] says what an event changed ([`Change`]).
//! - [`Discovery`]: an mDNS daemon of its own on the interface with the
//!   address `ip` only (as the host sets its own up), the three browses, one
//!   thread each keeping their events, [`Discovery::devices`] for the list
//!   now and a channel of every [`Change`] for a live list.
//!   [`Discovery::stop`] shuts the daemon down and waits for the threads.
//!
//! It only listens: it sends mDNS queries (mdns-sd's) and nothing to any
//! device. Logs carry counts and events, never a name or an address.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mdns_sd::{IfKind, ServiceDaemon, ServiceEvent};
use tracing::{debug, info, warn};

use crate::announce::{ARC_SERVICE, CMC_SERVICE};
use crate::subscribe::directory::DBC_SERVICE;
use crate::subscribe::resolve::{Browse, BrowseError, lowest_ipv4};

/// Devices kept at most; a record of a further device is ignored.
pub const MAX_DEVICES: usize = 256;
/// TXT strings kept of a record at most.
pub const MAX_TXT: usize = 32;
/// How long [`Discovery::stop`] waits for the mDNS daemon and the threads.
pub const STOP_WAIT: Duration = Duration::from_secs(3);

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
    /// Its device id, from its `_netaudio-cmc` record's `id`: the id in its
    /// heartbeat's header ([`crate::latency::Latency::device_id`]).
    pub device_id: Option<[u8; 8]>,
    /// The `_netaudio-arc` record's TXT strings, key to value (`mf`,
    /// `model`, `router_info`, `arcp_vers`, ...).
    pub txt: BTreeMap<String, String>,
}

/// What a browse event changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A device is listed now.
    Added(Device),
    /// A listed device's address, port, TXT or flow control changed.
    Changed(Device),
    /// A listed device withdrew its `_netaudio-arc` record (its name).
    Removed(String),
}

/// One device's records.
#[derive(Debug, Clone, Default)]
struct Entry {
    name: String,
    arc: Option<SocketAddrV4>,
    txt: BTreeMap<String, String>,
    dbc: Option<SocketAddrV4>,
    /// The `_netaudio-cmc` record is resolved, and its id when it reads.
    cmc: Option<Option<[u8; 8]>>,
}

impl Entry {
    fn device(&self) -> Option<Device> {
        Some(Device {
            name: self.name.clone(),
            arc: self.arc?,
            dbc: self.dbc,
            device_id: self.cmc.flatten(),
            txt: self.txt.clone(),
        })
    }
}

/// Which of the three services a record is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Service {
    Arc,
    Dbc,
    Cmc,
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

/// The service of type `ty`, and the device's name in `fullname` (its case
/// kept); `None` for another service or no name.
fn record_of(ty: &str, fullname: &str) -> Option<(Service, String)> {
    let service = if ty.eq_ignore_ascii_case(ARC_SERVICE) {
        Service::Arc
    } else if ty.eq_ignore_ascii_case(DBC_SERVICE) {
        Service::Dbc
    } else if ty.eq_ignore_ascii_case(CMC_SERVICE) {
        Service::Cmc
    } else {
        return None;
    };
    let cut = fullname.len().checked_sub(ty.len() + 1)?;
    let (name, rest) = (fullname.get(..cut)?, fullname.get(cut..)?);
    if name.is_empty() || !rest.starts_with('.') || !rest[1..].eq_ignore_ascii_case(ty) {
        return None;
    }
    Some((service, name.to_string()))
}

/// The devices on the network (see the module).
#[derive(Debug, Default)]
pub struct Devices {
    /// By name, case aside.
    entries: BTreeMap<String, Entry>,
}

impl Devices {
    /// Keeps what a browse event says; what changed for the list, if
    /// anything.
    pub fn apply(&mut self, ev: &ServiceEvent) -> Option<Change> {
        match ev {
            ServiceEvent::ServiceResolved(r) => {
                let (service, name) = record_of(&r.ty_domain, r.get_fullname())?;
                let at = SocketAddrV4::new(lowest_ipv4(r)?, r.get_port());
                let key = name.to_ascii_lowercase();
                if self.entries.len() >= MAX_DEVICES && !self.entries.contains_key(&key) {
                    return None;
                }
                let entry = self.entries.entry(key).or_default();
                let before = entry.device();
                match service {
                    Service::Arc => {
                        entry.name = name;
                        entry.arc = Some(at);
                        entry.txt = r
                            .get_properties()
                            .iter()
                            .take(MAX_TXT)
                            .map(|p| (p.key().to_string(), p.val_str().to_string()))
                            .collect();
                    }
                    Service::Dbc => entry.dbc = Some(at),
                    Service::Cmc => {
                        entry.cmc = Some(
                            r.get_properties()
                                .get_property_val_str("id")
                                .and_then(parse_device_id),
                        );
                    }
                }
                let after = entry.device();
                match (before, after) {
                    (None, Some(d)) => Some(Change::Added(d)),
                    (Some(b), Some(a)) if a != b => Some(Change::Changed(a)),
                    _ => None,
                }
            }
            ServiceEvent::ServiceRemoved(ty, fullname) => {
                let (service, name) = record_of(ty, fullname)?;
                let key = name.to_ascii_lowercase();
                let entry = self.entries.get_mut(&key)?;
                let before = entry.device();
                match service {
                    Service::Arc => entry.arc = None,
                    Service::Dbc => entry.dbc = None,
                    Service::Cmc => entry.cmc = None,
                }
                let after = entry.device();
                if entry.arc.is_none() && entry.dbc.is_none() && entry.cmc.is_none() {
                    self.entries.remove(&key);
                }
                match (before, after) {
                    (Some(b), None) => Some(Change::Removed(b.name)),
                    (Some(b), Some(a)) if a != b => Some(Change::Changed(a)),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Every listed device, by name (case aside).
    pub fn list(&self) -> Vec<Device> {
        self.entries.values().filter_map(Entry::device).collect()
    }
}

/// Why discovery did not start or stop cleanly.
#[derive(Debug, thiserror::Error)]
pub enum DiscoverError {
    #[error("the mDNS daemon: {0}")]
    Mdns(#[from] mdns_sd::Error),
    #[error(transparent)]
    Browse(#[from] BrowseError),
    #[error("a discovery thread: {0}")]
    Thread(#[from] std::io::Error),
    #[error("discovery did not stop within {} s", STOP_WAIT.as_secs())]
    StopTimeout,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Discovery running on one interface (see the module).
pub struct Discovery {
    devices: Arc<Mutex<Devices>>,
    daemon: Option<ServiceDaemon>,
    threads: Vec<JoinHandle<()>>,
}

impl Discovery {
    /// Starts discovery on the interface with the address `ip`: the devices
    /// listed so far through [`Discovery::devices`], every change on the
    /// returned channel.
    pub fn start(ip: Ipv4Addr) -> Result<(Self, Receiver<Change>), DiscoverError> {
        let daemon = ServiceDaemon::new()?;
        let selected = daemon
            .disable_interface(IfKind::All)
            .and_then(|()| daemon.enable_interface(IfKind::Addr(ip.into())));
        let started = selected
            .map_err(DiscoverError::from)
            .and_then(|()| Self::start_with(&daemon));
        match started {
            Ok((mut discovery, changes)) => {
                info!("discovery: started on one interface");
                discovery.daemon = Some(daemon);
                Ok((discovery, changes))
            }
            Err(e) => {
                let _status = daemon.shutdown();
                Err(e)
            }
        }
    }

    /// The browses on `daemon`, without a daemon of its own (the events end
    /// when `daemon`'s do).
    pub fn start_with<B: Browse>(daemon: &B) -> Result<(Self, Receiver<Change>), DiscoverError> {
        let devices = Arc::new(Mutex::new(Devices::default()));
        let (tx, rx) = mpsc::channel();
        let browses = [
            (
                "dante-discover-arc",
                daemon.browse(ARC_SERVICE).map_err(BrowseError::from)?,
            ),
            (
                "dante-discover-dbc",
                daemon.browse(DBC_SERVICE).map_err(BrowseError::from)?,
            ),
            (
                "dante-discover-cmc",
                daemon.browse(CMC_SERVICE).map_err(BrowseError::from)?,
            ),
        ];
        let mut threads = Vec::new();
        for (name, events) in browses {
            let devices = Arc::clone(&devices);
            let tx = tx.clone();
            threads.push(
                std::thread::Builder::new()
                    .name(name.to_string())
                    .spawn(move || keep(events, &devices, &tx))?,
            );
        }
        Ok((
            Discovery {
                devices,
                daemon: None,
                threads,
            },
            rx,
        ))
    }

    /// A handle of the mDNS daemon (a test watches its shutdown).
    #[cfg(test)]
    fn daemon(&self) -> Option<ServiceDaemon> {
        self.daemon.clone()
    }

    /// The devices listed now.
    pub fn devices(&self) -> Vec<Device> {
        lock(&self.devices).list()
    }

    /// Shuts the mDNS daemon down and waits at most [`STOP_WAIT`] for the
    /// browse threads to end.
    pub fn stop(mut self) -> Result<(), DiscoverError> {
        if let Some(daemon) = self.daemon.take() {
            daemon.shutdown()?;
        }
        let deadline = Instant::now() + STOP_WAIT;
        for thread in self.threads.drain(..) {
            while !thread.is_finished() {
                if Instant::now() >= deadline {
                    warn!("discovery: a browse thread did not end in time");
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
    /// The daemon shuts down; its events end, and the threads with them.
    fn drop(&mut self) {
        if let Some(daemon) = self.daemon.take() {
            let _status = daemon.shutdown();
        }
    }
}

/// A browse thread: every event into the list, every change on `tx` (none
/// left listening: the list still follows).
fn keep(events: impl Iterator<Item = ServiceEvent>, devices: &Mutex<Devices>, tx: &Sender<Change>) {
    for ev in events {
        let change = lock(devices).apply(&ev);
        if let Some(change) = change {
            debug!(
                kind = match &change {
                    Change::Added(_) => "added",
                    Change::Changed(_) => "changed",
                    Change::Removed(_) => "removed",
                },
                "discovery: a device"
            );
            let _listened = tx.send(change);
        }
    }
}

#[cfg(test)]
mod tests;
