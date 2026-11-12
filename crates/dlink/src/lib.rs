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
//! the block and 2 ms ([`least_work_margin`]).
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
//! An app that sets no receive buffer gets the P it had under the old
//! 15 ms budget ([`REFERENCE_PATH_NS`]): 15 ms less the work of the
//! directions the device has (the larger direction's when it has both),
//! at least the floor; its (b) is that P less 1 ms. Setting (a) alone moves
//! P by the difference: the terms are independent. At block 128, W 6 ms
//! with TX:
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
//! 12.041666 ms, RX path 15 ms. A config the old budget could not hold (an
//! 8 kHz app on a 96 kHz network, whose filters alone take about 19 ms)
//! now starts at P's floor. Our TX looped into our own RX takes the two
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
//! to the path above: about 40 ms, up to 80 ms after dropouts.
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

pub use rated::Rated;

use dstack::announce::FPP_MAX;
use dstack::clock::latency_samples;
use dstack::daemon::PlaceWait;
use dstack::device::{NORMAL_MIN_LATENCY_NS, NORMAL_MIN_WORK_MARGIN_NS, TimingProfile};
use dstack::embed::Embedded;
use dstack::host::ClockSource;
use dstack::media::bypass::FrameSender;
use dstack::media::tx::TX_MARGIN_NS;
use dstack::stream::{BLOCKS, MAX_WORK_MARGIN};

/// (a), the Dante network latency unless the app sets one
/// ([`Config::network_latency_ns`]), ns: 1 ms, Dante's own default for a
/// gigabit network (dstack #244).
pub const NETWORK_LATENCY_NS: u64 = 1_000_000;
/// The path each direction had up to v0.6.105 (the old budget, 15 ms), ns.
/// It sizes only the DEFAULT receive buffer (b): an app that sets no
/// [`Config::receive_buffer_ns`] gets the Dante latency it had then, so
/// nobody's latency moves (dstack #244). It is no key and no limit.
pub const REFERENCE_PATH_NS: u64 = 15_000_000;
/// W, the pump's work margin, of a device with TX unless the app sets one
/// ([`Config::work_margin_ns`]), ns: 6 ms, room for the 2.7 to 3.6 ms
/// preemptions seen on loaded PCs with no CPU pinning and no real time
/// (dstack #1; 2/5 of the old 15 ms budget, the same W as up to v0.6.105).
pub const WORK_MARGIN_NS: u64 = 6_000_000;
/// W of an RX-only device unless the app sets one, ns: 15 ms (dstack #1,
/// v0.6.32: W is in none of its paths, and OBS on a loaded PC was
/// preempted about 6 ms). At most `dstack::stream::MAX_WORK_MARGIN`.
pub const RX_WORK_MARGIN_NS: u64 = 15_000_000;
/// The least Dante latency, ns: the normal profile's own floor
/// (`dstack::device::NORMAL_MIN_LATENCY_NS`, 4 ms), which the device
/// refuses to go under.
pub const MIN_LATENCY_NS: u64 = NORMAL_MIN_LATENCY_NS;
/// How late a TX packet may leave past its due time on the normal profile,
/// ns (dstack #1, songplayer 4.11.2026): a Windows PC's TX thread at
/// THREAD_PRIORITY_HIGHEST, no real time, left a packet up to 3.71 ms past
/// due over 1 h 50 min (late_max_us 3712; of 80 million packets 301 in the
/// 2 to 5 ms bin, none at 5 ms or more), and a receiver set to our 4 ms
/// counted 137 of them late. Its TX floor ([`tx_floor_ns`]) covers it.
pub const TX_LATE_NS: u64 = 4_000_000;

