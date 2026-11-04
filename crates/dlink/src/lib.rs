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
//! within the process's normal class (no privilege, nothing process-wide)
//! and opt out of power throttling (our threads only); elsewhere they run
//! at normal priority. Very low latency is a different situation, for
//! iemmixer and dasio, which use `dstack` directly with its `low_latency`
//! profile.
//!
//! # The latency budget
//!
//! The app's latency is 10 ms ([`BUDGET_NS`], [`Config::budget_ns`] sets
//! another) for each direction's whole path, the work and the Dante
//! latency together (owner, ROZHODNUTÉ 3.11.2026, dstack #68). dlink
//! chooses the split ([`split`]):
//!
//! - B, the pump's block: 128 frames unless the app sets another
//!   ([`Config::block`]; dstack #90: a busy video PC preempted the pump for
//!   several ms at block 32 and normal priority).
//! - W, the pump's work margin: how late the pump may run. A pump more
//!   than W behind a block starts its schedule again, and the skipped
//!   blocks are lost on both sides (`dstack::stream::Blocks`; dstack #1:
//!   2 ms restarted on a loaded PC). W is 2/5 of the budget (4 ms of 10 ms,
//!   8 ms of 20 ms; [`work_margin`]), at least the block and 2 ms. On a
//!   device with TX, W is in the TX path and gives way to keep P's floor
//!   (with a rate converter); an RX-only device keeps the whole share, at
//!   no cost to its path (a late block only reaches the app later).
//! - The rate converter's delay each way when the app's rate differs from
//!   the network's ([`Rated`], [`rated::delay_ns`]).
//! - P, the device's Dante latency: what the work of the directions the
//!   device has leaves of the budget (the larger direction's when it has
//!   both; [`Directions`]), in whole samples, at least [`MIN_LATENCY_NS`]
//!   (4 ms). The device's RX plays out at P and its TX channels advertise
//!   it.
//!
//! TX (the app's output to a receiver's playout) is W + P, plus the
//! converter's TX delay; RX (a sample's network stamp to the app's block)
//! is P + B, plus the converter's RX delay. A receiver set to a longer
//! latency than P adds the difference; a latency Dante Controller sets on
//! the device replaces P (it is kept in the state file). At the default
//! 10 ms and block 128, a device with both directions (and a TX-only one,
//! whose TX work is the larger in every row):
//!
//! | Device | App | B | W | Converter RX / TX | P | TX path | RX path |
//! |---|---|---|---|---|---|---|---|
//! | 48 kHz | 48 kHz | 2.67 ms | 4 ms | none | 6 ms | 10 ms | 8.67 ms |
//! | 96 kHz | 96 kHz | 1.33 ms | 4 ms | none | 6 ms | 10 ms | 7.33 ms |
//! | 96 kHz | 48 kHz | 1.33 ms | 3.16 ms | 1.33 / 2.83 ms | 4.01 ms | 10 ms | 6.68 ms |
//! | 96 kHz | 44.1 kHz | 1.33 ms | 2.91 ms | 1.32 / 3.08 ms | 4.01 ms | 9.99 ms | 6.66 ms |
//! | 48 kHz | 44.1 kHz | 2.67 ms | 2.90 ms | 2.65 / 3.08 ms | 4.02 ms | 9.99 ms | 9.34 ms |
//! | 48 kHz | 96 kHz | 2.67 ms | 4 ms | 2.67 / 1.42 ms | 4.58 ms | 10 ms | 9.92 ms |
//!
//! A device with RX channels only sizes P by its RX work alone, so its RX
//! path takes the whole budget (dstack #1), and W keeps its 4 ms share:
//!
//! | Device | App | W | P | RX path |
//! |---|---|---|---|---|
//! | 48 kHz | 48 kHz | 4 ms | 7.33 ms | 10 ms |
//! | 96 kHz | 96 kHz | 4 ms | 8.67 ms | 10 ms |
//! | 96 kHz | 48 kHz | 4 ms | 7.33 ms | 10 ms |
//! | 96 kHz | 44.1 kHz | 4 ms | 7.34 ms | 9.99 ms |
//! | 48 kHz | 44.1 kHz | 4 ms | 4.67 ms | 9.99 ms |
//! | 48 kHz | 96 kHz | 4 ms | 4.67 ms | 10 ms |
//!
//! A larger budget grows W and P together: at 20 ms a 48 kHz or 96 kHz
//! device gets W 8 ms and P 12 ms, a 48 kHz app on a 96 kHz device W 8 ms
//! and P 9.17 ms (17.33 ms RX only).
//!
//! A larger block with a conversion can leave under 4 ms (block 256 on a
//! 48 kHz device with a 44.1 kHz app); [`Config::device`] refuses that
//! budget with the other problems. Our TX looped into our own RX takes
//! the two paths less the one P they share (W + B + P, plus both converter
//! delays): the whole-device test measures it.
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
pub use dstack::daemon::media::{Media, RxChannel};
pub use dstack::device::DeviceConfig;
pub use dstack::host::{HostError, Stopped};
pub use dstack::stream::Map;
pub use dstack::subscribe::{SubState, SubStatus};
pub use dstack_audio_io::{Block, Process};

