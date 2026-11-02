// SPDX-License-Identifier: GPL-3.0-or-later

//! dlink: the Dante device an application embeds (owner, 2.11.2026,
//! dstack #68). OBS, SongPlayer, the intercom and whatever comes next
//! carry it in their own process and appear on the Dante network as a
//! device of their own, under their own name.
//!
//! Its media clock is dsync's in-process follower (dsync-ptp) when it is
//! locked, and the PC's dsync service otherwise: it works whether the
//! service runs or not, and is never worse than it (owner, 2.11.2026). On
//! Linux the follower needs port 319 (CAP_NET_BIND_SERVICE); without it
//! the device logs why and runs on the service. On Windows the follower
//! comes with dsync's run-time Npcap load.
//!
//! It needs no low latency: the device latency is 10 ms unless the app
//! asks for another (4 ms at the least), and it asks the app for no
//! real-time setup. Its media threads run at normal priority, unpinned,
//! wait on the high-resolution timer without spinning a core, and on
//! Windows opt out of power throttling (our threads only). The pump's
//! work margin is half the latency, so a late wake still makes its block.
//! Very low latency is a different situation, for iemmixer and dasio,
//! which use `dstack` directly with its `low_latency` profile.
//!
//! ```no_run
//! use dlink::{Block, Config, Link, Map, Place, Process};
//!
//! struct Gain;
//! impl Process for Gain {
//!     fn process(&mut self, block: &mut Block<'_>) {
//!         let input = block.input(0).to_vec();
//!         for (out, x) in block.output(0).iter_mut().zip(input) {
//!             *out = x * 0.5;
//!         }
//!     }
//! }
//!
//! let config = Config::new("songplayer", "/var/lib/songplayer/dlink.toml")
//!     .rx(["In L", "In R"])
//!     .tx(["Out L", "Out R"]);
//! let map = Map { rx: vec![0, 1], tx: vec![0, 1] };
//! let link = Link::start(&config, &Place::Iface("eth1".into()), map, Gain).unwrap();
//! // ... the app runs ...
//! link.stop();
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub use dstack::daemon::Place;
pub use dstack::daemon::media::Media;
pub use dstack::device::DeviceConfig;
pub use dstack::host::{HostError, Stopped};
pub use dstack::stream::Map;
pub use dstack_audio_io::{Block, Process};

use dstack::device::{TimingProfile, normal_work_margin};
use dstack::embed::Embedded;
use dstack::host::ClockSource;

/// dlink's device latency unless the app sets one: 10 ms (owner,
/// 2.11.2026: "kľudne základ môže byť aj 10 ms").
pub const LATENCY_NS: u64 = 10_000_000;
/// The sample rate unless the app sets one.
pub const SAMPLE_RATE: u32 = 48_000;
/// How long [`Link::stop`] waits for the device's goodbyes.
pub const STOP_WAIT: Duration = Duration::from_secs(5);

/// What an app says about its device. Everything else is dlink's default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    name: String,
    state_file: PathBuf,
    rx: Vec<String>,
    tx: Vec<String>,
    sample_rate: u32,
    latency_ns: u64,
    manufacturer: String,
    model: String,
    ports: Option<Ports>,
    process_id: Option<u16>,
    rx_port_base: Option<u16>,
    subscribe: Vec<(usize, String, String)>,
}

/// A device's own control ports, for a second device on one address (the
/// first keeps Dante's defaults: ARC 4440, CMC 8800, flows 4455).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ports {
    pub arc: u16,
    pub cmc: u16,
    pub flows: u16,
}