/// The least Dante latency of a device with TX channels, ns: a TX packet's
/// own age when it leaves. That is the most frames a receiver may ask in a
/// packet (`dstack::announce::FPP_MAX`, our device's `tx_fpp_max`) at
/// `device_rate`, plus the device's TX margin
/// (`dstack::media::tx::TX_MARGIN_NS`), plus how late it may leave on the
/// normal profile ([`TX_LATE_NS`]). A receiver plays the packet that P
/// after its stamp, so P under it plays packets late (dstack #1). The
/// frames count in whole ns as [`split`] counts P's samples (rounded down),
/// so a P of exactly that age is taken: 4583333 ns at 96 kHz, 4916666 ns
/// at 48 kHz; over [`MIN_LATENCY_NS`] at any rate (a rate of 0 counts as
/// 1 Hz).
pub fn tx_floor_ns(device_rate: u32) -> u64 {
    let frames = u64::from(FPP_MAX) * 1_000_000_000 / u64::from(device_rate.max(1));
    frames + TX_MARGIN_NS + TX_LATE_NS
}
const _: () = assert!(TX_MARGIN_NS + TX_LATE_NS > MIN_LATENCY_NS);
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
    /// of its paths. Unset: [`WORK_MARGIN_NS`] with TX,
    /// [`RX_WORK_MARGIN_NS`] RX only. One under the block and 2 ms, or over
    /// `dstack::stream::MAX_WORK_MARGIN`, is refused by [`Config::device`].
    pub fn work_margin_ns(mut self, ns: u64) -> Self {
        self.work_margin_ns = Some(ns);
        self
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
        match (split, DeviceConfig::from_toml(&text)) {
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
        }
    }
}

/// W of a device that sets none, samples at `rate`: [`WORK_MARGIN_NS`] on a
/// device with TX, [`RX_WORK_MARGIN_NS`] on an RX-only one (dstack #1,
/// v0.6.32: W is in none of its paths: its RX path is P + B + the
/// converter's RX delay, and D = B + P + W only stamps the engine's TX
/// rings, which it does not have), at most `dstack::stream::MAX_WORK_MARGIN`
/// (the stack holds any W up to it beside any P:
/// `dstack::device::MAX_LATENCY_SAMPLES` keeps it inside half the RX rings),
/// at least [`least_work_margin`]. A block outside `stream::BLOCKS` is the
/// block's problem alone: W follows [`BLOCK`] then.
pub fn default_work_margin(rate: u32, block: u64, directions: Directions) -> u64 {
    let ns = if directions == Directions::Rx {
        RX_WORK_MARGIN_NS
    } else {
        WORK_MARGIN_NS
    };
    latency_samples(ns, rate)
        .min(MAX_WORK_MARGIN)
        .max(least_work_margin(rate, block))
}

/// The least pump margin, samples at `rate`: the block, and at least the
/// normal profile's 2 ms (`dstack::device::NORMAL_MIN_WORK_MARGIN_NS`), the
/// device's own floor.
pub fn least_work_margin(rate: u32, block: u64) -> u64 {
    valid_block(block).max(latency_samples(NORMAL_MIN_WORK_MARGIN_NS, rate))
}

/// `block` when it is one of `stream::BLOCKS`, else [`BLOCK`].
fn valid_block(block: u64) -> u64 {
    if BLOCKS.contains(&block) {
        block
    } else {
        BLOCK
    }
}

/// The directions a device has, which decide its floor, its default W
/// and its default receive buffer ([`split`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Directions {
    /// RX and TX (or no channels at all).
    Both,
    /// RX only (no TX channels).
    Rx,
    /// TX only (no RX channels).
    Tx,
}

impl Directions {
    /// The directions of a device with RX channels (`rx`) and TX channels
    /// (`tx`); one with neither counts as [`Directions::Both`].
    pub fn of(rx: bool, tx: bool) -> Self {
        match (rx, tx) {
            (true, false) => Self::Rx,
            (false, true) => Self::Tx,
            _ => Self::Both,
        }
    }

    /// Whether the device has TX channels (or none at all).
    pub fn has_tx(self) -> bool {
        self != Self::Rx
    }

    /// Whether the device has RX channels (or none at all).
    pub fn has_rx(self) -> bool {
        self != Self::Tx
    }
}

/// The latency terms an app sets (dstack #244, the owner's ROZHODNUTÉ of
/// 12.11.2026): [`Config::terms`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Terms {
    /// (a), the Dante network latency, ns ([`Config::network_latency_ns`]);
    /// `None` is [`NETWORK_LATENCY_NS`].
    pub network_latency_ns: Option<u64>,
    /// (b), the receive buffer, ns ([`Config::receive_buffer_ns`]); `None`
    /// keeps the Dante latency of v0.6.105.
    pub receive_buffer_ns: Option<u64>,
    /// W, the pump's work margin, ns ([`Config::work_margin_ns`]); `None`
    /// is [`default_work_margin`].
    pub work_margin_ns: Option<u64>,
}

impl Default for Terms {
    /// Nothing set: (a), (b) and W their defaults.
    fn default() -> Self {
        Terms {
            network_latency_ns: None,
            receive_buffer_ns: None,
            work_margin_ns: None,
        }
    }
}

