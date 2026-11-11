// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where the traffic watch finds our other devices on the PC (dstack
//! #203): a [`Discovery`] on the device's own address, run only while it
//! is needed (the main's decision on #203). A device whose NIC carries no
//! more than our audio never starts one, so it adds no mDNS load to the
//! network (dstack #141).
//!
//! - [`Lazy`] (pure, fake times): whether the discovery should run. It
//!   starts once a look finds the rest past our own audio (the NIC's bytes
//!   less this device's) over [`super::OTHER_BYTES_PER_S`], and stops after
//!   [`QUIET_NS`] with no such look. The rest is taken before the
//!   neighbours are subtracted: a PC loud only with our own devices keeps
//!   it, or the next look would warn about them again.
//! - [`Finder`]: the discovery as the watch uses it; [`DiscoveryFinder`] is
//!   the real one, a test gives its own.
//! - [`Neighbours`]: the list for a look ([`Neighbours::list`], empty while
//!   nothing runs), the start or stop after it ([`Neighbours::after`]), and
//!   the watch's step at each control tick ([`Neighbours::tick`]).
//!
//! The first warning of a loud NIC names no neighbour (the discovery starts
//! after it). At the first tick the started discovery lists any, the
//! watch's window starts again with them read
//! ([`super::TrafficWatch::restart`]), so the next look has both ends of
//! each and the next warning names them.

use std::net::Ipv4Addr;

use tracing::{info, warn};

use super::{Peer, Traffic, TrafficWatch, on_address, too_much};
use crate::discover::{Device, Discovery};

/// How long the discovery keeps running after the last loud look, ns.
pub const QUIET_NS: u64 = 3_600_000_000_000;

/// What the discovery should do after a look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Start,
    Stop,
    Keep,
}

/// When the last loud look was (see the module).
#[derive(Debug, Default)]
pub struct Lazy {
    last_loud: Option<u64>,
}

impl Lazy {
    /// After a look at `now_ns` that gave `t`, with the discovery `running`
    /// or not: whether to start, stop or keep it.
    pub fn step(&mut self, now_ns: u64, t: &Traffic, running: bool) -> Step {
        if too_much(rest_past_ours(t)) {
            self.last_loud = Some(now_ns);
        }
        let wanted = self
            .last_loud
            .is_some_and(|at| now_ns.saturating_sub(at) < QUIET_NS);
        match (running, wanted) {
            (false, true) => Step::Start,
            (true, false) => Step::Stop,
            _ => Step::Keep,
        }
    }
}

/// The NIC's rest past this device's audio alone: the other traffic and
/// every neighbour's audio.
pub fn rest_past_ours(t: &Traffic) -> u64 {
    t.other.saturating_add(t.our_devices() - t.ours)
}

/// The discovery as the watch uses it.
pub trait Finder: Send {
    /// Whether it runs.
    fn running(&self) -> bool;
    /// Starts it on the interface holding `ip`; the reason when it did not.
    fn start(&mut self, ip: Ipv4Addr) -> Result<(), String>;
    /// Stops it (its threads end in the background).
    fn stop(&mut self);
    /// The devices it lists now.
    fn devices(&self) -> Vec<Device>;
}

/// The real [`Finder`]: a [`Discovery`] on our address. Glue over
/// [`Discovery`], which holds mDNS's port 5353: outside the mutation gate
/// (`.cargo/mutants.toml`); its decisions are [`Lazy`]'s and
/// [`Neighbours`]'.
#[derive(Default)]
pub struct DiscoveryFinder {
    running: Option<Discovery>,
}

impl Finder for DiscoveryFinder {
    fn running(&self) -> bool {
        self.running.is_some()
    }

    fn start(&mut self, ip: Ipv4Addr) -> Result<(), String> {
        let (d, _changes) = Discovery::start(ip).map_err(|e| e.to_string())?;
        self.running = Some(d);
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(d) = self.running.take() {
            // Its stop waits for its threads (up to `discover::STOP_WAIT`):
            // never on the control tick.
            let spawned = std::thread::Builder::new()
                .name("dstack-neighbours-stop".to_string())
                .spawn(move || {
                    if let Err(e) = d.stop() {
                        warn!(error = %e, "the neighbours' discovery did not stop cleanly");
                    }
                });
            if let Err(e) = spawned {
                warn!(error = %e, "the neighbours' discovery's stop could not start: left to end");
            }
        }
    }

