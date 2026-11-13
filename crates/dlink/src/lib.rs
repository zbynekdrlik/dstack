// SPDX-License-Identifier: MIT OR Apache-2.0

//! dlink: the Dante device an application embeds (owner, 2.11.2026,
//! dstack #68). OBS, SongPlayer, the intercom and whatever comes next
//! carry it in their own process and appear on the Dante network as a
//! device of their own, under their own name.
//!
//! Its media clock is dsync's in-process follower (dsync-ptp) when it is
//! locked, and the PC's dsync service otherwise: it works whether the
//! service runs or not, and is never worse than it (owner, 2.11.2026). On
//! Linux the follower needs port 319 (CAP_NET_BIND_SERVICE); without it
//! the device logs why and runs on the service. On Windows it needs Npcap
//! (loaded at run time, dsync v1.27); without it the device logs why and
//! runs on the service.
//!
//! It needs no low latency, and it asks the app for no real-time setup.
//! Its media threads run unpinned and wait on the high-resolution timer
//! without spinning a core. On Windows they ask THREAD_PRIORITY_HIGHEST
//! within the process's normal class, join MMCSS "Pro Audio" at
//! AVRT_PRIORITY_HIGH (so an app's own MMCSS audio threads no longer
//! outrank them; dstack #1) and opt out of power throttling, all without
//! privilege, for our threads only, nothing process-wide; elsewhere they
//! run at normal priority. Very low latency is a different situation, for
//! iemmixer and dasio, which use `dstack` directly with its `low_latency`
//! profile.
//!
//! # The latency, in named terms
//!
//! A dlink device's latency is three named terms, each with its value and
//! who sets it (owner, ROZHODNUTÉ 12.11.2026, dstack #244; it replaces the
//! single 15 ms budget of dstack #68 and #1):
//!
//! - (a) the Dante network latency ([`Config::network_latency_ns`], per
//!   device): the subscription's latency as Dante Controller shows it (0.25,
//!   0.5, 1, 2 or 5 ms). 1 ms unless set ([`NETWORK_LATENCY_NS`]).
//! - (b) the receive buffer ([`Config::receive_buffer_ns`], per device):
//!   this PC's room for the RX thread's wake and pass, sized from
//!   measurement (the status's `wake_delay_us`, `wake_delay_peak_us`, and
//!   the measured `network_latency_*_us`; dstack #234).
//! - (c) the app hand-off, computed by dlink and stated: on the RX side the
//!   pump's block B ([`Config::block`], 128 frames unless set; dstack #90)
//!   plus the rate converter's RX delay ([`Rated`], [`rated::delay_ns`]);
//!   on the TX side the pump's work margin W plus the converter's TX delay.
//!
//! The device's Dante latency P = (a) + (b), in whole samples (rounded up).
//! P is what its RX plays out at, measured from the sender's media-clock
//! stamp, and what its TX channels advertise (ARC) and Dante Controller
//! shows. The RX path (a sample's stamp to the app's block) is P + (c) RX;
//! the TX path (the app's output to a receiver's playout) is (c) TX + P.
//! [`split`] gives every term ([`Split`]), and the device logs them at its
//! start, one line each ([`Split::lines`]), never as one lump.
//!
//! W is how late the pump may run: a pump more than W behind a block starts
//! its schedule again, and the skipped blocks are lost on both sides
//! (`dstack::stream::Blocks`; dstack #1: 2 ms restarted on a loaded PC).
//! On a device with TX it is in the TX path and 6 ms unless set
//! ([`WORK_MARGIN_NS`], room for the 2.7 to 3.6 ms preemptions seen on
//! loaded PCs, where a dlink app's threads run unpinned and without real
//! time). An RX-only device's W is in none of its paths (a late block only
//! reaches the app later), so it is 15 ms ([`RX_WORK_MARGIN_NS`]; v0.6.32,
//! dstack #1: OBS on a loaded PC was preempted about 6 ms). W is at least
//! the block and 2 ms ([`least_work_margin`]). The default W of a device
//! with TX gives way, as up to v0.6.105, where the old 15 ms path left no
//! room for both it and P's TX floor (a converter's long TX delay: a
//! 22.05 kHz app on a 96 kHz network gets W 2.29 ms); a W the app sets
//! never gives way.
//!
//! P has a floor: on a device with TX, a TX packet's own age when it leaves
//! ([`tx_floor_ns`]: the most frames in a packet + the TX margin + 4 ms of
//! lateness, 4.58 ms at 96 kHz, 4.92 ms at 48 kHz; dstack #1), and
//! [`MIN_LATENCY_NS`] (4 ms) on an RX-only device. [`Config::device`]
//! refuses (a) + (b) under it, with the floor named. A latency Dante
//! Controller sets on the device replaces P (it is kept in the state file).
//!
//! ## The defaults keep the latency of v0.6.105
//!
//! An app that sets no receive buffer gets the P, W and paths it had under
//! the old 15 ms budget ([`REFERENCE_PATH_NS`]), for every config: 15 ms
//! less the work of the directions the device has (the larger direction's
//! when it has both), at least the floor; its (b) is that P less 1 ms.
//! Setting (a) alone moves P by the difference (the terms are
//! independent), never under the floor. At block 128, W 6 ms with TX:
//!
//! | Device | App | Directions | (a) | (b) | P | (c) RX | (c) TX |
//! |---|---|---|---|---|---|---|---|
//! | 48 kHz | 48 kHz | both | 1 ms | 8 ms | 9 ms | 2.67 ms | 6 ms |
//! | 96 kHz | 96 kHz | both | 1 ms | 8 ms | 9 ms | 1.33 ms | 6 ms |
//! | 96 kHz | 48 kHz | both | 1 ms | 7.36 ms | 8.36 ms | 1.33 + 0.63 ms | 6 + 0.64 ms |
//! | 96 kHz | 44.1 kHz | both | 1 ms | 4.92 ms | 5.92 ms | 1.33 + 1.32 ms | 6 + 3.08 ms |
//! | 48 kHz | 48 kHz | RX | 1 ms | 11.33 ms | 12.33 ms | 2.67 ms | |
//! | 96 kHz | 96 kHz | RX | 1 ms | 12.67 ms | 13.67 ms | 1.33 ms | |
//! | 96 kHz | 48 kHz | RX | 1 ms | 12.04 ms | 13.04 ms | 1.33 + 0.63 ms | |
//!
//! The last row is OBS on the editing PC: P 13.041666 ms = (a) 1 ms + (b)
//! 12.041666 ms, RX path 15 ms. A config the old budget could not hold
//! (an 8, 11.025 or 16 kHz app, whose filters alone take up to about
//! 19 ms; a 22.05 or 32 kHz app on a 44.1 or 48 kHz device) now starts at
//! P's floor instead of being refused. Our TX looped into our own RX takes the two
//! paths less the one P they share ((c) TX + P + (c) RX): the
//! whole-device test measures it.
//!
//! # An app on its own clock
//!
//! An app whose audio runs on its own clock (OBS's mix, a sound card)
//! cannot hand it to the pump block for block: the two clocks drift. The
//! app writes a [`bridge::Fifo`] (made with [`bridge::fifo_for`]) on its
//! own audio thread, and its [`Process`] fills the device's block with
//! [`bridge::Player::fill_with`], which converts from the app's rate and
//! steers the FIFO's depth against the drift ([`bridge`]; dsend's path,
//! dstack #241). Neither side takes a lock or allocates. That depth adds
//! to the path above: the app buffer, a fixed term the app states with
//! [`Config::app_buffer`] (its chunk, and a configured value that wins;
//! dstack #247) and the device logs at its start; [`Config::bridge_buffer`]
//! gives it to the [`bridge::Player`].
//!
//! # Debug builds with a rate converter
//!
//! A device whose app rate differs from the network's runs the converter
//! ([`Rated`], `dstack-resample` over rubato) inside the pump's block.
//! Unoptimised, rubato takes longer than a block: intercom's debug tests
//! (a 48 kHz app on a 96 kHz device, block 1.33 ms) overran every block,
//! the longest 2.84 ms, and the tone broke (dstack #1, 4.11.2026); at
//! opt-level 3 the same block took at most 0.14 ms with no overrun. Cargo
//! takes profiles only from the top workspace, so a consumer that runs a
//! converting device in a debug build or its tests sets, in its own
//! workspace `Cargo.toml` (as dstack's own does):
//!
//! ```toml
//! [profile.dev.package.rubato]
//! opt-level = 3
//!
//! [profile.dev.package.dstack-resample]
//! opt-level = 3
//! ```
//!
//! A pump that overruns or restarts logs `engine pump lost or overran
//! blocks this second` once a second, with its `overruns` and that
//! second's longest block, `max_process_us` (dstack 0.6.25; that second's
//! since 0.6.32, the longest since the start before).
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
use std::sync::atomic::AtomicBool;
use std::time::Duration;