pub mod rated;

pub use rated::Rated;

use dstack::clock::latency_samples;
use dstack::device::{NORMAL_MIN_LATENCY_NS, NORMAL_MIN_WORK_MARGIN_NS, TimingProfile};
use dstack::embed::Embedded;
use dstack::host::ClockSource;
use dstack::media::bypass::FrameSender;
use dstack::stream::{BLOCKS, MAX_WORK_MARGIN};

/// dlink's latency budget unless the app sets one, ns: 10 ms for each
/// direction's whole path, the work (block, margin, rate conversion) and
/// the Dante latency together (owner, ROZHODNUTÉ 3.11.2026, dstack #68).
/// [`split`] gives the Dante latency what the work leaves.
pub const BUDGET_NS: u64 = 10_000_000;
/// The least Dante latency [`split`] leaves, ns: the normal profile's own
/// floor (`dstack::device::NORMAL_MIN_LATENCY_NS`, 4 ms), which the device
/// refuses to go under.
pub const MIN_LATENCY_NS: u64 = NORMAL_MIN_LATENCY_NS;
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
    budget_ns: u64,
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
            budget_ns: BUDGET_NS,
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

    /// The latency budget, ns ([`BUDGET_NS`] unless set): each direction's
    /// whole path, the work and the Dante latency together. The Dante
    /// latency is what the work leaves ([`split`]); a budget that leaves
    /// under [`MIN_LATENCY_NS`] is refused by [`Config::device`] with the
    /// other problems.
    pub fn budget_ns(mut self, ns: u64) -> Self {
        self.budget_ns = ns;
        self
    }

    /// The directions the device has: RX with RX channels, TX with TX
    /// channels ([`Directions::of`]).
    pub fn directions(&self) -> Directions {
        Directions::of(!self.rx.is_empty(), !self.tx.is_empty())
    }

    /// How this config splits its budget ([`split`]) for the directions it
    /// has ([`Config::directions`]): the Dante latency, the pump's margin
    /// and each direction's path.
    pub fn split(&self) -> Result<Split, String> {
        split(
            self.budget_ns,
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
    /// app's block ([`BLOCK`] unless set), the Dante latency and the pump's
    /// margin from the budget's [`split`], 24-bit. Every problem is listed,
    /// as a config file's would be (a budget the work leaves too little of
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
        // A budget the work leaves too little of is listed first; the
        // device is checked at the floor meanwhile, for the other problems.
        let split = self.split();
        let (latency_ns, margin) = match &split {
            Ok(s) => (s.latency_ns, s.margin),
            Err(_) => (
                MIN_LATENCY_NS,
                work_margin(self.budget_ns, rate, self.block),
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

/// The pump's share of the budget: W is [`WORK_SHARE_NUM`] /
/// [`WORK_SHARE_DEN`] of it (4 ms of 10 ms; the owner's 48 kHz example, dstack
/// #1: about 4 ms of pump margin and 6 ms of Dante latency).
pub const WORK_SHARE_NUM: u64 = 2;
/// See [`WORK_SHARE_NUM`].
pub const WORK_SHARE_DEN: u64 = 5;

/// The pump's margin W before the TX cap of [`split`], samples at `rate`:
/// [`WORK_SHARE_NUM`] / [`WORK_SHARE_DEN`] of `budget_ns` (so it grows with
/// the budget), at least [`least_work_margin`], at most
/// `dstack::stream::MAX_WORK_MARGIN`. W is how late the pump may run: a pump
/// later than W behind a block starts its schedule again
/// (`dstack::stream::Blocks`), and the skipped blocks are lost on both
/// sides (dstack #1, intercom: 2 ms restarted on a loaded PC). A block
/// outside `stream::BLOCKS` is the block's problem alone: W follows
/// [`BLOCK`] then.
pub fn work_margin(budget_ns: u64, rate: u32, block: u64) -> u64 {
    let share = budget_ns / WORK_SHARE_DEN * WORK_SHARE_NUM;
    latency_samples(share, rate)
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

/// The directions a device has, which size its Dante latency ([`split`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Directions {
    /// RX and TX (or no channels at all): the larger direction's work.
    Both,
    /// RX only (no TX channels): the RX work alone.
    Rx,
    /// TX only (no RX channels): the TX work alone.
    Tx,
}

impl Directions {
    /// The directions of a device with RX channels (`rx`) and TX channels
    /// (`tx`); one with neither splits as [`Directions::Both`].
    pub fn of(rx: bool, tx: bool) -> Self {
        match (rx, tx) {
            (true, false) => Self::Rx,
            (false, true) => Self::Tx,
            _ => Self::Both,
        }
    }
}

/// How a latency budget splits ([`split`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Split {
    /// P, the device's Dante latency, ns: whole samples at the device's
    /// rate. Its RX plays out at it and its TX channels advertise it.
    pub latency_ns: u64,
    /// W, the pump's work margin, samples at the device's rate: how late
    /// its pump may run before it starts again ([`work_margin`], given way
    /// to P's floor on a device with TX).
    pub margin: u64,
    /// The app's output to a receiver's playout, ns: W + P, plus the rate
    /// converter's TX delay when it runs. A receiver set to a longer
    /// latency than P adds the difference. An RX-only device has no TX
    /// path: the figure may then pass the budget.
    pub tx_path_ns: u64,
    /// A sample's network stamp to the app's block, ns: P + B, plus the
    /// rate converter's RX delay when it runs. A TX-only device has no RX
    /// path: the figure may then pass the budget.
    pub rx_path_ns: u64,
}

/// Splits a latency budget (dstack #68, the owner's ROZHODNUTÉ of
/// 3.11.2026): `budget_ns` is each direction's whole path. The pump's
/// block B (`block` device frames; [`BLOCK`] when it is not one of
/// `stream::BLOCKS`), its margin W ([`work_margin`]: 2/5 of the budget;
/// with TX, no more than leaves P its floor after the converter's TX delay,
/// never under the block and 2 ms; dstack #1) and the rate converter's
/// delays ([`rated::delay_ns`], none when `app_rate` is
/// `device_rate`) are the work; the Dante latency P is the budget less the
/// work of the device's `directions` (TX: W + the converter's TX delay; RX:
/// B + its RX delay; the larger of the two for [`Directions::Both`]), in
/// whole samples at `device_rate` (dstack #1: an RX-only device no longer
/// pays for a TX path it never runs).
///
/// The path each way (`dstack::stream`: block k runs once media time
/// passes kB + B + P, and its output goes out stamped kB + B + P + W): TX
/// W + P, RX P + B. A budget that leaves P under [`MIN_LATENCY_NS`], a
/// device rate of 0 and a rate pair the converter refuses are problems.
pub fn split(
    budget_ns: u64,
    device_rate: u32,
    app_rate: u32,
    block: u64,
    directions: Directions,
) -> Result<Split, String> {
    if device_rate == 0 {
        return Err("sample_rate: the device's rate is 0 Hz".to_string());
    }
    let b = valid_block(block);
    let (convert_rx, convert_tx) = rated::delay_ns(
        device_rate,
        app_rate,
        usize::try_from(b).unwrap_or(usize::MAX),
    )
    .map_err(|e| format!("rate conversion: {e}"))?;
    let rate = u128::from(device_rate);
    let ns = |samples: u64| (u128::from(samples) * 1_000_000_000 / rate) as u64;
    let samples_in = |ns: u64| (u128::from(ns) * rate / 1_000_000_000) as u64;
    // W is in the TX path: a device with TX keeps P's floor, and W gives
    // way, down to its own least. The room's last sample is left to P, so
    // P's whole samples still reach the floor at any rate. An RX-only
    // device's W is not in its path (a late block only comes later): it
    // keeps the whole share.
    let mut margin = work_margin(budget_ns, device_rate, b);
    if directions != Directions::Rx {
        let room = budget_ns.saturating_sub(convert_tx + MIN_LATENCY_NS);
        margin = margin
            .min(samples_in(room).saturating_sub(1))
            .max(least_work_margin(device_rate, b));
    }
    let tx_work = ns(margin) + convert_tx;
    let rx_work = ns(b) + convert_rx;
    let work = match directions {
        Directions::Both => tx_work.max(rx_work),
        Directions::Rx => rx_work,
        Directions::Tx => tx_work,
    };
    let latency_ns = ns(samples_in(budget_ns.saturating_sub(work)));
    if latency_ns < MIN_LATENCY_NS {
        return Err(format!(
            "budget_ns: {budget_ns} leaves a Dante latency of {latency_ns} ns after \
             {work} ns of work (block, margin, rate conversion); at least {MIN_LATENCY_NS}"
        ));
    }
    Ok(Split {
        latency_ns,
        margin,
        tx_path_ns: tx_work + latency_ns,
        rx_path_ns: rx_work + latency_ns,
    })
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
        let (cfg, s) = config.checked().map_err(|problems| {
            HostError::Setup(format!("dlink config: {}", problems.join("; ")))
        })?;
        let sender = config.frame_sender.0.clone();
        tracing::info!(
            budget_ns = config.budget_ns,
            latency_ns = s.latency_ns,
            margin = s.margin,
            block = cfg.block,
            tx_path_ns = s.tx_path_ns,
            rx_path_ns = s.rx_path_ns,
            directions = ?config.directions(),
            "dlink: the latency budget's split (the Dante latency is what the work leaves)"
        );
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
            Embedded::start_with_sender(cfg, place, map, rated, source, sender)?
        } else {
            Embedded::start_with_sender(cfg, place, map, processor, source, sender)?
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