impl Config {
    /// A device named `name` (the name Dante Controller lists: 1 to 31 of
    /// A-Z a-z 0-9 and '-'), keeping what Dante Controller sets (names,
    /// subscriptions, latency) in `state_file`. No channels yet.
    pub fn new(name: impl Into<String>, state_file: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            state_file: state_file.into(),
            rx: Vec::new(),
            tx: Vec::new(),
            sample_rate: SAMPLE_RATE,
            latency_ns: LATENCY_NS,
            manufacturer: "dlink".to_string(),
            model: "dlink".to_string(),
            ports: None,
            process_id: None,
            rx_port_base: None,
            subscribe: Vec::new(),
        }
    }

    /// The RX channels' names, in channel order.
    pub fn rx<S: Into<String>>(mut self, names: impl IntoIterator<Item = S>) -> Self {
        self.rx = names.into_iter().map(Into::into).collect();
        self
    }

    /// The TX channels' names, in channel order.
    pub fn tx<S: Into<String>>(mut self, names: impl IntoIterator<Item = S>) -> Self {
        self.tx = names.into_iter().map(Into::into).collect();
        self
    }

    /// The sample rate, Hz.
    pub fn sample_rate(mut self, hz: u32) -> Self {
        self.sample_rate = hz;
        self
    }

    /// The device latency, ns (4 ms at the least).
    pub fn latency_ns(mut self, ns: u64) -> Self {
        self.latency_ns = ns;
        self
    }

    /// The manufacturer and model Dante Controller shows (both `dlink`
    /// unless set).
    pub fn product(mut self, manufacturer: impl Into<String>, model: impl Into<String>) -> Self {
        self.manufacturer = manufacturer.into();
        self.model = model.into();
        self
    }

    /// The control ports, when another device on the same address holds
    /// Dante's defaults (several apps on one PC, intercom#5).
    pub fn ports(mut self, ports: Ports) -> Self {
        self.ports = Some(ports);
        self
    }

    /// The process id that tells this device apart from the others on the
    /// same address (0 unless set; each device on an address needs its own).
    pub fn process_id(mut self, id: u16) -> Self {
        self.process_id = Some(id);
        self
    }

    /// The RX flows' ports from `base` (`base` to `base + 31`), for a
    /// firewall that names them; the OS picks free ones unless set.
    pub fn rx_port_base(mut self, base: u16) -> Self {
        self.rx_port_base = Some(base);
        self
    }

    /// RX channel `rx` (from 1) receives `tx_channel` of `tx_device`, by
    /// their Dante names (intercom#5). It holds from the first start and
    /// whenever the state file holds no subscription for that channel; one
    /// Dante Controller sets is saved there and wins.
    pub fn subscribe(
        mut self,
        rx: usize,
        tx_channel: impl Into<String>,
        tx_device: impl Into<String>,
    ) -> Self {
        self.subscribe
            .push((rx, tx_channel.into(), tx_device.into()));
        self
    }

    /// The device config dstack runs: the normal timing profile, the pump
    /// margin half the latency, 24-bit. Every problem is listed, as a
    /// config file's would be.
    pub fn device(&self) -> Result<DeviceConfig, Vec<String>> {
        let mut t = toml::Table::new();
        t.insert("name".into(), self.name.clone().into());
        t.insert("manufacturer".into(), self.manufacturer.clone().into());
        t.insert("model".into(), self.model.clone().into());
        t.insert("sample_rate".into(), i64::from(self.sample_rate).into());
        t.insert("encoding".into(), 24.into());
        t.insert("latency_ns".into(), to_int(self.latency_ns).into());
        t.insert(
            "work_margin".into(),
            to_int(normal_work_margin(self.latency_ns, self.sample_rate)).into(),
        );
        t.insert("timing".into(), "normal".into());
        t.insert(
            "state_file".into(),
            self.state_file.to_string_lossy().into_owned().into(),
        );
        if let Some(p) = self.ports {
            t.insert("arc_port".into(), i64::from(p.arc).into());
            t.insert("cmc_port".into(), i64::from(p.cmc).into());
            t.insert("flows_port".into(), i64::from(p.flows).into());
        }
        if let Some(id) = self.process_id {
            t.insert("process_id".into(), i64::from(id).into());
        }
        if let Some(base) = self.rx_port_base {
            t.insert("rx_port_base".into(), i64::from(base).into());
        }
        if !self.subscribe.is_empty() {
            let subs: Vec<toml::Value> = self
                .subscribe
                .iter()
                .map(|(rx, ch, dev)| {
                    let mut s = toml::Table::new();
                    s.insert(
                        "rx".into(),
                        to_int(u64::try_from(*rx).unwrap_or(u64::MAX)).into(),
                    );
                    s.insert("tx_channel".into(), ch.clone().into());
                    s.insert("tx_device".into(), dev.clone().into());
                    s.into()
                })
                .collect();
            t.insert("subscribe".into(), subs.into());
        }
        t.insert("rx".into(), self.rx.clone().into());
        t.insert("tx".into(), self.tx.clone().into());
        let text = toml::to_string(&t).map_err(|e| vec![e.to_string()])?;
        let cfg = DeviceConfig::from_toml(&text).map_err(|e| e.problems)?;
        debug_assert_eq!(cfg.timing, TimingProfile::Normal);
        Ok(cfg)
    }
}

fn to_int(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// The device running inside the app.
pub struct Link {
    device: Embedded,
}

impl Link {
    /// Starts the device of `config` on `place`, running `processor` every
    /// block on the RX channels `map.rx` into the TX channels `map.tx`.
    /// Its media clock is dsync's in-process follower when it is locked
    /// and the PC's dsync service otherwise (the owner's rule, dstack #68:
    /// a clock with or without the service, never worse than it). Call it
    /// outside any tokio runtime: the device runs its own.
    pub fn start<P: Process + 'static>(
        config: &Config,
        place: &Place,
        map: Map,
        processor: P,
    ) -> Result<Link, HostError> {
        Self::start_on(config, place, map, processor, ClockSource::Linked)
    }

    /// [`Link::start`] on the clock `source` (`ClockSource::System` is for
    /// tests only).
    pub fn start_on<P: Process + 'static>(
        config: &Config,
        place: &Place,
        map: Map,
        processor: P,
        source: ClockSource,
    ) -> Result<Link, HostError> {
        let cfg = config.device().map_err(|problems| {
            HostError::Setup(format!("dlink config: {}", problems.join("; ")))
        })?;
        let device = Embedded::start_with(cfg, place, map, processor, source)?;
        Ok(Link { device })
    }

    /// The device's media side: the pump's and the flows' counts.
    pub fn media(&self) -> &Arc<Media> {
        self.device.media()
    }

    /// Why the device ended by itself, if it did.
    pub fn ended(&self) -> Option<String> {
        self.device.ended()
    }

    /// Stops the pump, then the device, its goodbyes sent ([`STOP_WAIT`]).
    pub fn stop(self) -> Stopped {
        self.device.stop(STOP_WAIT)
    }
}

pub use dstack::host::ClockSource as Clock;

#[cfg(test)]
mod tests;