pub use dstack::daemon::Place;
pub use dstack::daemon::media::{Media, RxChannel};
pub use dstack::device::DeviceConfig;
pub use dstack::host::{HostError, Stopped};
pub use dstack::stream::Map;
pub use dstack::subscribe::{SubState, SubStatus};
pub use dstack_audio_io::{Block, Process};

pub mod bridge;
pub mod rated;

mod latency;

pub use latency::{
    Directions, MIN_LATENCY_NS, NETWORK_LATENCY_NS, REFERENCE_PATH_NS, RX_WORK_MARGIN_NS, Split,
    TX_LATE_NS, Terms, WORK_MARGIN_NS, default_work_margin, least_work_margin, ms, split,
    tx_floor_ns,
};
pub use rated::Rated;

use dstack::daemon::PlaceWait;
use dstack::device::TimingProfile;
use dstack::embed::Embedded;
use dstack::host::ClockSource;
use dstack::media::bypass::FrameSender;

/// The sample rate unless the app sets one.
pub const SAMPLE_RATE: u32 = 48_000;
/// The pump's block unless the app sets one, frames (1.33 ms at 96 kHz,
/// 2.67 ms at 48 kHz): a quarter of the stack's wakes at block 32, and
/// four times the frames for each [`Rated`] call (dstack #90).
pub const BLOCK: u64 = 128;
/// The app rates dlink converts from and to, Hz.
pub const APP_RATES: std::ops::RangeInclusive<u32> = 8_000..=384_000;
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
    network_rate: Option<u32>,
    network_latency_ns: Option<u64>,
    receive_buffer_ns: Option<u64>,
    work_margin_ns: Option<u64>,
    /// The bridge app's chunk and its configured app buffer, ns (dstack
    /// #247).
    app_buffer: Option<(u64, Option<u64>)>,
    manufacturer: String,
    model: String,
    ports: Option<Ports>,
    process_id: Option<u16>,
    rx_port_base: Option<u16>,
    subscribe: Vec<(usize, String, String)>,
    flow_slots: Option<usize>,
    block: u64,
    frame_sender: Sender,
}