/// A device's latency in named terms ([`split`]; dstack #244). Each figure
/// is ns unless named samples. The Dante latency P = (a) + (b); each path
/// is P plus that direction's hand-off (c).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Split {
    /// (a), the Dante network latency: the app's, else
    /// [`NETWORK_LATENCY_NS`].
    pub network_latency_ns: u64,
    /// (b), the receive buffer: P less (a). The app's, rounded up to make P
    /// whole samples; else the default that keeps P as in v0.6.105.
    pub receive_buffer_ns: u64,
    /// P, the device's Dante latency: (a) + (b), whole samples at the
    /// device's rate. Its RX plays out at it, measured from the sender's
    /// media-clock stamp, and its TX channels advertise it (ARC), as Dante
    /// Controller shows it.
    pub latency_ns: u64,
    /// B, the pump's block.
    pub block_ns: u64,
    /// The rate converter's delay on the RX side (0 without conversion).
    pub rx_convert_ns: u64,
    /// The rate converter's delay on the TX side (0 without conversion).
    pub tx_convert_ns: u64,
    /// (c) on the RX side, the app hand-off: B + the converter's RX delay.
    pub rx_handoff_ns: u64,
    /// (c) on the TX side, the app hand-off: W + the converter's TX delay.
    pub tx_handoff_ns: u64,
    /// W, the pump's work margin, samples at the device's rate: how late
    /// its pump may run before it starts again.
    pub margin: u64,
    /// W in ns, whole samples.
    pub work_margin_ns: u64,
    /// The app's output to a receiver's playout: (c) TX + P. A receiver set
    /// to a longer latency than P adds the difference. An RX-only device
    /// has no TX path: the figure then holds its W, which is in no path.
    pub tx_path_ns: u64,
    /// A sample's network stamp to the app's block: P + (c) RX. A TX-only
    /// device has no RX path: the figure is only the arithmetic.
    pub rx_path_ns: u64,
}

impl Split {
    /// The terms as the device logs them at its start, one line each
    /// (dstack #244: never one lump): the Dante latency with (a) and (b),
    /// then the RX hand-off when the device has RX channels and the TX
    /// hand-off with W when it has TX channels. `set` says which terms the
    /// app set ((a), (b), W); the others read "default".
    pub fn lines(&self, directions: Directions, set: Terms) -> Vec<String> {
        let from = |given: bool| if given { "set" } else { "default" };
        let mut lines = vec![format!(
            "Dante latency P {} ms = network latency (a) {} ms ({}) + receive buffer (b) {} ms ({})",
            ms(self.latency_ns),
            ms(self.network_latency_ns),
            from(set.network_latency_ns.is_some()),
            ms(self.receive_buffer_ns),
            from(set.receive_buffer_ns.is_some()),
        )];
        if directions.has_rx() {
            lines.push(format!(
                "RX app hand-off (c) {} ms = block {} ms + converter {} ms; RX path P + (c) {} ms",
                ms(self.rx_handoff_ns),
                ms(self.block_ns),
                ms(self.rx_convert_ns),
                ms(self.rx_path_ns),
            ));
        }
        if directions.has_tx() {
            lines.push(format!(
                "TX app hand-off (c) {} ms = work margin W {} ms ({}) + converter {} ms; TX path (c) + P {} ms",
                ms(self.tx_handoff_ns),
                ms(self.work_margin_ns),
                from(set.work_margin_ns.is_some()),
                ms(self.tx_convert_ns),
                ms(self.tx_path_ns),
            ));
        }
        lines
    }
}

/// `ns` as ms with up to six decimals, trailing zeros dropped
/// (13041666 -> "13.041666", 1000000 -> "1").
pub fn ms(ns: u64) -> String {
    let (whole, frac) = (ns / 1_000_000, ns % 1_000_000);
    if frac == 0 {
        return whole.to_string();
    }
    let digits = format!("{frac:06}");
    format!("{whole}.{}", digits.trim_end_matches('0'))
}

