// SPDX-License-Identifier: MIT OR Apache-2.0

//! A dlink device's latency in named terms (dstack #244, the owner's
//! ROZHODNUTÉ of 12.11.2026): (a) the Dante network latency, (b) the
//! receive buffer, P = (a) + (b), and (c) the app hand-off, with the pump's
//! margin W and P's floors. The crate docs' "The latency, in named terms"
//! has the model and the defaults' table.

use dstack::announce::FPP_MAX;
use dstack::clock::latency_samples;
use dstack::device::{NORMAL_MIN_LATENCY_NS, NORMAL_MIN_WORK_MARGIN_NS};
use dstack::media::tx::TX_MARGIN_NS;
use dstack::stream::{BLOCKS, MAX_WORK_MARGIN};

use crate::{BLOCK, rated};

/// (a), the Dante network latency unless the app sets one
/// ([`crate::Config::network_latency_ns`]), ns: 1 ms, Dante's own default for a
/// gigabit network (dstack #244).
pub const NETWORK_LATENCY_NS: u64 = 1_000_000;
/// The path each direction had up to v0.6.105 (the old budget, 15 ms), ns.
/// It sizes only the DEFAULT receive buffer (b): an app that sets no
/// [`crate::Config::receive_buffer_ns`] gets the Dante latency it had then, so
/// nobody's latency moves (dstack #244). It is no key and no limit.
pub const REFERENCE_PATH_NS: u64 = 15_000_000;
/// W, the pump's work margin, of a device with TX unless the app sets one
/// ([`crate::Config::work_margin_ns`]), ns: 6 ms, room for the 2.7 to 3.6 ms
/// preemptions seen on loaded PCs with no CPU pinning and no real time
/// (dstack #1; 2/5 of the old 15 ms budget). Unset, it gives way to P's TX
/// floor as it did up to v0.6.105 ([`split`]).
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

/// W of a device that sets none, samples at `rate`: [`WORK_MARGIN_NS`] on a
/// device with TX, [`RX_WORK_MARGIN_NS`] on an RX-only one (dstack #1,
/// v0.6.32: W is in none of its paths: its RX path is P + B + the
/// converter's RX delay, and D = B + P + W only stamps the engine's TX
/// rings, which it does not have), at most `dstack::stream::MAX_WORK_MARGIN`
/// (the stack holds any W up to it beside any P:
/// `dstack::device::MAX_LATENCY_SAMPLES` keeps it inside half the RX rings),
/// at least [`least_work_margin`]. A block outside `stream::BLOCKS` is the
/// block's problem alone: W follows [`crate::BLOCK`] then.
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

/// `block` when it is one of `stream::BLOCKS`, else [`crate::BLOCK`].
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
/// 12.11.2026): [`crate::Config::terms`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Terms {
    /// (a), the Dante network latency, ns ([`crate::Config::network_latency_ns`]);
    /// `None` is [`NETWORK_LATENCY_NS`].
    pub network_latency_ns: Option<u64>,
    /// (b), the receive buffer, ns ([`crate::Config::receive_buffer_ns`]); `None`
    /// keeps the Dante latency of v0.6.105.
    pub receive_buffer_ns: Option<u64>,
    /// W, the pump's work margin, ns ([`crate::Config::work_margin_ns`]); `None`
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
            match set.receive_buffer_ns {
                Some(b) if b != self.receive_buffer_ns =>
                    format!("set {} ms, rounded up to whole samples", ms(b)),
                b => from(b.is_some()).to_string(),
            },
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
///   W + its TX delay ([`crate::rated::delay_ns`], none when `app_rate` is
///   `device_rate`); W is `terms.work_margin_ns`, else
///   [`default_work_margin`], which on a device with TX gives way (down to
///   [`least_work_margin`]) where [`REFERENCE_PATH_NS`] leaves no room for
///   both it and P's floor, exactly as up to v0.6.105.
///
/// With (b) unset, P, W and both paths are v0.6.105's for every config the
/// old 15 ms budget held, and P's floor for one it refused. With (b)
/// unset and (a) set under 1 ms, (b) grows so P stays at its floor.
///
/// The block B is `block` device frames ([`crate::BLOCK`] when it is not one of
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
    let (floor, why) = if directions == Directions::Rx {
        (MIN_LATENCY_NS, "the normal profile's floor")
    } else {
        (
            tx_floor_ns(device_rate),
            "a TX packet's own age, dlink::tx_floor_ns",
        )
    };
    let samples_in = |ns: u64| (u128::from(ns) * rate / 1_000_000_000) as u64;
    let least = least_work_margin(device_rate, b);
    // The default W as up to v0.6.105: on a device with TX it gives way,
    // down to its least, where the reference path leaves no room for both
    // it and P's floor (the room's last sample is left to P, so P's whole
    // samples still reach the floor at any rate). A W the app sets never
    // gives way.
    let default_margin = if directions.has_tx() {
        let room = REFERENCE_PATH_NS.saturating_sub(tx_convert_ns + floor);
        default_work_margin(device_rate, b, directions)
            .min(samples_in(room).saturating_sub(1))
            .max(least)
    } else {
        default_work_margin(device_rate, b, directions)
    };
    let margin = match terms.work_margin_ns {
        None => default_margin,
        Some(w) => {
            let m = latency_samples(w, device_rate);
            if !(least..=MAX_WORK_MARGIN).contains(&m) {
                return Err(format!(
                    "work_margin_ns: {w} is {m} samples at {device_rate} Hz; \
                     {least} to {MAX_WORK_MARGIN} (the block and 2 ms, the stack's largest margin)"
                ));
            }
            m
        }
    };
    let block_ns = ns(b);
    let rx_handoff_ns = block_ns + rx_convert_ns;
    let tx_handoff_ns = ns(margin) + tx_convert_ns;
    let a = terms.network_latency_ns.unwrap_or(NETWORK_LATENCY_NS);
    let receive_buffer = match terms.receive_buffer_ns {
        Some(rb) => rb,
        None => {
            // P as up to v0.6.105: the reference path less the work of the
            // device's directions at the default W, whole samples (rounded
            // down, as the budget's split did), at least the floor (a config
            // the old budget refused starts there); (b) is that less the
            // default (a), and never so small that the app's own (a) puts P
            // under the floor.
            let tx_work = ns(default_margin) + tx_convert_ns;
            let work = match directions {
                Directions::Both => tx_work.max(rx_handoff_ns),
                Directions::Rx => rx_handoff_ns,
                Directions::Tx => tx_work,
            };
            let reference = ns(samples_in(REFERENCE_PATH_NS.saturating_sub(work)));
            reference
                .max(floor)
                .saturating_sub(NETWORK_LATENCY_NS)
                .max(floor.saturating_sub(a))
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
