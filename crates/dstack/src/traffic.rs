//! Other traffic on the Dante NIC, watched while the device runs (dstack
//! #1, the FOH PC, 1.11.2026). Every UDP sender there saw 1 to 2.5 ms gaps
//! every ~285 ms, the period of another program's TCP bursts on the same
//! NIC: one NIC queue has no priority between packets (no DCB on consumer
//! NICs), so our audio waits behind whatever the NIC holds.
//!
//! Once a minute the watch compares the NIC's own sent-bytes counter with
//! our devices' audio and warns when the rest averages over
//! [`OTHER_BYTES_PER_S`]. Our own control traffic (heartbeats, mDNS, ARC
//! answers) is a few kB/s, well under it.
//!
//! Our devices' audio (dstack #203): this device's wire bytes
//! ([`TxShared::bytes_sent`]) and every other dstack device's on the same
//! PC, read from its local status endpoint ([`crate::ctl::local`], the
//! status's `tx_bytes`) through the stack's own client
//! ([`crate::ctl::probe`]), all of them within [`PEERS_WAIT`] at each look.
//! A neighbour counts in a window only when it was read at both of its
//! ends with a counter that did not go back; one that appeared, went away
//! or restarted meanwhile stays in the rest for that window. One that does
//! not answer is left out and warned about once (again only after it
//! answered).
//!
//! - [`TrafficWatch::look_with`] (pure, fake times): the window's
//!   arithmetic, [`Traffic`] per second.
//! - [`deltas`] (pure): the neighbours read at both ends.
//! - [`tx_bytes_of`] and [`reply_bytes`] (pure): a neighbour's status line
//!   read.
//! - [`our_devices`] (pure): the warning's text of our devices' part.
//! - [`read_peers`]: the reads, one deadline for all.
//! - [`neighbours()`] (pure): our devices on our address in a discovery's
//!   list; [`mod@neighbours`]: that discovery, run only while the NIC is
//!   loud.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use tracing::warn;

use crate::ctl::probe::{self, ProbeError};
use crate::media::tx::TxShared;

pub mod neighbours;

/// How often the watch looks, ns.
pub const EVERY_NS: u64 = 60_000_000_000;
/// Other traffic past this average, bytes per second, is warned about.
pub const OTHER_BYTES_PER_S: u64 = 100_000;
/// How long reading every neighbour may take at a look, together: the
/// watch runs on the daemon's control tick, which a stuck neighbour holds
/// this long at most, once a minute. A healthy one answers within a
/// millisecond on 127.0.0.1.
pub const PEERS_WAIT: Duration = Duration::from_millis(200);

/// A neighbour on the PC: another dstack device's name and its local
/// status port.
pub type Peer = (String, u16);

/// Each neighbour's `tx_bytes` at a look, by name.
pub type Readings = BTreeMap<String, u64>;

/// One window's traffic on the NIC, bytes per second.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Traffic {
    /// What is left past our devices' audio.
    pub other: u64,
    /// This device's audio.
    pub ours: u64,
    /// Each neighbour's audio read at both ends of the window, by name.
    pub devices: Vec<(String, u64)>,
}

impl Traffic {
    /// Our devices' audio together: this device's and every neighbour's.
    pub fn our_devices(&self) -> u64 {
        self.ours + self.devices.iter().map(|(_, b)| b).sum::<u64>()
    }
}

/// A look's counters, kept for the next one.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Mark {
    at: u64,
    nic: u64,
    ours: u64,
    peers: Readings,
}

/// The watch's memory: the last look's time and counters, and the
/// neighbours warned about as unreadable.
#[derive(Debug, Default)]
pub struct TrafficWatch {
    last: Option<Mark>,
    unread: BTreeSet<String>,
}

impl TrafficWatch {
    pub const fn new() -> Self {
        TrafficWatch {
            last: None,
            unread: BTreeSet::new(),
        }
    }

    /// Whether a look at `now_ns` counts: the first one, or [`EVERY_NS`]
    /// after the last.
    pub fn due(&self, now_ns: u64) -> bool {
        self.last
            .as_ref()
            .is_none_or(|m| now_ns.saturating_sub(m.at) >= EVERY_NS)
    }

