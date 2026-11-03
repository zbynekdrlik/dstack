// SPDX-License-Identifier: MIT OR Apache-2.0

//! The dstack devices on this PC.
//!
//! - [`own_addresses`] and [`local_devices`] (pure): the PC's IPv4
//!   addresses, and of the devices discovery lists the ones whose ARC
//!   record sits on one of them and names a local status port.
//! - [`plan`] (pure): which addresses need a discovery started, and which
//!   running ones are gone.
//! - [`Scanner`]: a [`Discovery`] per own address, kept in step with the
//!   PC's addresses on each [`Scanner::refresh`] (a tray started before the
//!   network is up finds it later), and the readings of every local device.
//!
//! Logs carry counts, never a name or an address.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;

use dstack::announce::STATUS_PORT_KEY;
use dstack::daemon::IfAddr;
use dstack::discover::{Device, Discovery};
use tracing::{debug, info, warn};

use crate::probe;

/// A dstack device on this PC: its name and its local status port.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Local {
    pub name: String,
    pub port: u16,
}

/// The PC's own IPv4 addresses discovery listens on: every interface's
/// address but loopback (a Dante device never serves on it), each once.
pub fn own_addresses(ifs: &[IfAddr]) -> Vec<Ipv4Addr> {
    let mut out: Vec<Ipv4Addr> = ifs
        .iter()
        .map(|i| i.ip)
        .filter(|ip| !ip.is_loopback())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The status port a device's ARC TXT names (`dstack_ctl=<port>`); `None`
/// without the key, or with a value that is no port (0 included).
pub fn status_port(device: &Device) -> Option<u16> {
    device
        .txt
        .get(STATUS_PORT_KEY)
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|&p| p != 0)
}

/// Of `found` (every discovery's list), the devices on this PC: an ARC
/// address in `own` and a status port. Sorted by name, each once (a device
/// two discoveries both list).
pub fn local_devices(found: &[Device], own: &[Ipv4Addr]) -> Vec<Local> {
    let mut out: Vec<Local> = found
        .iter()
        .filter(|d| own.contains(d.arc.ip()))
        .filter_map(|d| {
            status_port(d).map(|port| Local {
                name: d.name.clone(),
                port,
            })
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The addresses of `own` with no discovery yet (to start), and the
/// running ones no longer in `own` (to stop).
pub fn plan(running: &[Ipv4Addr], own: &[Ipv4Addr]) -> (Vec<Ipv4Addr>, Vec<Ipv4Addr>) {
    let start = own
        .iter()
        .filter(|ip| !running.contains(ip))
        .copied()
        .collect();
    let stop = running
        .iter()
        .filter(|ip| !own.contains(ip))
        .copied()
        .collect();
    (start, stop)
}

/// A device's reading: its name and its status line, or why there is none.
pub type Reading = (String, Result<String, String>);

/// Discovery on each of the PC's addresses (see the module).
#[derive(Default)]
pub struct Scanner {
    running: BTreeMap<Ipv4Addr, Discovery>,
}

impl Scanner {
    /// A scanner with no discovery yet; [`Scanner::refresh`] starts them.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads the PC's addresses, starts discovery on each new one and stops
    /// it on each gone one. The failures, as text: the interfaces could not
    /// be read, or a discovery did not start (tried again on the next
    /// refresh).
    pub fn refresh(&mut self) -> Vec<String> {
        let ifs = match dstack::host::interfaces() {
            Ok(ifs) => ifs,
            Err(e) => {
                warn!(error = %e, "dtray: the interfaces could not be read");
                return vec![format!("the interfaces could not be read: {e}")];
            }
        };
        let own = own_addresses(&ifs);
        let running: Vec<Ipv4Addr> = self.running.keys().copied().collect();
        let (start, stop) = plan(&running, &own);
        for ip in stop {
            if let Some(d) = self.running.remove(&ip) {
                info!("dtray: an address went away; its discovery stops");
                if let Err(e) = d.stop() {
                    warn!(error = %e, "dtray: a discovery did not stop cleanly");
                }
            }
        }
        let mut failed = Vec::new();
        for ip in start {
            match Discovery::start(ip) {
                Ok((d, _changes)) => {
                    info!("dtray: discovery started on an address");
                    self.running.insert(ip, d);
                }
                Err(e) => {
                    warn!(error = %e, "dtray: discovery did not start on an address");
                    failed.push(format!("discovery did not start on an interface: {e}"));
                }
            }
        }
        debug!(addresses = self.running.len(), "dtray: discovery running");
        failed
    }

    /// The devices on this PC now.
    pub fn devices(&self) -> Vec<Local> {
        let own: Vec<Ipv4Addr> = self.running.keys().copied().collect();
        let found: Vec<Device> = self.running.values().flat_map(Discovery::devices).collect();
        local_devices(&found, &own)
    }

    /// Every local device's status now ([`probe::status`]), by name.
    pub fn readings(&self) -> Vec<Reading> {
        self.devices()
            .into_iter()
            .map(|d| {
                let r = probe::status(d.port).map_err(|e| e.to_string());
                (d.name, r)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