/// The host's frame sender in a [`Config`] (compared by identity).
#[derive(Clone, Default)]
struct Sender(Option<Arc<dyn FrameSender>>);

impl std::fmt::Debug for Sender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() { "Some(..)" } else { "None" })
    }
}

impl PartialEq for Sender {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (a, b) => a.is_none() && b.is_none(),
        }
    }
}

impl Eq for Sender {}

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
            network_rate: None,
            network_latency_ns: None,
            receive_buffer_ns: None,
            work_margin_ns: None,
            app_buffer: None,
            manufacturer: "dlink".to_string(),
            model: "dlink".to_string(),
            ports: None,
            process_id: None,
            rx_port_base: None,
            subscribe: Vec::new(),
            flow_slots: None,
            block: BLOCK,
            frame_sender: Sender::default(),
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

    /// The app's sample rate, Hz: its blocks come at this rate.
    pub fn sample_rate(mut self, hz: u32) -> Self {
        self.sample_rate = hz;
        self
    }

    /// The network's sample rate, Hz, when it differs from the app's: the
    /// device runs at it (a Dante flow needs both ends at one rate), and
    /// [`Rated`] converts between it and the app's rate on the device's
    /// clock (dstack #90: 96 kHz on the site, 48 kHz for the intercom and
    /// OBS). Unset: the app's rate.
    pub fn network_rate(mut self, hz: u32) -> Self {
        self.network_rate = Some(hz);
        self
    }

    /// The rate the device runs at: the network's, else the app's.
    pub fn device_rate(&self) -> u32 {
        self.network_rate.unwrap_or(self.sample_rate)
    }

    /// Whether the app's rate differs from the device's ([`Rated`] runs).
    pub fn converts(&self) -> bool {
        self.device_rate() != self.sample_rate
    }

    /// (a), the Dante network latency of this device's subscriptions, ns
    /// ([`NETWORK_LATENCY_NS`], 1 ms, unless set): what Dante Controller
    /// shows as the latency (0.25, 0.5, 1, 2 or 5 ms; about 1 ms or less on
    /// a one-switch gigabit VLAN with a hardware sender). The device's
    /// Dante latency P is this plus the receive buffer ([`split`], dstack
    /// #244).
    pub fn network_latency_ns(mut self, ns: u64) -> Self {
        self.network_latency_ns = Some(ns);
        self
    }

    /// (b), the receive buffer, ns: this PC's room for the RX thread's wake
    /// and pass, sized from measurement (the status's `wake_delay_us` and
    /// `wake_delay_peak_us`; dstack #234, #244). P = (a) + (b). Unset, it is
    /// what keeps P as it was up to v0.6.105 ([`REFERENCE_PATH_NS`]): the
    /// default changes no app's latency. A P under its floor
    /// ([`MIN_LATENCY_NS`], or [`tx_floor_ns`] with TX) is refused by
    /// [`Config::device`] with the other problems.
    pub fn receive_buffer_ns(mut self, ns: u64) -> Self {
        self.receive_buffer_ns = Some(ns);
        self
    }

    /// W, the pump's work margin, ns: how late the pump may run before its
    /// schedule starts again (dstack #1). On a device with TX it is in the
    /// TX path ([`Split::tx_handoff_ns`]); an RX-only device's W is in none
    /// of its paths. Unset: [`WORK_MARGIN_NS`] with TX (giving way to P's TX
    /// floor as up to v0.6.105, [`split`]), [`RX_WORK_MARGIN_NS`] RX only. One under the block and 2 ms, or over
    /// `dstack::stream::MAX_WORK_MARGIN`, is refused by [`Config::device`].
    pub fn work_margin_ns(mut self, ns: u64) -> Self {
        self.work_margin_ns = Some(ns);
        self
    }

    /// An app on its own clock that bridges through [`bridge::Player`]
    /// (dstack #247): its fixed chunk, ns (the frames it hands or takes at
    /// once, [`bridge::chunk_ns`]: OBS 1024 frames at 48 kHz, an ASIO
    /// bridge its ASIO buffer, dsend and dlisten their WASAPI period), and
    /// the app buffer it configured, if any. The FIFO's target is that value,
    /// else the chunk and one pump block ([`bridge::AppBuffer`]); the device
    /// logs it at its start beside the other terms, and refuses one outside
    /// [`bridge::MIN_APP_BUFFER_NS`] to [`bridge::MAX_APP_BUFFER_NS`].
    pub fn app_buffer(mut self, chunk_ns: u64, set_ns: Option<u64>) -> Self {
        self.app_buffer = Some((chunk_ns, set_ns));
        self
    }

    /// The bridge FIFO's target of this device ([`Config::app_buffer`]),
    /// its pump block at the device's rate; `None` for an app that stated
    /// none (it does not bridge). Pass it to [`bridge::Player::new`].
    pub fn bridge_buffer(&self) -> Option<bridge::AppBuffer> {
        let (chunk, set) = self.app_buffer?;
        let block_ns = bridge::chunk_ns(self.block, self.device_rate());
        Some(bridge::AppBuffer::new(chunk, block_ns).set(set))
    }

    /// The latency terms this config sets ([`Terms`]).
    pub fn terms(&self) -> Terms {
        Terms {
            network_latency_ns: self.network_latency_ns,
            receive_buffer_ns: self.receive_buffer_ns,
            work_margin_ns: self.work_margin_ns,
        }
    }

    /// The directions the device has: RX with RX channels, TX with TX
    /// channels ([`Directions::of`]).
    pub fn directions(&self) -> Directions {
        Directions::of(!self.rx.is_empty(), !self.tx.is_empty())
    }

    /// This config's latency in named terms ([`split`]) for the directions
    /// it has ([`Config::directions`]): the Dante latency and its two
    /// terms, the app hand-off, the pump's margin and each direction's
    /// path.
    pub fn split(&self) -> Result<Split, String> {
        split(
            self.terms(),
            self.device_rate(),
            self.sample_rate,
            self.block,
            self.directions(),
        )
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

    /// At most `n` TX flows and `n` RX flows at once (1 to 32; 32 unless
    /// set): the device opens a socket for each, so an app that needs one
    /// or two flows holds few sockets.
    pub fn flow_slots(mut self, n: usize) -> Self {
        self.flow_slots = Some(n);
        self
    }

    /// The pump's block, frames at the device's rate: one of
    /// `dstack::stream::BLOCKS` (16, 32, 64, 128 or 256; [`BLOCK`] unless
    /// set). Another is refused by [`Config::device`] with the other
    /// problems. A smaller block wakes the pump more often; a larger one
    /// adds to the path's latency.
    pub fn block(mut self, frames: u64) -> Self {
        self.block = frames;
        self
    }

    /// The host's kernel send path for the device's TX (dstack #1, Windows
    /// only): dstack writes whole Ethernet II frames and hands each TX pass's
    /// frames to `sender` in one call (`dstack::media::bypass::FrameSender`).
    /// Without one (and off Windows) TX goes through the UDP sockets.
    pub fn frame_sender(mut self, sender: Arc<dyn FrameSender>) -> Self {
        self.frame_sender = Sender(Some(sender));
        self
    }

    /// The device config dstack runs: the normal timing profile, the
    /// app's block ([`BLOCK`] unless set), the Dante latency P and the
    /// pump's margin W from the config's [`split`], 24-bit. Every problem is
    /// listed, as a config file's would be (a latency under its floor
    /// first).
    pub fn device(&self) -> Result<DeviceConfig, Vec<String>> {
        self.checked().map(|(cfg, _)| cfg)
    }

    /// [`Config::device`] and the [`split`] it runs, one computation.
    fn checked(&self) -> Result<(DeviceConfig, Split), Vec<String>> {
        let mut t = toml::Table::new();
        t.insert("name".into(), self.name.clone().into());
        t.insert("manufacturer".into(), self.manufacturer.clone().into());
        t.insert("model".into(), self.model.clone().into());
        let rate = self.device_rate();
        if !APP_RATES.contains(&self.sample_rate) {
            return Err(vec![format!(
                "sample_rate {} must be between {} and {} Hz",
                self.sample_rate,
                APP_RATES.start(),
                APP_RATES.end()
            )]);
        }
        t.insert("sample_rate".into(), i64::from(rate).into());
        t.insert("encoding".into(), 24.into());
        // A latency under its floor (or a margin out of range) is listed
        // first; the device is checked at the floor and the default margin
        // meanwhile, for the other problems.
        let split = self.split();
        let (latency_ns, margin) = match &split {
            Ok(s) => (s.latency_ns, s.margin),
            Err(_) => (
                MIN_LATENCY_NS,
                default_work_margin(rate, self.block, self.directions()),
            ),
        };
        t.insert("latency_ns".into(), to_int(latency_ns).into());
        t.insert("block".into(), to_int(self.block).into());
        t.insert("work_margin".into(), to_int(margin).into());
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
        if let Some(n) = self.flow_slots {
            t.insert(
                "flow_slots".into(),
                to_int(u64::try_from(n).unwrap_or(u64::MAX)).into(),
            );
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
        let checked = match (split, DeviceConfig::from_toml(&text)) {
            (Ok(s), Ok(cfg)) => {
                debug_assert_eq!(cfg.timing, TimingProfile::Normal);
                Ok((cfg, s))
            }
            (Ok(_), Err(e)) => Err(e.problems),
            (Err(p), Ok(_)) => Err(vec![p]),
            // A device rate of 0 is the device's own sample_rate problem:
            // listed once, as the device words it.
            (Err(_), Err(e)) if rate == 0 => Err(e.problems),
            (Err(p), Err(e)) => Err(std::iter::once(p).chain(e.problems).collect()),
        };
        match (checked, self.bridge_buffer().and_then(|b| b.problem())) {
            (checked, None) => checked,
            (Ok(_), Some(p)) => Err(vec![p]),
            (Err(mut problems), Some(p)) => {
                problems.push(p);
                Err(problems)
            }
        }
    }
}

fn to_int(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// The device running inside the app.
pub struct Link {
    device: Embedded,
}

/// Waits until `place` is there (the address on an interface, or the
/// interface with its one IPv4 address), as [`Link::start_until`] does but
/// with no stop (dstack #225), for an app that uses the address before the
/// start: dsend and dlisten probe their free control ports on it, then
/// call [`Link::start`]. A place refused (an address the Dante network
/// cannot reach, an interface with several IPv4 addresses, an interface
/// list that cannot be read) is [`HostError::Run`] at once.
pub fn wait_for_place(place: &Place) -> Result<(), HostError> {
    dstack::daemon::wait_for_interface(place, dstack::host::interfaces, std::thread::sleep)
        .map(drop)
        .map_err(HostError::Run)
}

/// The default route's address as the device's place, for an app given no
/// address or interface (dsend, dlisten), waited for while the box has no
/// default route yet (at boot; dstack #230), with no time limit and the
/// same log lines as [`wait_for_place`]. A socket that cannot be opened and
/// an IPv6 route are [`HostError::Run`] at once.
pub fn default_route_place() -> Result<Place, HostError> {
    dstack::daemon::wait_for_route(dstack::host::default_route, std::thread::sleep)
        .map(Place::Ip)
        .map_err(HostError::Run)
}

impl Link {
    /// Starts the device of `config` on `place`, running `processor` every
    /// block on the RX channels `map.rx` into the TX channels `map.tx`.
    /// Its media clock is dsync's in-process follower when it is locked
    /// and the PC's dsync service otherwise (the owner's rule, dstack #68:
    /// a clock with or without the service, never worse than it). Call it
    /// outside any tokio runtime: the device runs its own.
    ///
    /// Every error returns at once, an address or interface not there yet
    /// too ([`HostError::Run`], "no interface of this box holds the
    /// address" or "no such interface, or it has no IPv4 address"; dstack
    /// #230, as up to v0.6.97): an app that retries on its own calls it
    /// again later. To wait for the place instead (the network still
    /// coming up at boot or logon), call [`Link::start_until`].
    pub fn start<P: Process + 'static>(
        config: &Config,
        place: &Place,
        map: Map,
        processor: P,
    ) -> Result<Link, HostError> {
        Self::start_on(config, place, map, processor, ClockSource::Linked)
    }

    /// [`Link::start`] that waits for an address or interface not there
    /// yet (the network still coming up at boot or logon, dstack #225,
    /// #230) until `stop` is set: it looks the place up every second and
    /// logs the wait at its start, once a minute and when the place is
    /// there. Once `stop` is set, the call returns within one second with
    /// [`HostError::Run`] "stopped while waiting for the device's address"
    /// (or "... interface"); the app knows the stop is its own. A place
    /// already there starts the device even with `stop` set. Every other
    /// error returns at once, as from [`Link::start`].
    ///
    /// Call it on the app's own thread (never a UI thread: it blocks while
    /// it waits), and set `stop` at shutdown or before a rebuild.
    pub fn start_until<P: Process + 'static>(
        config: &Config,
        place: &Place,
        map: Map,
        processor: P,
        stop: &AtomicBool,
    ) -> Result<Link, HostError> {
        Self::start_placed(
            config,
            place,
            PlaceWait::Until(stop),
            map,
            processor,
            ClockSource::Linked,
        )
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
        Self::start_placed(config, place, PlaceWait::Now, map, processor, source)
    }

    /// The start, a place not there yet met as `wait` says.
    fn start_placed<P: Process + 'static>(
        config: &Config,
        place: &Place,
        wait: PlaceWait<'_>,
        map: Map,
        processor: P,
        source: ClockSource,
    ) -> Result<Link, HostError> {
        let (cfg, s) = config.checked().map_err(|problems| {
            HostError::Setup(format!("dlink config: {}", problems.join("; ")))
        })?;
        let sender = config.frame_sender.0.clone();
        for line in s.lines(config.directions(), config.terms()) {
            tracing::info!(device = %config.name, "dlink latency: {line}");
        }
        if let Some(buffer) = config.bridge_buffer() {
            tracing::info!(device = %config.name, "dlink latency: {}", buffer.line());
            if let Some(short) = buffer.short() {
                tracing::warn!(device = %config.name, "dlink: {short}");
            }
        }
        let device = if config.converts() {
            tracing::info!(
                device_rate = config.device_rate(),
                app_rate = config.sample_rate,
                "dlink: the device runs at the network's rate, the app at its own (converted on the device's clock)"
            );
            let (inputs, outputs) = (map.rx.len(), map.tx.len());
            let block = usize::try_from(cfg.block).unwrap_or(usize::MAX);
            let rated = Rated::new(
                processor,
                config.device_rate(),
                config.sample_rate,
                block,
                inputs,
                outputs,
            )
            .map_err(|e| HostError::Setup(format!("dlink rate conversion: {e}")))?;
            Embedded::start_waiting(cfg, place, wait, map, rated, source, sender)?
        } else {
            Embedded::start_waiting(cfg, place, wait, map, processor, source, sender)?
        };
        Ok(Link { device })
    }

    /// The device's media side: the pump's and the flows' counts.
    pub fn media(&self) -> &Arc<Media> {
        self.device.media()
    }

    /// Each RX channel's subscription, status, flow and counts
    /// ([`Media::rx_channels`]): poll it to tell a lost flow from a quiet
    /// source (a `written` count that stops while the status reads
    /// receiving).
    pub fn rx_channels(&self) -> Vec<RxChannel> {
        self.device.media().rx_channels()
    }

    /// The device's numbers as one JSON line: the clock, the flows, the
    /// pump, each channel (what `dstack-dev ctl status` prints). An app reads
    /// it here, without a dependency on `dstack` (dstack #102, #104).
    pub fn status(&self) -> String {
        dstack::ctl::status(self.device.media())
    }

    /// Sets the app's own part of [`Link::status`] (dstack #104): the
    /// status carries what `f` returns as `app` when it is one JSON object
    /// of at most 4096 bytes ([`Media::set_app_status`]). dtray shows its
    /// `line` (a short text) after the device's line and turns orange on
    /// `"state":"warn"`, red on `"state":"error"`. A later call replaces the
    /// hook.
    ///
    /// `f` runs on a thread of its own, at most once a second and only
    /// while the status is read; the status carries its last answer, with
    /// `"stale":true` once that is over 3 s old (dtray: orange). A slow `f`
    /// never delays the device. Never call [`Link::status`] from `f`: each
    /// call counts as a read and keeps `f` called for good.
    pub fn set_app_status(&self, f: impl Fn() -> String + Send + Sync + 'static) {
        self.device.media().set_app_status(f);
    }

    /// Sets the app's command hook (dstack #139): a request `app <words>` on
    /// the device's local endpoint (dtray's menu) or its control socket
    /// calls `f` with `<words>` and answers `ok` for `Ok("")`, the text for
    /// another `Ok`, `error: <reason>` for an `Err`
    /// ([`Media::set_app_command`]). Without a hook the answer is `error: no
    /// app command`. A later call replaces the hook.
    ///
    /// The command is the app's own (dlisten's output: `output <n>`); it
    /// changes nothing of the device. `f` runs on the device's control
    /// task: answer at once, and hand longer work to the app's own thread.
    pub fn set_app_command(
        &self,
        f: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static,
    ) {
        self.device.media().set_app_command(f);
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
