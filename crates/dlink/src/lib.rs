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
//! The app's latency is 15 ms ([`BUDGET_NS`], [`Config::budget_ns`] sets
//! another) for each direction's whole path, the work and the Dante
//! latency together (owner, ROZHODNUTÉ 3.11.2026, dstack #68: 10 ms;
//! ROZHODNUTÉ 4.11.2026, dstack #1: 15 ms, 10 ms was never meant to push
//! dlink apps low). dlink chooses the split ([`split`]):
//!
//! - B, the pump's block: 128 frames unless the app sets another
//!   ([`Config::block`]; dstack #90: a busy video PC preempted the pump for
//!   several ms at block 32 and normal priority).
//! - W, the pump's work margin: how late the pump may run. A pump more
//!   than W behind a block starts its schedule again, and the skipped
//!   blocks are lost on both sides (`dstack::stream::Blocks`; dstack #1:
//!   2 ms restarted on a loaded PC). W is 2/5 of the budget (6 ms of 15 ms,
//!   4 ms of 10 ms; [`work_margin`]), at least the block and 2 ms: at the
//!   default, room for the 2.7 to 3.6 ms preemptions seen on loaded PCs,
//!   where a dlink app's threads run unpinned and without real time. On a
//!   device with TX, W is in the TX path and gives way to keep P's TX
//!   floor when a smaller budget leaves no room for both. An RX-only
//!   device's W is in none of its paths (a late block only reaches the app
//!   later), so it is the whole budget ([`rx_work_margin`]; 15 ms at the
//!   default, v0.6.32, dstack #1: OBS on a loaded PC was preempted about
//!   6 ms and restarted at W 6 ms), at no cost to its latency.
//! - The rate converter's delay each way when the app's rate differs from
//!   the network's ([`Rated`], [`rated::delay_ns`]).
//! - P, the device's Dante latency: what the work of the directions the
//!   device has leaves of the budget (the larger direction's when it has
//!   both; [`Directions`]), in whole samples. Its floor is
//!   [`tx_floor_ns`] on a device with TX channels: a TX packet's own age
//!   when it leaves (the most frames in a packet + the TX margin + 4 ms of
//!   lateness, 4.58 ms at 96 kHz, 4.92 ms at 48 kHz; dstack #1), and
//!   [`MIN_LATENCY_NS`] (4 ms) on an RX-only device. The device's RX plays
//!   out at P and its TX channels advertise it.
//!
//! TX (the app's output to a receiver's playout) is W + P, plus the
//! converter's TX delay; RX (a sample's network stamp to the app's block)
//! is P + B, plus the converter's RX delay. A receiver set to a longer
//! latency than P adds the difference; a latency Dante Controller sets on
//! the device replaces P (it is kept in the state file). At the default
//! 15 ms and block 128, a device with both directions (and a TX-only one,
//! whose TX work is the larger in every row), W 6 ms in every row:
//!
//! | Device | App | B | Converter RX / TX | P | TX path | RX path |
//! |---|---|---|---|---|---|---|
//! | 48 kHz | 48 kHz | 2.67 ms | none | 9 ms | 15 ms | 11.67 ms |
//! | 96 kHz | 96 kHz | 1.33 ms | none | 9 ms | 15 ms | 10.33 ms |
//! | 96 kHz | 48 kHz | 1.33 ms | 0.63 / 0.64 ms | 8.36 ms | 15 ms | 10.32 ms |
//! | 96 kHz | 44.1 kHz | 1.33 ms | 1.32 / 3.08 ms | 5.92 ms | 14.99 ms | 8.57 ms |
//! | 48 kHz | 44.1 kHz | 2.67 ms | 2.65 / 3.08 ms | 5.92 ms | 14.99 ms | 11.24 ms |
//! | 48 kHz | 96 kHz | 2.67 ms | 0.64 / 0.63 ms | 8.38 ms | 15 ms | 11.68 ms |
//!
//! A device with RX channels only sizes P by its RX work alone, so its RX
//! path takes the whole budget (dstack #1), and its W is the whole 15 ms
//! (1440 samples at 96 kHz, 720 at 48 kHz; v0.6.32, 6 ms before):
//!
//! | Device | App | P | RX path |
//! |---|---|---|---|
//! | 48 kHz | 48 kHz | 12.33 ms | 15 ms |
//! | 96 kHz | 96 kHz | 13.67 ms | 15 ms |
//! | 96 kHz | 48 kHz | 13.04 ms | 15 ms |
//! | 96 kHz | 44.1 kHz | 12.34 ms | 14.99 ms |
//! | 48 kHz | 44.1 kHz | 9.67 ms | 14.99 ms |
//! | 48 kHz | 96 kHz | 11.69 ms | 14.99 ms |
//!
//! Block 256 at the default changes only the RX path (W stays 6 ms with
//! TX, over the block). Every pair of 44.1, 48 and 96 kHz fits 15 ms at every block
//! of the stack. A larger budget grows W and P together: at 20 ms a 48 kHz
//! or 96 kHz device gets W 8 ms and P 12 ms, a 48 kHz app on a 96 kHz
//! device W 8 ms and P 11.36 ms (18.04 ms and W 20 ms RX only).
//!
//! A smaller budget an app sets may not hold the TX floor: [`Config::device`]
//! refuses it with the floor named, with the other problems (dstack #1: such
//! configs ran at P 4 ms before, and played late at a receiver). At 10 ms
//! those are a 48 kHz device with a 44.1 kHz app at block 128, block 256 on
//! a 48 or 44.1 kHz device, a 96 kHz device with a 44.1 kHz app at block
//! 256, and a 44.1 kHz device with a 48 kHz app at block 128. Our TX looped
//! into our own RX takes the two paths less the one P they share (W + B +
//! P, plus both converter delays): the whole-device test measures it.
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