/// A device's latency in named terms (dstack #244, the owner's ROZHODNUTÉ
/// of 12.11.2026, replacing the single budget of dstack #68):
///
/// - (a) the Dante network latency, `terms.network_latency_ns`;
/// - (b) the receive buffer, `terms.receive_buffer_ns`: this PC's room for
///   the RX thread's wake and pass. Unset, it is what keeps P as it was up
///   to v0.6.105: P at [`REFERENCE_PATH_NS`] less the work of the device's
///   `directions` (TX: the default W + the converter's TX delay; RX: B +
///   its RX delay; the larger for [`Directions::Both`]), at least the
///   floor, less [`NETWORK_LATENCY_NS`];
/// - P = (a) + (b), rounded up to whole samples at `device_rate`;
/// - (c) the app hand-off, computed: RX B + the converter's RX delay, TX
///   W + its TX delay ([`rated::delay_ns`], none when `app_rate` is
///   `device_rate`); W is `terms.work_margin_ns` or [`default_work_margin`].
///
/// The block B is `block` device frames ([`BLOCK`] when it is not one of
/// `stream::BLOCKS`). P's floor is [`tx_floor_ns`] on a device with TX (a
/// TX packet's own age) and [`MIN_LATENCY_NS`] RX only. A P under its
/// floor, a W out of its range, a device rate of 0 and a rate pair the
/// converter refuses are problems.
pub fn split(
    terms: Terms,
    device_rate: u32,
    app_rate: u32,
    block: u64,
    directions: Directions,
) -> Result<Split, String> {
    if device_rate == 0 {
        return Err("sample_rate: the device's rate is 0 Hz".to_string());
    }
    let b = valid_block(block);
    let (rx_convert_ns, tx_convert_ns) = rated::delay_ns(
        device_rate,
        app_rate,
        usize::try_from(b).unwrap_or(usize::MAX),
    )
    .map_err(|e| format!("rate conversion: {e}"))?;
    let rate = u128::from(device_rate);
    let ns = |samples: u64| {
        u64::try_from(u128::from(samples) * 1_000_000_000 / rate).unwrap_or(u64::MAX)
    };
    let default_margin = default_work_margin(device_rate, b, directions);
    let margin = match terms.work_margin_ns {
        None => default_margin,
        Some(w) => {
            let (least, most) = (least_work_margin(device_rate, b), MAX_WORK_MARGIN);
            let m = latency_samples(w, device_rate);
            if !(least..=most).contains(&m) {
                return Err(format!(
                    "work_margin_ns: {w} is {m} samples at {device_rate} Hz; \
                     {least} to {most} (the block and 2 ms, the stack's largest margin)"
                ));
            }
            m
        }
    };
    let block_ns = ns(b);
    let rx_handoff_ns = block_ns + rx_convert_ns;
    let tx_handoff_ns = ns(margin) + tx_convert_ns;
    let (floor, why) = if directions == Directions::Rx {
        (MIN_LATENCY_NS, "the normal profile's floor")
    } else {
        (
            tx_floor_ns(device_rate),
            "a TX packet's own age, dlink::tx_floor_ns",
        )
    };
    let a = terms.network_latency_ns.unwrap_or(NETWORK_LATENCY_NS);
    let receive_buffer = match terms.receive_buffer_ns {
        Some(rb) => rb,
        None => {
            // P as up to v0.6.105: the reference path less the work of the
            // device's directions at the default W, whole samples (rounded
            // down, as the budget's split did), at least the floor.
            let tx_work = ns(default_margin) + tx_convert_ns;
            let work = match directions {
                Directions::Both => tx_work.max(rx_handoff_ns),
                Directions::Rx => rx_handoff_ns,
                Directions::Tx => tx_work,
            };
            let reference = ns(
                (u128::from(REFERENCE_PATH_NS.saturating_sub(work)) * rate / 1_000_000_000) as u64
            );
            reference.max(floor).saturating_sub(NETWORK_LATENCY_NS)
        }
    };
    let wanted = a.saturating_add(receive_buffer);
    // P rounded up to whole samples: never under what the terms ask.
    let samples = (u128::from(wanted) * rate).div_ceil(1_000_000_000);
    let latency_ns = ns(u64::try_from(samples).unwrap_or(u64::MAX));
    if latency_ns < floor {
        return Err(format!(
            "network_latency_ns + receive_buffer_ns: {a} + {receive_buffer} = {wanted} ns \
             of Dante latency; at least {floor} ({why})"
        ));
    }
    Ok(Split {
        network_latency_ns: a,
        receive_buffer_ns: latency_ns - a,
        latency_ns,
        block_ns,
        rx_convert_ns,
        tx_convert_ns,
        rx_handoff_ns,
        tx_handoff_ns,
        margin,
        work_margin_ns: ns(margin),
        tx_path_ns: tx_handoff_ns.saturating_add(latency_ns),
        rx_path_ns: rx_handoff_ns.saturating_add(latency_ns),
    })
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