    /// At `now_ns`, with the NIC's sent bytes `nic` (none when unread) and
    /// ours `ours`: once [`EVERY_NS`] has passed since the last look, the
    /// other traffic's average in bytes per second; `None` between looks,
    /// at the first look and when the NIC is unread. A counter that went
    /// back (a NIC reset) starts the window again. It is
    /// [`TrafficWatch::look_with`] with no neighbour.
    pub fn look(&mut self, now_ns: u64, nic: Option<u64>, ours: u64) -> Option<u64> {
        self.look_with(now_ns, nic, ours, &Readings::new())
            .map(|t| t.other)
    }

    /// [`TrafficWatch::look`] with the neighbours' `tx_bytes` `peers` read
    /// at `now_ns` too: the window's [`Traffic`], each neighbour's audio
    /// ([`deltas`]) taken off the rest with ours.
    pub fn look_with(
        &mut self,
        now_ns: u64,
        nic: Option<u64>,
        ours: u64,
        peers: &Readings,
    ) -> Option<Traffic> {
        let nic = nic?;
        if !self.due(now_ns) {
            return None;
        }
        let mark = Mark {
            at: now_ns,
            nic,
            ours,
            peers: peers.clone(),
        };
        let last = self.last.replace(mark)?;
        if nic < last.nic || ours < last.ours {
            return None;
        }
        let secs = ((now_ns - last.at) / 1_000_000_000).max(1);
        let devices = deltas(&last.peers, peers);
        let mine = ours - last.ours;
        let all_ours = mine + devices.iter().map(|(_, b)| b).sum::<u64>();
        let other = (nic - last.nic).saturating_sub(all_ours);
        Some(Traffic {
            other: other / secs,
            ours: mine / secs,
            devices: devices.into_iter().map(|(n, b)| (n, b / secs)).collect(),
        })
    }

    /// A neighbour's read `outcome` as a reading: its bytes, or `None`,
    /// warned about the first time it failed (again only after a read).
    pub fn take(&mut self, name: &str, outcome: Result<u64, String>) -> Option<u64> {
        match outcome {
            Ok(bytes) => {
                self.unread.remove(name);
                Some(bytes)
            }
            Err(reason) => {
                if self.unread.insert(name.to_string()) {
                    warn!(
                        device = name,
                        %reason,
                        "a dstack device on this PC could not be read: its audio counts as other traffic until it is"
                    );
                }
                None
            }
        }
    }

    /// [`TrafficWatch::look_with`] on the NIC holding `ip`, `tx`'s audio
    /// and the neighbours' that `peers` lists (asked and read within
    /// [`PEERS_WAIT`] only when a look is due), a rate over
    /// [`OTHER_BYTES_PER_S`] logged as a warning with our devices' part.
    /// The window's traffic, for the caller ([`neighbours::Neighbours::after`]).
    pub fn watch(
        &mut self,
        now_ns: u64,
        ip: Ipv4Addr,
        tx: &TxShared,
        peers: impl FnOnce() -> Vec<Peer>,
    ) -> Option<Traffic> {
        if !self.due(now_ns) {
            return None;
        }
        let readings: Readings = read_peers(&peers(), PEERS_WAIT)
            .into_iter()
            .filter_map(|(name, r)| self.take(&name, r).map(|b| (name, b)))
            .collect();
        let t = self.look_with(now_ns, nic_sent_bytes(ip), tx.bytes_sent(), &readings)?;
        if too_much(t.other) {
            warn!(
                other_bytes_per_s = t.other,
                our_devices_bytes_per_s = t.our_devices(),
                our_devices = %our_devices(&t),
                "the Dante NIC carries other traffic besides our devices' audio: our audio waits behind it in the NIC's queue (late packets at a receiver); move that traffic to another NIC (docs/windows-box.md, docs/linux-box.md)"
            );
        }
        Some(t)
    }
}