use dstack::announce::FPP_MAX;
use dstack::clock::latency_samples;
use dstack::device::{NORMAL_MIN_LATENCY_NS, NORMAL_MIN_WORK_MARGIN_NS, TimingProfile};
use dstack::embed::Embedded;
use dstack::host::ClockSource;
use dstack::media::bypass::FrameSender;
use dstack::media::tx::TX_MARGIN_NS;
use dstack::stream::{BLOCKS, MAX_WORK_MARGIN};

/// dlink's latency budget unless the app sets one, ns: 15 ms for each
/// direction's whole path, the work (block, margin, rate conversion) and
/// the Dante latency together. [`split`] gives the Dante latency what the
/// work leaves. 10 ms from v0.6.22 (owner, ROZHODNUTÉ 3.11.2026, dstack
/// #68); 15 ms since v0.6.28 (owner, ROZHODNUTÉ 4.11.2026, dstack #1: 10 ms
/// was never meant to push dlink apps low). At 15 ms W gets 6 ms, room for
/// the 2.7 to 3.6 ms preemptions seen on loaded PCs with no CPU pinning
/// and no real time, and P stays over the TX floor ([`tx_floor_ns`]) for
/// every rate pair at every block.
pub const BUDGET_NS: u64 = 15_000_000;
/// The least Dante latency [`split`] leaves, ns: the normal profile's own
/// floor (`dstack::device::NORMAL_MIN_LATENCY_NS`, 4 ms), which the device
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
/// [`WORK_SHARE_DEN`] of it (the owner's 48 kHz example, dstack #1: about
/// 4 ms of pump margin and 6 ms of Dante latency at 10 ms; 6 ms of 15 ms,
/// the default since v0.6.28, 1.7 times the longest preemption seen on a
/// loaded PC, 3.6 ms).
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

/// The pump's margin W of an RX-only device, samples at `rate`: the whole
/// `budget_ns` (dstack #1, camera-box 4.11.2026: OBS dante-input, RX only
/// and unpinned, was preempted about 6 ms, right at its W 6 ms, and
/// re-bootstrapped every half minute), at most
/// `dstack::stream::MAX_WORK_MARGIN`, at least [`least_work_margin`].
///
/// W is in none of an RX-only device's paths: its RX path is P + B + the
/// converter's RX delay (block k reads the RX rings at its own stamps
/// whenever the pump runs), and D = B + P + W only stamps the engine's TX
/// rings, which it does not have. W only says how late the pump may run
/// before it starts again; a block that late reaches the app that much
/// later, so the app's own budget bounds it. The stack holds any W up to
/// `MAX_WORK_MARGIN` beside any P: `dstack::device::MAX_LATENCY_SAMPLES`
/// keeps it inside half the RX rings (`dstack::media::ring::SLOTS`).
pub fn rx_work_margin(budget_ns: u64, rate: u32, block: u64) -> u64 {
    latency_samples(budget_ns, rate)
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
    /// to P's TX floor on a device with TX; [`rx_work_margin`], the whole
    /// budget, on an RX-only device).
    pub margin: u64,
    /// The app's output to a receiver's playout, ns: W + P, plus the rate
    /// converter's TX delay when it runs. A receiver set to a longer
    /// latency than P adds the difference. An RX-only device has no TX
    /// path: the figure (its W of the whole budget + P) then passes the
    /// budget.
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
/// never under the block and 2 ms; RX only, the whole budget,
/// [`rx_work_margin`]; dstack #1) and the rate converter's
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
    // W is in the TX path: a device with TX keeps P's TX floor (the
    // packets' own age, [`tx_floor_ns`]), and W gives way, down to its own
    // least. The room's last sample is left to P, so P's whole samples
    // still reach the floor at any rate. An RX-only device's W is in none
    // of its paths (a late block only comes later): W is the whole budget
    // ([`rx_work_margin`]), and its P has the profile's floor.
    let (margin, floor) = if directions == Directions::Rx {
        (rx_work_margin(budget_ns, device_rate, b), MIN_LATENCY_NS)
    } else {
        let floor = tx_floor_ns(device_rate);
        let room = budget_ns.saturating_sub(convert_tx + floor);
        let margin = work_margin(budget_ns, device_rate, b)
            .min(samples_in(room).saturating_sub(1))
            .max(least_work_margin(device_rate, b));
        (margin, floor)
    };
    let tx_work = ns(margin) + convert_tx;
    let rx_work = ns(b) + convert_rx;
    let work = match directions {
        Directions::Both => tx_work.max(rx_work),
        Directions::Rx => rx_work,
        Directions::Tx => tx_work,
    };
    let latency_ns = ns(samples_in(budget_ns.saturating_sub(work)));
    if latency_ns < floor {
        return Err(format!(
            "budget_ns: {budget_ns} leaves a Dante latency of {latency_ns} ns after \
             {work} ns of work (block, margin, rate conversion); at least {floor} \
             ({})",
            if directions == Directions::Rx {
                "the normal profile's floor"
            } else {
                "a TX packet's own age, dlink::tx_floor_ns"
            }
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