    fn devices(&self) -> Vec<Device> {
        self.running
            .as_ref()
            .map(Discovery::devices)
            .unwrap_or_default()
    }
}

/// Our neighbours for the traffic watch (see the module).
pub struct Neighbours {
    finder: Box<dyn Finder>,
    lazy: Lazy,
    /// Our own local status port, never a neighbour.
    own: Option<u16>,
    /// A failed start was warned about (again only after one started).
    told: bool,
    /// A discovery started and has not listed a neighbour yet: its first
    /// list starts the watch's window again ([`Neighbours::tick`]).
    fresh: bool,
}

impl Default for Neighbours {
    fn default() -> Self {
        Self::new(Box::new(DiscoveryFinder::default()))
    }
}

impl Neighbours {
    pub fn new(finder: Box<dyn Finder>) -> Self {
        Neighbours {
            finder,
            lazy: Lazy::default(),
            own: None,
            told: false,
            fresh: false,
        }
    }

    /// Our own local status port (`None`: the device has none).
    pub fn set_own(&mut self, port: Option<u16>) {
        self.own = port;
    }

    /// The neighbours on `ip` now ([`on_address`]); none while the
    /// discovery does not run.
    pub fn list(&self, ip: Ipv4Addr) -> Vec<Peer> {
        if !self.finder.running() {
            return Vec::new();
        }
        on_address(&self.finder.devices(), ip, self.own)
    }

    /// The watch's step at a control tick at `now_ns` on `ip`, with the
    /// NIC's sent bytes from `nic` and this device's audio bytes `ours`.
    /// The first tick a started discovery lists a neighbour, the window
    /// starts again with them read ([`TrafficWatch::restart`]; not while the
    /// NIC is unread). Otherwise the look ([`TrafficWatch::watch`], with
    /// [`Neighbours::list`]) and the start or stop after it
    /// ([`Neighbours::after`]). The window's traffic, when one closed.
    pub fn tick(
        &mut self,
        watch: &mut TrafficWatch,
        now_ns: u64,
        ip: Ipv4Addr,
        nic: impl FnOnce() -> Option<u64>,
        ours: u64,
    ) -> Option<Traffic> {
        if self.fresh {
            let found = self.list(ip);
            if !found.is_empty() {
                let nic = nic()?;
                self.fresh = false;
                watch.restart(now_ns, nic, ours, &found);
                info!(
                    neighbours = found.len(),
                    "our other devices on this PC found: the traffic watch's window starts again with their audio"
                );
                return None;
            }
        }
        let window = watch.watch(now_ns, nic, ours, || self.list(ip));
        self.after(now_ns, window.as_ref(), ip);
        window
    }

    /// After a look at `now_ns` that gave `t` (`None`: no window closed,
    /// nothing to do): the discovery started on `ip`, stopped or kept
    /// ([`Lazy::step`]). A start that fails is tried again at the next look
    /// (once a minute at most) and warned about once.
    pub fn after(&mut self, now_ns: u64, t: Option<&Traffic>, ip: Ipv4Addr) {
        let Some(t) = t else { return };
        match self.lazy.step(now_ns, t, self.finder.running()) {
            Step::Start => match self.finder.start(ip) {
                Ok(()) => {
                    self.told = false;
                    self.fresh = true;
                    info!(
                        "other traffic on the Dante NIC: discovery started to find our other devices on this PC"
                    );
                }
                Err(reason) => {
                    if !self.told {
                        self.told = true;
                        warn!(
                            %reason,
                            "the discovery of our other devices on this PC did not start: their audio counts as other traffic"
                        );
                    }
                }
            },
            Step::Stop => {
                self.finder.stop();
                info!(
                    "the Dante NIC was quiet for an hour: the discovery of our other devices stopped"
                );
            }
            Step::Keep => {}
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