/// Of `found` (a discovery's list), our neighbours on the NIC holding `ip`:
/// the dstack devices whose ARC record is at `ip` and names a status port
/// ([`crate::discover::Device::status_port`]) other than ours, `own` (a
/// device without a local endpoint passes `None`). Sorted by name, each
/// once.
pub fn neighbours(found: &[crate::discover::Device], ip: Ipv4Addr, own: Option<u16>) -> Vec<Peer> {
    let mut out: Vec<Peer> = found
        .iter()
        .filter(|d| *d.arc.ip() == ip)
        .filter_map(|d| d.status_port().map(|port| (d.name.clone(), port)))
        .filter(|&(_, port)| Some(port) != own)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Each neighbour in `now` also read in `before` with a counter that did
/// not go back: its name and the bytes between, in name order. One that
/// appeared, went away or restarted is left out.
pub fn deltas(before: &Readings, now: &Readings) -> Vec<(String, u64)> {
    now.iter()
        .filter_map(|(name, &b)| {
            let a = *before.get(name)?;
            b.checked_sub(a).map(|d| (name.clone(), d))
        })
        .collect()
}

/// Our devices' part of `t` as the warning shows it: this device's rate,
/// then each neighbour's by name, bytes per second.
pub fn our_devices(t: &Traffic) -> String {
    let mut out = format!("this device {}", t.ours);
    for (name, b) in &t.devices {
        out.push_str(&format!(", {name} {b}"));
    }
    out
}

/// The `tx_bytes` of a device's status line; `None` when the line is no
/// JSON object or carries none (a stack before dstack #203).
pub fn tx_bytes_of(status: &str) -> Option<u64> {
    serde_json::from_str::<serde_json::Value>(status)
        .ok()?
        .get("tx_bytes")?
        .as_u64()
}

/// The reason a status line without `tx_bytes` gives.
pub const NO_TX_BYTES: &str = "its status carries no tx_bytes (a stack before dstack #203)";

/// A neighbour's status `read` as its bytes, or why there are none.
pub fn reply_bytes(read: Result<String, ProbeError>) -> Result<u64, String> {
    let line = read.map_err(|e| e.to_string())?;
    tx_bytes_of(&line).ok_or_else(|| NO_TX_BYTES.to_string())
}

/// Each of `peers`' bytes ([`reply_bytes`]), read one after another from
/// 127.0.0.1 within `wait` for all: a neighbour past the deadline is not
/// read (its reason says so).
pub fn read_peers(peers: &[Peer], wait: Duration) -> Vec<(String, Result<u64, String>)> {
    let deadline = Instant::now() + wait;
    peers
        .iter()
        .map(|(name, port)| {
            let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, *port));
            let read = probe::time_left(deadline, Instant::now())
                .map_err(ProbeError::from)
                .and_then(|left| probe::status_at(addr, left));
            (name.clone(), reply_bytes(read))
        })
        .collect()
}

/// Whether other traffic at `rate` bytes per second is warned about.
pub fn too_much(rate: u64) -> bool {
    rate > OTHER_BYTES_PER_S
}

/// The name of the interface holding `ip` among `ifaces` (name, address).
pub fn name_of(ip: Ipv4Addr, ifaces: &[(String, std::net::IpAddr)]) -> Option<String> {
    ifaces
        .iter()
        .find(|(_, a)| *a == std::net::IpAddr::V4(ip))
        .map(|(n, _)| n.clone())
}

/// The bytes the NIC holding `ip` has sent, its own counter; `None` when
/// no interface holds `ip` or the counter cannot be read.
#[cfg(target_os = "linux")]
pub fn nic_sent_bytes(ip: Ipv4Addr) -> Option<u64> {
    let ifaces: Vec<_> = if_addrs::get_if_addrs()
        .ok()?
        .into_iter()
        .map(|i| (i.name.clone(), i.ip()))
        .collect();
    let name = name_of(ip, &ifaces)?;
    std::fs::read_to_string(format!("/sys/class/net/{name}/statistics/tx_bytes"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::nic_sent_bytes;

#[cfg(not(any(target_os = "linux", windows)))]
pub fn nic_sent_bytes(_ip: Ipv4Addr) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests;
