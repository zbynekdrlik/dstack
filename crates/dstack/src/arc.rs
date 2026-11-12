// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.
// Protocol facts (byte layouts, opcodes, behaviour) checked against
// inferno (https://github.com/teodly/inferno) as a reference; the code is
// our own.

//! ARC, Dante's control protocol on UDP 4440: the device's names, its
//! channel counts and lists, channel renames, its flows (phase B: the TX
//! flows receivers asked for on 4455, our RX flows) and its RX
//! subscriptions. Every opcode inferno's server answers is answered as
//! inferno answers it with flows and a subscriber (a request Dante
//! Controller gets refused it asks again at once: #72, 2026-10-26, 0x2200
//! and 0x2204 about 2 250 times a second while both were refused).
//!
//! | op1 | answer |
//! |---|---|
//! | 0x1000 | counts: TX, RX, inferno's 32 TX and 32 RX flows |
//! | 0x1001 | the device's new name (netaudio's `set_name`): OK, no body; saved, |
//! |        | announced, our own subscriptions to it renamed |
//! | 0x1002 | the device name |
//! | 0x1003 | the device names: current, factory, board, revision |
//! | 0x1100 | keyed (a count and keys: netaudio's start code 0x2809, [`control::settings`], and Dante Controller's 0x27ff with its 19 keys): each key asked, in its order, that we hold (`settings_body`), the latency's default and range included; with no body: zeros, as inferno |
//! | 0x1102 | the settings key table (`key_table`): the keys 0x1100 holds, each with its access; Dante Controller writes only a key marked writable (dstack #216) |
//! | 0x2000 | TX channels with their default names, paged |
//! | 0x2010 | TX channels' current names, paged |
//! | 0x2013 | TX rename |
//! | 0x2200 | TX flows, [`FLOW_PAGE`] slots a page (inferno's `query_tx_flows`) |
//! | 0x2201, 0x2202 | the older create, delete of a multicast TX flow: refused (inferno's answer when it creates or deletes none); ARC 2.8.9's 0x2601 creates one |
//! | 0x2600 (start 0x2809) | ARC 2.8.9's TX flow list ([`flows::list_reply_body`]): receivers' flows unicast, ours multicast with their group |
//! | 0x2601 (start 0x2809) | ARC 2.8.9's multicast create (dstack #193): a multicast flow in the asked slot ([`answer`]); the reply is its record, no destination |
//! | 0x2602 (start 0x2809) | ARC 2.8.9's delete of a multicast flow: OK, no body |
//! | 0x2204 | status [`STATUS_2320`], no body (the IEM card's answer, captured) |
//! | 0x2320 | status [`STATUS_2320`], no body (inferno's "???") |
//! | 0x3000 | RX channels with their names, subscriptions and statuses, paged |
//! | 0x3001 | RX rename |
//! | 0x3010 | subscribe or unsubscribe RX channels: OK, no body |
//! | 0x3014 | netaudio's unsubscribe: OK, no body |
//! | 0x3200 | RX flows, [`FLOW_PAGE`] slots a page (inferno's `query_rx_flows`) |
//! | 0x3300 | the fixed 8 bytes that keep Dante Controller from a clock-domain warning |
//!
//! Under ARC 2.8.9's start code 0x2809 (dstack #216), which Dante Controller
//! uses for a device announcing [`ARCP_VERSION`], 0x1000, 0x1003, 0x2010,
//! 0x2204 and the lists 0x2400 (TX channels), 0x3400 (RX channels) and
//! 0x3600 (RX flows) are answered in a 2.8.9 card's layouts (`arc/v289.rs`);
//! everything else as above. 0x1000 there states the device rename (word
//! 0's 0x0001), without which Dante Controller greys out its Rename.
//! netaudio's 2.8.9 writes there, 0x3410 (subscribe), 0x3401 (RX rename)
//! and 0x2438 (TX names), make the change of 0x3010, 0x3001 and 0x2013
//! through the same functions (`arc/v289/writes.rs`, dstack #220).
//!
//! No reply, as from inferno, to any other opcode, to a request whose op2 is not 0, and to a body inferno
//! fails to read before it answers (0x2201 under 2 bytes, 0x2202 under 4,
//! 0x3010 whose items are cut, 0x3014 under 6): [`ArcOutcome::Unanswered`].
//! A datagram that is no request ([`Header::parse`]) is dropped; one whose
//! length field disagrees with its size is counted as well
//! ([`ArcOutcome::Misframed`]). Refused
//! ([`STATUS_ERROR`]), changing nothing: a rename that renames nothing, a
//! page request without its first item, 0x2201 and 0x2202, and a change
//! the state file cannot keep ([`respond`]). A rename is all or nothing:
//! one invalid name refuses the whole request (inferno renames the valid
//! ones). An empty name is Dante's reset to the channel's default name
//! (netaudio's `reset_channel_name` sends one; since 0.4.2 its name offset
//! is the datagram's end, with no NUL after it). Every refused or
//! unanswered request is counted per peer and opcode ([`gaps`]); the server
//! logs the counts as one warning a minute.
//!
//! **Subscriptions** (0x3010, 0x3014) change our own RX channels only, and
//! are kept in the state file before the OK goes out; the channel reads
//! Unresolved at once (Dante Controller reads 0x3000 right after), and the
//! server hands the new subscriptions to the manager ([`Flows::subscribe`]),
//! which asks the transmitters for flows. A subscription whose names
//! Dante's rules refuse is not made (inferno makes it; our state file
//! could not keep it): the requester still gets inferno's OK.
//!
//! **Pages** (inferno's `serialize_items`): `[slots, count]`, `slots`
//! descriptors (the unused ones zero), then what they point at. Channel
//! lists hold 32 TX or 16 RX channels a page, as real devices answer
//! (netaudio steps through pages by those counts, Dante Controller by the
//! count each page reports), flow lists [`FLOW_PAGE`] of the 32 flow slots
//! (an empty slot is passed over, not counted). A page ends early, status
//! `0x8112`, once an item has brought it to [`SOFT_LIMIT`] bytes: that
//! item's descriptor is written but not counted (inferno), so the next page
//! starts with it. Every page fits one datagram
//! ([`MAX_DATAGRAM`]): no item is longer than
//! 600 bytes (a TX flow's names are cut to [`MAX_REMOTE_NAME`], ours are 31
//! at most). Logs name opcodes, sequences and channel numbers, never a
//! name.
//!
//! Our own reads of another device's ARC (a transmitter's channel list, for
//! a device that publishes no `_netaudio-chan` record, plan Task 9b) are
//! [`client`]'s: read-only, the same page format.

use std::io;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tokio::time::MissedTickBehavior;
use tracing::{debug, error, info, warn};

use crate::ctl::{Requests, header_op};
use crate::device::{
    Device, DeviceConfig, DeviceState, Subscription, channel_problems, valid_channel_name,
    valid_device_name,
};
use crate::info::Changes;
use crate::media::tx::{TxShared, TxTable};
use crate::subscribe::{Manager, SubStatus};
use crate::wire::{
    Body, HEADER_LEN, Header, MAX_DATAGRAM, STATUS_ERROR, STATUS_MORE, STATUS_OK, WireError,
    read_u16, reply, string_at,
};
use gaps::{Gap, Gaps};

pub mod client;
pub mod control;
pub mod flows;
pub mod gaps;
mod multicast;
mod v289;

/// ARC's UDP port.
pub const PORT: u16 = 4440;

/// The ARC version our devices announce (TXT `arcp_vers`, dstack #216):
/// 2.8.9, the generation Dante Controller offers the device rename to.
/// Its reads are in `arc/v289.rs`; a 2.7 request (start code 0x2729, 0x27ff)
/// gets 2.7's answers as before.
pub const ARCP_VERSION: &str = "2.8.9";
/// The router version announced with it (TXT `router_vers`), the 2.8.9
/// card's; 0x1003 states it too.
pub const ROUTER_VERSION: &str = "4.3.0";
/// The revision 0x1003 states after the board name.
const REVISION: &str = ":705";
/// How often the server looks whether a minute of [`Gaps`] is to be
/// reported.
pub const GAP_CHECK: Duration = Duration::from_secs(1);

/// The opcodes (op1) phase A answers.
pub mod op {
    pub const COUNTS: u16 = 0x1000;
    /// The device's new name (netaudio's `set_name`; inferno answers none).
    pub const DEVICE_RENAME: u16 = 0x1001;
    pub const DEVICE_NAME: u16 = 0x1002;
    pub const DEVICE_NAMES: u16 = 0x1003;
    /// Asked by Dante Controller; inferno answers 110 zero bytes.
    pub const ZEROS_1100: u16 = 0x1100;
    /// The settings key table, asked by Dante Controller: which settings
    /// it may write (dstack #216; inferno answers 94 zero bytes, which
    /// marks none writable). The name is the opcode's older one.
    pub const ZEROS_1102: u16 = 0x1102;
    /// A settings write. Our device takes the latency write, netaudio's
    /// and Dante Controller's (dasio #2, dstack #216), and leaves any other
    /// unanswered, as inferno: a refusal once made Dante Controller ask
    /// again thousands of times a second.
    pub const SETTINGS_SET: u16 = 0x1101;
    pub const TX_CHANNELS: u16 = 0x2000;
    pub const TX_NAMES: u16 = 0x2010;
    pub const TX_RENAME: u16 = 0x2013;
    pub const TX_FLOWS: u16 = 0x2200;
    pub const TX_MULTICAST_CREATE: u16 = 0x2201;
    pub const TX_MULTICAST_DELETE: u16 = 0x2202;
    /// Asked by Dante Controller; inferno answers status
    /// [`STATUS_2320`](super::STATUS_2320) and no body.
    pub const OP_2320: u16 = 0x2320;
    /// Asked by Dante Controller (a page request); the IEM card answers
    /// status [`STATUS_2320`](super::STATUS_2320) and no body (captured
    /// 30.10.2026, dstack #1).
    pub const OP_2204: u16 = 0x2204;
    pub const RX_CHANNELS: u16 = 0x3000;
    pub const RX_RENAME: u16 = 0x3001;
    pub const RX_SUBSCRIBE: u16 = 0x3010;
    /// netaudio's unsubscribe.
    pub const RX_UNSUBSCRIBE: u16 = 0x3014;
    pub const RX_FLOWS: u16 = 0x3200;
    pub const CLOCK: u16 = 0x3300;
}

/// The slots of a flow list's page (0x2200, 0x3200): inferno's 32 flows,
/// at most 16 a page.
pub const FLOW_PAGE: u8 = 16;
/// The most flows each way (inferno's TX and RX `MAX_FLOWS`, 0x1000).
pub const MAX_FLOWS: u16 = 32;
/// A page ends once an item has brought the reply to this many bytes, the
/// header counted (inferno's `PACKET_SIZE_SOFT_LIMIT`).
pub const SOFT_LIMIT: usize = 800;
/// The most bytes of a receiver's device name and of its flow name a TX
/// flow list writes (inferno writes them whole). A Dante name has 31 at
/// most, but the 4455 request that brought them may hold a kilobyte; cut
/// here, every item of a page stays far below a datagram.
pub const MAX_REMOTE_NAME: usize = 63;
/// inferno's status for 0x2320 ("???" in its source).
pub const STATUS_2320: u16 = 0x0030;
/// A flow's socket descriptor (`0x8002`, port, IPv4 address).
const SOCKET_DESCRIPTOR: u16 = 0x8002;
/// A TX flow descriptor's type: unicast.
const FLOW_UNICAST: u16 = 0x11;
/// A TX flow descriptor's type: multicast (dstack #193).
const FLOW_MULTICAST: u16 = 0x02;

/// TX channels per page.
pub const TX_PAGE: usize = 32;
/// RX channels per page.
pub const RX_PAGE: usize = 16;
/// The PCM type of the channel descriptor and the mDNS `pcm` entry: 0x0e,
/// what current Dante devices state (older ones state 4).
pub const PCM_TYPE: u8 = 0x0e;
/// The most channels one flow carries (inferno's `MAX_CHANNELS_IN_FLOW`).
pub const MAX_CHANNELS_IN_FLOW: u16 = 8;
/// The answer to 0x3300. Without it Dante Controller reports a clock
/// domain mismatch for the device.
pub const CLOCK_REPLY: [u8; 8] = [0x38, 0x00, 0x38, 0xfd, 0x38, 0xfe, 0x38, 0xff];
/// The counts' flags byte: bit 4, TX channels can be renamed. Bit 5, TX
/// multicast, stays clear (inferno sets it): the device creates no
/// multicast flow by 0x2201 (refused), so Dante Controller is never offered
/// one; ARC 2.8.9's create (0x2601, dstack #193) is answered.
const FLAG_TX_RENAME: u8 = 0x10;
/// An RX channel's flags in both generations' records (0x3000's second
/// word, 0x3400's flags; a reader sees no change from the generation):
/// inferno's 6 (netaudio: rename allowed) and 0x0008, netaudio's
/// can-subscribe-self bit (dstack #220). Our RX channels take a
/// subscription to our own TX, and netaudio refuses to send one without
/// the bit.
const RX_CHANNEL_FLAGS: u16 = 0x000e;
/// The device names' header (0x1003), before its strings.
const NAMES_HEADER: usize = 38;
/// The 16-byte descriptor of a page's channels.
const COMMON_DESCRIPTOR: usize = 16;
/// A datagram read from the socket; anything longer is cut and so fails
/// [`Header::parse`].
const RECV_BUFFER: usize = 2048;

/// What a saved change changed besides the names the mDNS records carry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Change {
    /// RX channels (from 0) renamed: the channel-change notification names
    /// them (inferno).
    pub renamed_rx: Vec<usize>,
    /// RX channels (from 0) whose subscription changed: the manager takes
    /// them ([`Flows::subscribe`]), and the notification names them.
    pub subscriptions: Vec<usize>,
    /// The device's own name changed: the records are announced under it
    /// and our flow requests carry it ([`Flows::renamed`]).
    pub device_renamed: bool,
    /// Dante Controller set the device latency to this, ns: the manager and
    /// the TX side take it live ([`Flows::set_latency`]) and the records
    /// are announced with it.
    pub latency: Option<u64>,
    /// Our multicast TX flows changed (ARC 2.8.9's create or delete,
    /// dstack #207): saved, and nothing else to tell (the records and the
    /// channel-change notification carry no TX flow). Set alone, never with
    /// another field ([`Change::TX_FLOWS_ONLY`]).
    pub tx_flows: bool,
}

impl Change {
    /// What a multicast create or delete changed: our TX flows alone. The
    /// server signals it to no one ([`serve`]).
    pub const TX_FLOWS_ONLY: Change = Change {
        renamed_rx: Vec::new(),
        subscriptions: Vec::new(),
        device_renamed: false,
        latency: None,
        tx_flows: true,
    };
}

/// What a request comes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArcOutcome {
    /// Send this reply; nothing changed.
    Reply(Vec<u8>),
    /// Send this reply; the state changed, so the caller saves it, announces
    /// the new mDNS records and passes the [`Change`] on (but
    /// [`Change::TX_FLOWS_ONLY`]: saved, nothing announced).
    Changed(Vec<u8>, Change),
    /// A request inferno leaves unanswered: no reply.
    Unanswered { op1: u16 },
    /// A datagram whose length field disagrees with its size (`op1` as its
    /// header's bytes 6–7 say): no request, no reply, but counted
    /// ([`Gap::Misframed`]), so a sender we misread shows in the warning.
    Misframed { op1: u16 },
    /// Shorter than the header: no request, no reply.
    Ignored,
}

impl ArcOutcome {
    /// What [`gaps::Gaps`] counts of this outcome: the opcode of a request
    /// left unanswered or refused ([`STATUS_ERROR`]), or of a datagram whose
    /// length field disagrees; `None` for any other answer and for a
    /// datagram shorter than the header.
    pub fn gap(&self) -> Option<(u16, Gap)> {
        match self {
            ArcOutcome::Unanswered { op1 } => Some((*op1, Gap::Unanswered)),
            ArcOutcome::Misframed { op1 } => Some((*op1, Gap::Misframed)),
            ArcOutcome::Reply(bytes) => match Header::parse(bytes) {
                Ok((h, _)) if h.op2 == STATUS_ERROR => Some((h.op1, Gap::Refused)),
                _ => None,
            },
            ArcOutcome::Changed(..) | ArcOutcome::Ignored => None,
        }
    }
}

/// The running flows as the ARC lists show them: the TX flows receivers
/// asked for on 4455 and our RX subscriptions' flows and statuses.
#[derive(Debug, Clone, Copy)]
pub struct Live<'a> {
    pub tx: &'a TxTable,
    pub rx: &'a Manager,
}

/// Where the ARC server reads the running flows and leaves the saved
/// subscriptions. Locks are taken in one order across the device: the
/// device, then the TX table, then the manager; nothing holding a later one
/// takes an earlier one.
pub trait Flows: Send + Sync + 'static {
    /// Calls `f` with the flows as they are now.
    fn view<R>(&self, f: impl FnOnce(&Live<'_>) -> R) -> R;
    /// The saved subscriptions changed to `subs`: the manager takes them.
    /// Returns the RX channels whose status moved
    /// ([`Manager::changed`]).
    fn subscribe(&self, subs: &[Option<Subscription>]) -> Vec<usize>;
    /// The device was renamed to `name`.
    fn renamed(&self, name: &str);
    /// The device latency is `ns` now (a Dante Controller write).
    fn set_latency(&self, ns: u64);
    /// Calls `f` with the TX table to change and the device's address (ARC
    /// 2.8.9's multicast create and delete, dstack #193), then hands the
    /// table's plans to the TX thread.
    fn change_tx<R>(&self, f: impl FnOnce(&mut TxTable, Ipv4Addr) -> R) -> R;
}

/// The daemon's flows (plan Task 7): the TX side's table and the manager.
#[derive(Debug, Clone)]
pub struct Running {
    pub tx: Arc<TxShared>,
    pub rx: Arc<Mutex<Manager>>,
}

impl Flows for Running {
    fn view<R>(&self, f: impl FnOnce(&Live<'_>) -> R) -> R {
        let control = self.tx.lock();
        let rx = self.rx.lock().unwrap_or_else(PoisonError::into_inner);
        f(&Live {
            tx: &control.table,
            rx: &rx,
        })
    }

    fn subscribe(&self, subs: &[Option<Subscription>]) -> Vec<usize> {
        let mut rx = self.rx.lock().unwrap_or_else(PoisonError::into_inner);
        rx.load(subs);
        rx.changed()
    }

    fn renamed(&self, name: &str) {
        self.rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set_host(name);
    }

    fn set_latency(&self, ns: u64) {
        self.rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set_latency(ns);
        self.tx.set_device_latency(ns);
    }

    fn change_tx<R>(&self, f: impl FnOnce(&mut TxTable, Ipv4Addr) -> R) -> R {
        let mut control = self.tx.lock();
        let out = f(&mut control.table, self.tx.ip());
        self.tx.publish(&control);
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    Rx,
    Tx,
}

impl Dir {
    fn label(self) -> &'static str {
        match self {
            Dir::Rx => "RX",
            Dir::Tx => "TX",
        }
    }
}

/// Answers one datagram from `state`, `cfg` and the running flows `live`.
/// Only a valid rename or a subscription change changes `state` (and is
/// [`ArcOutcome::Changed`]); this function saves nothing ([`respond`] does)
/// and tells nobody (the server does). ARC 2.8.9's multicast create and
/// delete change the TX table, which `live` only reads: [`answer`] answers
/// them (and saves the state's flows), here they go unanswered.
pub fn handle(
    state: &mut DeviceState,
    cfg: &DeviceConfig,
    live: &Live<'_>,
    req: &[u8],
) -> ArcOutcome {
    let (h, body) = match Header::parse(req) {
        Ok(parsed) => parsed,
        Err(e @ WireError::Length { .. }) => {
            // The header is there (Length comes only after it was read):
            // its opcode names the datagram in the gap guard's count.
            let op1 = read_u16(req, 6).unwrap_or(0);
            debug!(error = %e, op = format_args!("{op1:#06x}"), "ARC datagram dropped, counted");
            return ArcOutcome::Misframed { op1 };
        }
        Err(e @ WireError::Short(_)) => {
            debug!(error = %e, "ARC datagram dropped");
            return ArcOutcome::Ignored;
        }
    };
    debug!(
        op = format_args!("{:#06x}", h.op1),
        seq = h.seq,
        "ARC request"
    );
    if h.op2 != 0 {
        return unanswered(&h, "op2 is not 0");
    }
    // ARC 2.8.9's own reads (dstack #216) and writes (#220); its other
    // requests are 2.7's.
    if let Some(outcome) = v289::read(&h, body, state, cfg, live) {
        return outcome;
    }
    if let Some(outcome) = v289::write(&h, body, state, cfg) {
        return outcome;
    }
    let ok = |body: &[u8]| ArcOutcome::Reply(reply(&h, STATUS_OK, body));
    match h.op1 {
        op::COUNTS => ok(&counts(cfg)),
        op::DEVICE_NAME => {
            let mut out = Body::new();
            out.cstr(&state.name);
            ok(&out.into_vec())
        }
        op::DEVICE_NAMES => ok(&device_names(state, cfg)),
        op::DEVICE_RENAME => device_rename(&h, body, state),
        // Keyed, from netaudio (start 0x2809) or Dante Controller (0x27ff
        // with its 19 keys): our values, as the hardware answers both.
        op::ZEROS_1100 if h.start == control::SETTINGS_START => match settings_body(body, cfg) {
            Some(out) => ok(&out),
            None => unanswered(&h, "a settings request that does not read"),
        },
        // Dante Controller's (0x27ff with its 19 keys): our values too; a
        // body that does not read, or none, gets zeros as inferno's.
        op::ZEROS_1100 => ok(&settings_body(body, cfg)
            .filter(|_| !body.is_empty())
            .unwrap_or_else(|| vec![0; 110])),
        op::ZEROS_1102 => ok(&key_table(cfg)),
        op::TX_CHANNELS => tx_channels(&h, body, cfg),
        op::TX_NAMES => tx_names(&h, body, state, |_| true),
        op::TX_RENAME => rename(&h, body, state, cfg, Dir::Tx),
        op::RX_CHANNELS => rx_channels(&h, body, state, cfg, live.rx),
        op::RX_RENAME => rename(&h, body, state, cfg, Dir::Rx),
        op::TX_FLOWS => tx_flows(&h, body, cfg, live.tx),
        // ARC 2.8.9's TX flow list (dstack #148): what every dstack receiver
        // reads to find a transmitter's multicast flows.
        flows::LIST if h.start == flows::START_2809 => ok(&flows_2809(cfg, live.tx)),
        op::RX_FLOWS => rx_flows(&h, body, cfg, live.rx),
        op::TX_MULTICAST_CREATE => no_multicast(&h, body, 2),
        op::TX_MULTICAST_DELETE => no_multicast(&h, body, 4),
        op::OP_2320 | op::OP_2204 => ArcOutcome::Reply(reply(&h, STATUS_2320, &[])),
        op::RX_SUBSCRIBE => subscribe(&h, body, state, cfg),
        op::RX_UNSUBSCRIBE => unsubscribe(&h, body, state, cfg),
        op::CLOCK => ok(&CLOCK_REPLY),
        _ => unanswered(&h, "an opcode inferno does not answer"),
    }
}

/// What the server answers to `req`: ARC 2.8.9's multicast create and
/// delete (0x2601, 0x2602 under [`flows::START_2809`], dstack #193) with
/// the TX table to change ([`Flows::change_tx`]), saved to the state file
/// before it is answered (dstack #207: a restart makes the flow again); a
/// change that cannot be saved is undone, on the TX table too, and
/// refused, as [`respond`] does for a rename. Anything else by [`respond`]
/// with the flows as they are.
pub fn answer<F: Flows>(device: &mut Device, running: &F, req: &[u8]) -> ArcOutcome {
    match Header::parse(req) {
        Ok((h, body))
            if h.start == flows::START_2809
                && h.op2 == 0
                && matches!(h.op1, flows::CREATE | flows::DELETE) =>
        {
            running.change_tx(|tx, ip| {
                let (state, table) = (device.state.clone(), tx.clone());
                let outcome = multicast::answer(&h, body, &mut device.state, &device.cfg, tx, ip);
                saved(device, req, outcome, |device| {
                    device.state = state;
                    *tx = table;
                })
            })
        }
        _ => running.view(|live| respond(device, live, req)),
    }
}

/// [`handle`], and a change saved to the state file before it is
/// answered: a rename or a subscription that cannot be saved is undone and
/// refused, so a restart never loses a change the device confirmed.
pub fn respond(device: &mut Device, live: &Live<'_>, req: &[u8]) -> ArcOutcome {
    let before = device.state.clone();
    let latency_before = device.cfg.latency_ns;
    let outcome = match Header::parse(req) {
        Ok((h, body)) if h.op1 == op::SETTINGS_SET && h.op2 == 0 => set_latency(&h, body, device),
        _ => handle(&mut device.state, &device.cfg, live, req),
    };
    saved(device, req, outcome, |device| {
        device.state = before;
        device.cfg.latency_ns = latency_before;
    })
}

/// `outcome` of `req`, a change in it saved to the state file first: one
/// that cannot be saved is undone by `undo` (whatever it changed: the
/// state, the latency, the TX table) and refused ([`STATUS_ERROR`]).
fn saved(
    device: &mut Device,
    req: &[u8],
    outcome: ArcOutcome,
    undo: impl FnOnce(&mut Device),
) -> ArcOutcome {
    match outcome {
        ArcOutcome::Changed(bytes, change) => match device.state.save(&device.cfg.state_file) {
            Ok(()) => ArcOutcome::Changed(bytes, change),
            Err(e) => {
                let op1 = header_op(req);
                error!(
                    error = %e,
                    op = format_args!("{op1:#06x}"),
                    "a change could not be saved: refused and undone"
                );
                undo(device);
                match Header::parse(req) {
                    Ok((h, _)) => ArcOutcome::Reply(reply(&h, STATUS_ERROR, &[])),
                    Err(_) => ArcOutcome::Ignored,
                }
            }
        },
        other => other,
    }
}

/// 0x1101, a settings write (dasio #2): a latency write, netaudio's or
/// Dante Controller's keyed one (dstack #216;
/// [`control::parse_latency_body`]), sets the device latency when the
/// config's rules hold for it ([`DeviceConfig::with_latency`]): kept in
/// the state ([`DeviceState::set_latency`]), taken live, answered OK; a
/// latency they refuse is refused ([`STATUS_ERROR`]). Any other body stays
/// unanswered, as inferno leaves 0x1101 (a refusal once made Dante
/// Controller ask again thousands of times a second).
fn set_latency(h: &Header, body: &[u8], device: &mut Device) -> ArcOutcome {
    let Some(ns) = control::parse_latency_body(body).map(u64::from) else {
        return unanswered(h, "a settings write in another form than the latency write");
    };
    if let Err(problems) = device.cfg.with_latency(ns) {
        warn!(latency_ns = ns, problems = %problems.join("; "), "a latency write refused");
        return ArcOutcome::Reply(reply(h, STATUS_ERROR, &[]));
    }
    let config_ns = device
        .state
        .config_latency_ns
        .unwrap_or(device.cfg.latency_ns);
    device.state.set_latency(ns, config_ns);
    device.cfg.latency_ns = ns;
    info!(latency_ns = ns, "device latency set (Dante Controller)");
    ArcOutcome::Changed(
        reply(h, STATUS_OK, &[]),
        Change {
            latency: Some(ns),
            ..Change::default()
        },
    )
}

/// The ARC server: answers every datagram on `sock` from the shared device
/// and the running `flows`. After each saved change but a multicast
/// flow's ([`Change::TX_FLOWS_ONLY`]) it signals `changed` (the mDNS
/// announcer waits on it), hands changed subscriptions to the manager
/// ([`Flows::subscribe`]) and adds the RX channels renamed, whose
/// subscription changed or whose status moved to `rx_changes` (the info
/// server's channel-change notification). Ends only with an error
/// receiving; a reply that cannot be sent is logged and the server goes
/// on. Refused and unanswered requests are counted ([`Gaps`]) and their
/// counts logged as one warning a minute, looked at every [`GAP_CHECK`];
/// every datagram is counted in `requests` (the control socket's status).
pub async fn serve<F: Flows>(
    sock: UdpSocket,
    shared: Arc<Mutex<Device>>,
    flows: Arc<F>,
    changed: Arc<Notify>,
    rx_changes: Arc<Changes>,
    requests: Arc<Requests>,
) -> io::Result<()> {
    let mut buf = vec![0u8; RECV_BUFFER];
    let mut gaps = Gaps::new(gaps::WINDOW);
    let mut check = tokio::time::interval(GAP_CHECK);
    check.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        let report = tokio::select! {
            got = sock.recv_from(&mut buf) => {
                let (n, peer) = got?;
                requests.received(PORT, peer.ip(), header_op(&buf[..n]));
                let (outcome, moved) = {
                    let mut device = shared.lock().unwrap_or_else(PoisonError::into_inner);
                    let outcome = answer(&mut device, flows.as_ref(), &buf[..n]);
                    if let ArcOutcome::Changed(_, c) = &outcome
                        && c.device_renamed
                    {
                        flows.renamed(&device.state.name);
                    }
                    if let ArcOutcome::Changed(_, Change { latency: Some(ns), .. }) = &outcome {
                        flows.set_latency(*ns);
                    }
                    let moved = match &outcome {
                        ArcOutcome::Changed(_, c) if !c.subscriptions.is_empty() => {
                            flows.subscribe(&device.state.subscriptions)
                        }
                        _ => Vec::new(),
                    };
                    (outcome, moved)
                };
                let report = outcome
                    .gap()
                    .and_then(|(op1, gap)| gaps.record(Instant::now(), peer.ip(), op1, gap));
                let sent = match outcome {
                    ArcOutcome::Ignored
                    | ArcOutcome::Misframed { .. }
                    | ArcOutcome::Unanswered { .. } => None,
                    ArcOutcome::Reply(bytes) => Some((bytes, None)),
                    ArcOutcome::Changed(bytes, change) => Some((bytes, Some(change))),
                };
                if let Some((bytes, change)) = sent {
                    // The notification is queued before the reply goes, as
                    // inferno sends its own before answering a rename.
                    if let Some(change) = change.filter(|c| *c != Change::TX_FLOWS_ONLY) {
                        rx_changes.add(
                            change
                                .renamed_rx
                                .into_iter()
                                .chain(change.subscriptions)
                                .chain(moved),
                        );
                        changed.notify_one();
                    }
                    if let Err(e) = sock.send_to(&bytes, peer).await {
                        warn!(error = %e, "an ARC reply could not be sent");
                    }
                }
                report
            }
            _ = check.tick() => gaps.flush(Instant::now()),
        };
        if let Some(report) = report {
            warn!(
                requests = report.requests(),
                counts = %report,
                "ARC requests refused or not answered in a minute (peer id, opcode)"
            );
        }
    }
}

/// 0x1001 (netaudio's `set_name`): the body is the new name, NUL-ended. A
/// name Dante holds ([`valid_device_name`]) becomes the device's (saved,
/// announced, carried by our flow requests); the same name again changes
/// nothing; anything else is refused.
fn device_rename(h: &Header, body: &[u8], state: &mut DeviceState) -> ArcOutcome {
    let Some(end) = body.iter().position(|&b| b == 0) else {
        return refuse(h, "a device name without its NUL");
    };
    let Some(name) = std::str::from_utf8(&body[..end])
        .ok()
        .filter(|n| valid_device_name(n))
    else {
        return refuse(h, "not a name Dante holds");
    };
    if name == state.name {
        return ArcOutcome::Reply(reply(h, STATUS_OK, &[]));
    }
    // Our own RX subscribed to our own TX follow the name.
    let moved = state.rename(name);
    info!(
        name,
        own_subscriptions = moved.len(),
        "the device renamed by ARC (0x1001)"
    );
    ArcOutcome::Changed(
        reply(h, STATUS_OK, &[]),
        Change {
            subscriptions: moved,
            device_renamed: true,
            ..Change::default()
        },
    )
}

fn refuse(h: &Header, why: &str) -> ArcOutcome {
    debug!(
        op = format_args!("{:#06x}", h.op1),
        seq = h.seq,
        why,
        "ARC request refused"
    );
    ArcOutcome::Reply(reply(h, STATUS_ERROR, &[]))
}

/// No reply, as inferno sends none.
fn unanswered(h: &Header, why: &str) -> ArcOutcome {
    debug!(
        op = format_args!("{:#06x}", h.op1),
        seq = h.seq,
        why,
        "ARC request not answered"
    );
    ArcOutcome::Unanswered { op1: h.op1 }
}

/// The first item a page request names (from 1), in its body's bytes 2–3
/// (inferno's `extract_start_index`); `None` without one.
fn first_item(req: &[u8]) -> Option<u16> {
    read_u16(req, 2).filter(|&first| first > 0)
}

/// `s` cut to at most [`MAX_REMOTE_NAME`] bytes, at a character's start.
fn capped(s: &str) -> &str {
    let mut end = s.len().min(MAX_REMOTE_NAME);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// A flow socket descriptor: `0x8002`, the port, the IPv4 address
/// (inferno's `DestinationSocketDescriptor`).
fn socket_descriptor(out: &mut Body, at: std::net::SocketAddrV4) {
    out.u16(SOCKET_DESCRIPTOR);
    out.u16(at.port());
    out.bytes(&at.ip().octets());
}

/// Our TX flows as an ARC 2.8.9 list (0x2600, [`flows::list_reply_body`]):
/// each a unicast flow to its receiver or a multicast flow to its group
/// (dstack #193), its slot (from 1) as both ids.
fn flows_2809(cfg: &DeviceConfig, tx: &TxTable) -> Vec<u8> {
    let listed: Vec<flows::TxFlow> = (0..usize::from(MAX_FLOWS))
        .filter_map(|slot| {
            let f = tx.flow(slot)?;
            let id = wire_u16(slot + 1);
            Some(flows::TxFlow {
                id,
                local_id: id,
                flags: if f.multicast {
                    flows::MULTICAST
                } else {
                    flows::UNICAST
                },
                channels: f.channels.iter().map(|c| c.unwrap_or(0)).collect(),
                fpp: f.fpp,
                bits: u16::from(cfg.encoding),
                destinations: vec![f.dest],
            })
        })
        .collect();
    flows::list_reply_body(&listed)
}

/// 0x2200: the TX flows receivers asked for on 4455, expired ones too
/// (listed until a new destination takes the slot), by slot. Each flow,
/// from a multiple of 4: the flow descriptor (flow, `0x11` unicast or
/// `0x02` multicast (dstack #193), our rate, 0, our encoding, 1, the
/// slots, the socket descriptor's offset, each slot's TX channel or 0, the
/// names descriptor's offset); at a multiple of 4 the socket descriptor
/// (the receiver's address, a multicast flow's group); the names
/// descriptor (`0x0a00, 1`, the receiver's device and flow names, `0x10`,
/// ours, 8 zeros); then the three names: ours `<flow>_<process>`, the
/// receiver's device and flow names (cut to [`MAX_REMOTE_NAME`]). A reader
/// follows the offsets. The page's item is the flow descriptor's offset.
fn tx_flows(h: &Header, req: &[u8], cfg: &DeviceConfig, tx: &TxTable) -> ArcOutcome {
    page(
        h,
        req,
        usize::from(FLOW_PAGE),
        2,
        0..usize::from(MAX_FLOWS),
        |slot, out| {
            let f = tx.flow(slot)?;
            let id = wire_u16(slot + 1);
            out.align(4);
            let main = out.offset();
            let kind = if f.multicast {
                FLOW_MULTICAST
            } else {
                FLOW_UNICAST
            };
            for field in [id, kind] {
                out.u16(field);
            }
            out.u32(cfg.sample_rate);
            for field in [0, u16::from(cfg.encoding), 1, wire_u16(f.channels.len())] {
                out.u16(field);
            }
            let socket_ref = out.offset();
            out.u16(0);
            for channel in &f.channels {
                out.u16(channel.unwrap_or(0));
            }
            let names_ref = out.offset();
            out.u16(0);
            out.align(4);
            let here = out.offset();
            out.set_u16(socket_ref, here);
            socket_descriptor(out, f.dest);
            let here = out.offset();
            out.set_u16(names_ref, here);
            let names = out.offset();
            out.zeros(20);
            out.set_u16(names, 0x0a00);
            out.set_u16(names + 2, 1);
            out.set_u16(names + 8, 0x10);
            let local = out.cstr(&format!("{id}_{}", cfg.process_id));
            let host = out.cstr(capped(&f.rx_host));
            let name = out.cstr(capped(&f.rx_flow_name));
            out.set_u16(names + 4, host);
            out.set_u16(names + 6, name);
            out.set_u16(names + 10, local);
            Some(main.to_be_bytes().to_vec())
        },
    )
}

/// A flow latency of `latency_ns` as an RX flow list states it: in whole
/// samples at `rate`, then back in ns (inferno keeps it in samples), at
/// most `u32::MAX`.
fn stated_latency_ns(latency_ns: u64, rate: u32) -> u32 {
    let samples = crate::clock::latency_samples(latency_ns, rate);
    let ns = u128::from(samples) * 1_000_000_000 / u128::from(rate.max(1));
    u32::try_from(ns).unwrap_or(u32::MAX)
}

/// The latency default the settings read states: Dante's 1 ms, as the
/// hardware states it.
const LATENCY_DEFAULT_NS: u64 = 1_000_000;

/// The most keys one settings reply holds: each takes a pair and a value
/// (8 bytes), and the count is one byte.
const SETTINGS_MAX_KEYS: usize = 128;
const _: () = assert!(HEADER_LEN + 2 + 8 * SETTINGS_MAX_KEYS <= MAX_DATAGRAM);

/// Our value for a settings key (0x1100): the sample rate, no pull-up,
/// our latency as configured and in effect, a fixed default
/// ([`LATENCY_DEFAULT_NS`]) and the range
/// Dante Controller may set ([`DeviceConfig::latency_range`]). Dante
/// Controller greys its latency control and shows 0.0 ms without the
/// default and the range (mbc, 2.11.2026); every hardware device answers
/// them.
fn setting(key: u16, cfg: &DeviceConfig) -> Option<u32> {
    use control::key;
    let ns = |v: u64| u32::try_from(v).unwrap_or(u32::MAX);
    let (min, max) = cfg.latency_range();
    match key {
        key::SAMPLE_RATE => Some(cfg.sample_rate),
        key::PULLUP => Some(0),
        key::LATENCY_CONFIGURED | key::LATENCY_ACTIVE => Some(ns(cfg.latency_ns)),
        // A fixed default (Dante's 1 ms, within our range): it never moves
        // with a write, which netaudio's write check would read as another
        // setting changed.
        key::LATENCY_DEFAULT => Some(ns(LATENCY_DEFAULT_NS.clamp(min, max))),
        key::LATENCY_MIN => Some(ns(min)),
        key::LATENCY_MAX => Some(ns(max)),
        _ => None,
    }
}

/// The 0x1100 reply to a keyed request (`body`: a `u16` count, then the
/// keys), as the hardware answers it: the count twice (a byte each), then
/// a pair per key in the asked order, `key, offset` for a key we hold (the
/// offset, from the packet's start, of its `u32` after the pairs) and
/// `0, key` for one we do not, then the values. `None` when the count
/// passes the body or [`SETTINGS_MAX_KEYS`].
fn settings_body(body: &[u8], cfg: &DeviceConfig) -> Option<Vec<u8>> {
    let n = usize::from(read_u16(body, 0)?);
    if n > SETTINGS_MAX_KEYS {
        return None;
    }
    let keys: Vec<u16> = (0..n)
        .map(|k| read_u16(body, 2 + 2 * k))
        .collect::<Option<_>>()?;
    let mut pairs = Vec::new();
    let mut values = Vec::new();
    pairs.extend_from_slice(&[n as u8, n as u8]);
    let values_at = HEADER_LEN + 2 + 4 * n;
    for k in keys {
        match setting(k, cfg) {
            Some(v) => {
                let at = (values_at + values.len()) as u16;
                pairs.extend_from_slice(&k.to_be_bytes());
                pairs.extend_from_slice(&at.to_be_bytes());
                values.extend_from_slice(&v.to_be_bytes());
            }
            None => {
                pairs.extend_from_slice(&[0, 0]);
                pairs.extend_from_slice(&k.to_be_bytes());
            }
        }
    }
    pairs.extend_from_slice(&values);
    Some(pairs)
}

/// A key Dante Controller may write, as the hardware marks it in its
/// key table (0x1102).
const KEY_READ_WRITE: u16 = 0x0003;
/// A key it may only read.
const KEY_READ_ONLY: u16 = 0x0001;

/// 0x1102, the settings key table (dstack #216): the count (`u16`), then
/// `key, access` (`u16` each) for each key our settings read holds
/// ([`control::SETTING_KEYS`] that [`setting`] answers), in that order.
/// The latency as configured and in effect (0x8205, 0x8301, the keys
/// netaudio's write sets) is writable when a write can move it
/// ([`latency_writable`]); every other key is read only. A real card
/// answers its 31 keys so (`tests/fixtures/arc_settings_card.json`);
/// with inferno's zeros Dante Controller marked every setting read only,
/// greyed out the rename and never sent the latency write.
fn key_table(cfg: &DeviceConfig) -> Vec<u8> {
    use control::key;
    let keys: Vec<u16> = control::SETTING_KEYS
        .into_iter()
        .filter(|&k| setting(k, cfg).is_some())
        .collect();
    let mut out = Body::new();
    out.u16(keys.len() as u16);
    for k in keys {
        let access = match k {
            key::LATENCY_CONFIGURED | key::LATENCY_ACTIVE if latency_writable(cfg) => {
                KEY_READ_WRITE
            }
            _ => KEY_READ_ONLY,
        };
        out.u16(k);
        out.u16(access);
    }
    out.into_vec()
}

/// Whether a latency write (0x1101) can move the device's latency: the
/// range it states ([`DeviceConfig::latency_range`]) holds another value
/// than its own.
fn latency_writable(cfg: &DeviceConfig) -> bool {
    let (min, max) = cfg.latency_range();
    min < max
}

/// 0x3200: our RX flows a transmitter granted (an asked flow is listed
/// once granted), by slot. Each flow, from a multiple of 4: the flow
/// descriptor (flow, 1, our rate, 0, our encoding, 1, the slots, the words
/// a bitmask, the socket descriptor's offset, each slot's bitmask offset,
/// descriptor 2's offset); at a multiple of 4 the socket descriptor (our
/// address and the slot's port); descriptor 2 (`9, 1, 0x800, 0`, the
/// latency in ns, 0); each slot's bitmask of the RX channels it feeds (16 a
/// word, as many words as our RX channels need; one word for a flow
/// without slots). A reader follows the offsets. The page's item is the
/// flow descriptor's offset.
fn rx_flows(h: &Header, req: &[u8], cfg: &DeviceConfig, rx: &Manager) -> ArcOutcome {
    page(
        h,
        req,
        usize::from(FLOW_PAGE),
        2,
        0..usize::from(MAX_FLOWS),
        |slot, out| {
            let f = granted_flow(rx, slot)?;
            let words = if f.rx.is_empty() {
                1
            } else {
                cfg.rx.len().div_ceil(16).max(1)
            };
            out.align(4);
            let main = out.offset();
            out.u16(wire_u16(slot + 1));
            out.u16(1);
            out.u32(cfg.sample_rate);
            for field in [
                0,
                u16::from(cfg.encoding),
                1,
                wire_u16(f.rx.len()),
                wire_u16(words),
            ] {
                out.u16(field);
            }
            // The offsets, filled in once their targets are written: the
            // socket descriptor, each slot's bitmask, descriptor 2.
            let refs = out.offset();
            out.zeros(2 * (f.rx.len() + 2));
            out.align(4);
            let here = out.offset();
            out.set_u16(refs, here);
            socket_descriptor(out, std::net::SocketAddrV4::new(rx.ip(), f.port));
            let here = out.offset();
            out.set_u16(refs + wire_u16(2 * (f.rx.len() + 1)), here);
            for field in [9, 1, 0x800, 0] {
                out.u16(field);
            }
            out.u32(stated_latency_ns(f.latency_ns, cfg.sample_rate));
            out.u32(0);
            for (i, fed) in f.rx.iter().enumerate() {
                let here = out.offset();
                out.set_u16(refs + wire_u16(2 * (i + 1)), here);
                for word in bitmask(fed, words) {
                    out.u16(word);
                }
            }
            Some(main.to_be_bytes().to_vec())
        },
    )
}

/// The RX channels `channels` (from 0) as `words` 16-bit words: channel
/// `c` is bit `c mod 16` of word `c / 16` (inferno's bit vector: a channel
/// named twice is one bit); channels past the words are left out.
fn bitmask(channels: &[usize], words: usize) -> Vec<u16> {
    let mut out = vec![0u16; words];
    for &c in channels {
        if let Some(word) = out.get_mut(c / 16) {
            *word |= 1 << (c % 16);
        }
    }
    out
}

/// 0x2201 and 0x2202: the device makes no multicast flow, so it creates or
/// deletes none and refuses, as inferno does when it creates or deletes
/// none. A body shorter than `readable` bytes inferno fails to read before
/// it answers: no reply.
fn no_multicast(h: &Header, req: &[u8], readable: usize) -> ArcOutcome {
    if req.len() < readable {
        return unanswered(h, "a multicast flow request too short to read");
    }
    refuse(h, "no multicast flows")
}

/// 0x3010's items (6 bytes each: local channel, TX channel name offset,
/// TX device name offset), read as inferno's `deserialize_items` reads
/// them: `min(count, len / 6)` items after the first two bytes. `None`
/// where inferno fails: under 2 bytes, or those items running past the end.
fn subscription_items(req: &[u8]) -> Option<Vec<[u16; 3]>> {
    const ITEM: usize = 6;
    let count = usize::from(*req.get(1)?).min(req.len() / ITEM);
    let items = req.get(2..)?.get(..count * ITEM)?;
    let (items, _) = items.as_chunks::<ITEM>();
    Some(
        items
            .iter()
            .map(|i| {
                [
                    u16::from_be_bytes([i[0], i[1]]),
                    u16::from_be_bytes([i[2], i[3]]),
                    u16::from_be_bytes([i[4], i[5]]),
                ]
            })
            .collect(),
    )
}

/// Sets RX channel `rx`'s subscription to `sub` in `state`; whether it
/// changed.
fn set_subscription(
    state: &mut DeviceState,
    cfg: &DeviceConfig,
    rx: usize,
    sub: Option<Subscription>,
) -> bool {
    state.subscription(rx) != sub.as_ref() && state.set_subscription(cfg, rx, sub)
}

/// OK, no body; [`ArcOutcome::Changed`] when `changed` names a channel.
fn subscribed(h: &Header, changed: Vec<usize>) -> ArcOutcome {
    let bytes = reply(h, STATUS_OK, &[]);
    if changed.is_empty() {
        return ArcOutcome::Reply(bytes);
    }
    ArcOutcome::Changed(
        bytes,
        Change {
            subscriptions: changed,
            ..Change::default()
        },
    )
}

/// 0x3010 (inferno's `set_channels_subscriptions`): its items, applied
/// ([`subscribe_items`]). OK, no body, to every request inferno can read
/// (also when nothing changed); a cut one gets no reply.
fn subscribe(h: &Header, req: &[u8], state: &mut DeviceState, cfg: &DeviceConfig) -> ArcOutcome {
    let Some(items) = subscription_items(req) else {
        return unanswered(h, "a subscription request whose items are cut");
    };
    subscribe_items(h, req, &items, state, cfg)
}

/// Each item (`[RX channel, TX channel name offset, TX device name
/// offset]`, the offsets into `req`) subscribes its RX channel to the TX
/// channel and device its two names say, or, when either name's offset is
/// 0, unsubscribes it. Channel 0 and channels we do not have are passed
/// over; names inferno cannot read (an offset inside the header or past
/// the datagram, bytes that are not UTF-8) change nothing, and so do names
/// Dante's rules refuse (inferno subscribes to them; our state file cannot
/// keep them). OK, no body, also when nothing changed. 2.7's 0x3010 and
/// 2.8.9's 0x3410 (`arc/v289/writes.rs`) share it.
fn subscribe_items(
    h: &Header,
    req: &[u8],
    items: &[[u16; 3]],
    state: &mut DeviceState,
    cfg: &DeviceConfig,
) -> ArcOutcome {
    let mut changed = Vec::new();
    for &[id, channel_at, device_at] in items {
        // Channel 0 is passed over here, a channel we do not have where the
        // state refuses it.
        let Some(rx) = usize::from(id).checked_sub(1) else {
            continue;
        };
        let sub = if channel_at > 0 && device_at > 0 {
            let (Some(tx_channel), Some(tx_device)) =
                (string_at(req, channel_at), string_at(req, device_at))
            else {
                debug!(channel = id, "subscription whose names cannot be read");
                continue;
            };
            let sub = Subscription {
                tx_channel: tx_channel.to_string(),
                tx_device: tx_device.to_string(),
            };
            if !sub.is_valid() {
                info!(
                    channel = id,
                    "subscription to a name Dante's rules refuse: not made"
                );
                continue;
            }
            Some(sub)
        } else {
            None
        };
        let made = sub.is_some();
        if set_subscription(state, cfg, rx, sub) && !changed.contains(&rx) {
            info!(
                seq = h.seq,
                channel = id,
                subscribed = made,
                "RX subscription changed"
            );
            changed.push(rx);
        }
    }
    subscribed(h, changed)
}

/// 0x3014, netaudio's unsubscribe (`0001 0000 <local channel>`): the
/// channel unsubscribed; OK, no body, also for channel 0, a channel we do
/// not have or one not subscribed. Under 6 bytes inferno fails to read it:
/// no reply.
fn unsubscribe(h: &Header, req: &[u8], state: &mut DeviceState, cfg: &DeviceConfig) -> ArcOutcome {
    let Some(channel) = read_u16(req, 4) else {
        return unanswered(h, "an unsubscribe request without its channel");
    };
    // A channel we do not have the state refuses.
    let changed = usize::from(channel)
        .checked_sub(1)
        .filter(|&rx| set_subscription(state, cfg, rx, None));
    if changed.is_some() {
        info!(seq = h.seq, channel, "RX unsubscribed");
    }
    subscribed(h, changed.into_iter().collect())
}

/// A count or a channel number as the wire's u16 (the config allows far
/// fewer channels than that).
fn wire_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

/// 0x1000: inferno's layout.
fn counts(cfg: &DeviceConfig) -> Vec<u8> {
    let tx = wire_u16(cfg.tx.len());
    let rx = wire_u16(cfg.rx.len());
    let mut out = Body::new();
    out.u8(0);
    out.u8(FLAG_TX_RENAME);
    out.u16(tx);
    out.u16(rx);
    out.u16(4);
    out.u16(MAX_CHANNELS_IN_FLOW.min(tx));
    out.u16(8);
    // The most TX and RX flows.
    out.u16(MAX_FLOWS);
    out.u16(MAX_FLOWS);
    out.u16(tx.saturating_add(rx));
    out.u16(1);
    out.u16(1);
    out.zeros(12);
    out.into_vec()
}

/// 0x1003: a 38-byte header of offsets and constants, then the current
/// name, the factory name, the board name and the revision.
fn device_names(state: &DeviceState, cfg: &DeviceConfig) -> Vec<u8> {
    let mut out = Body::new();
    out.zeros(NAMES_HEADER);
    let name = out.cstr(&state.name);
    let factory = out.cstr(&cfg.name);
    let board = out.cstr(&cfg.model);
    let revision = out.cstr(REVISION);
    #[rustfmt::skip]
    let fields: [u16; NAMES_HEADER / 2] = [
        0, 0, 0, board, revision, 0, name, factory, name,
        0, 0, 0, 0, 0, 0,
        0x2729, 0, 0x1102, 0,
    ];
    for (i, value) in fields.into_iter().enumerate() {
        out.set_u16(wire_u16(HEADER_LEN + 2 * i), value);
    }
    out.into_vec()
}

/// The descriptor every channel of a page points at: rate, encoding, PCM
/// type.
fn common_descriptor(out: &mut Body, cfg: &DeviceConfig) -> u16 {
    let at = out.offset();
    let bits = u16::from(cfg.encoding);
    out.u32(cfg.sample_rate);
    out.u8(1);
    out.u8(1);
    out.u16(bits);
    out.u16(0x400);
    out.u16(bits);
    out.u16(bits);
    out.u16(u16::from(PCM_TYPE));
    debug_assert_eq!(usize::from(out.offset() - at), COMMON_DESCRIPTOR);
    at
}

/// What a page's items write behind the descriptor table: their strings
/// and descriptors, as packet offsets that are final when written (the
/// table's room is reserved first), and one descriptor every item may
/// share, written once, where the first item that asks for it stands.
struct Heap {
    body: Body,
    shared: Option<u16>,
}

impl Heap {
    /// The shared descriptor's offset, `write` writing it the first time.
    fn shared(&mut self, write: impl FnOnce(&mut Body) -> u16) -> u16 {
        match self.shared {
            Some(at) => at,
            None => *self.shared.insert(write(&mut self.body)),
        }
    }
}

impl std::ops::Deref for Heap {
    type Target = Body;
    fn deref(&self) -> &Body {
        &self.body
    }
}

impl std::ops::DerefMut for Heap {
    fn deref_mut(&mut self) -> &mut Body {
        &mut self.body
    }
}

/// One page of a list: `[slots, count]`, a table of `slots` descriptors of
/// `item` bytes (unused ones zero), then the heap they point into. The
/// request names the page's first item (from 1) in its body's bytes 2–3.
/// `write` writes one item's strings and descriptors to the heap and
/// returns its table descriptor, or `None` for an item that is not listed
/// (an empty flow slot: passed over, taking no room). More follow
/// (`0x8112`) when an item is left once the table is full, or when an item
/// has brought the reply to [`SOFT_LIMIT`] bytes: that item stands in the
/// table but is not counted (Dante Controller asks for it again on the
/// next page).
fn page<I>(
    h: &Header,
    req: &[u8],
    slots: usize,
    item: usize,
    items: impl IntoIterator<Item = I>,
    write: impl FnMut(I, &mut Heap) -> Option<Vec<u8>>,
) -> ArcOutcome {
    let Some(first) = first_item(req) else {
        return refuse(h, "a page request without its first item");
    };
    paged(h, 0, first, slots, item, items, write)
}

/// [`page`]'s reply from item `first` (from 1, not 0), with `prefix` zero
/// bytes before `[slots, count]` (ARC 2.8.9's pages have 6, [`v289`]).
fn paged<I>(
    h: &Header,
    prefix: usize,
    first: u16,
    slots: usize,
    item: usize,
    items: impl IntoIterator<Item = I>,
    mut write: impl FnMut(I, &mut Heap) -> Option<Vec<u8>>,
) -> ArcOutcome {
    let mut heap = Heap {
        body: Body::new(),
        shared: None,
    };
    heap.zeros(prefix + 2 + slots * item);
    let mut table: Vec<Vec<u8>> = Vec::with_capacity(slots);
    let mut counted = 0;
    let mut left = items.into_iter().skip(usize::from(first) - 1);
    let more = loop {
        let Some(it) = left.next() else {
            break false;
        };
        if table.len() == slots {
            break true;
        }
        let Some(descriptor) = write(it, &mut heap) else {
            continue;
        };
        debug_assert_eq!(descriptor.len(), item);
        table.push(descriptor);
        if usize::from(heap.offset()) >= SOFT_LIMIT {
            break true;
        }
        counted += 1;
    };
    let mut body = heap.body.into_vec();
    body[prefix] = slots as u8;
    body[prefix + 1] = counted as u8;
    let table_at = prefix + 2;
    for (i, descriptor) in table.iter().enumerate() {
        body[table_at + i * item..table_at + (i + 1) * item].copy_from_slice(descriptor);
    }
    let status = if more { STATUS_MORE } else { STATUS_OK };
    ArcOutcome::Reply(reply(h, status, &body))
}

/// 0x2000: each TX channel with its default name.
fn tx_channels(h: &Header, req: &[u8], cfg: &DeviceConfig) -> ArcOutcome {
    let n = cfg.tx.len();
    page(h, req, n.min(TX_PAGE), 8, 0..n, |index, out| {
        let common = out.shared(|b| common_descriptor(b, cfg));
        let name = out.cstr(&cfg.tx[index]);
        Some(descriptor(&[wire_u16(index + 1), 7, common, name]))
    })
}

/// 0x2010: each TX channel `listed` takes (by index) with its current
/// name: every one under 2.7, the renamed ones under 2.8.9 ([`v289`]).
fn tx_names(
    h: &Header,
    req: &[u8],
    state: &DeviceState,
    listed: impl Fn(usize) -> bool,
) -> ArcOutcome {
    let n = state.tx.len();
    page(h, req, n.min(TX_PAGE), 6, 0..n, |index, out| {
        if !listed(index) {
            return None;
        }
        // A zero word heads the names (written once, before the first).
        out.shared(|b| {
            let at = b.offset();
            b.u32(0);
            at
        });
        let id = wire_u16(index + 1);
        let name = out.cstr(&state.tx[index]);
        Some(descriptor(&[id, id, name]))
    })
}

/// Our RX flow in `slot` once its transmitter granted it (an asked flow
/// is listed from its grant), as 0x3200 and ARC 2.8.9's 0x3600 list it.
fn granted_flow(rx: &Manager, slot: usize) -> Option<crate::subscribe::FlowView> {
    rx.flow(slot).filter(|f| f.handle.is_some())
}

/// RX channel `rx`'s subscription as 0x3000 lists it, with its status
/// code: the manager's status when it holds the same subscription, else
/// Unresolved (a subscription saved a moment ago, not handed over yet).
fn listed_subscription<'s>(
    state: &'s DeviceState,
    rx: usize,
    manager: &Manager,
) -> Option<(&'s Subscription, u32)> {
    let sub = state.subscription(rx)?;
    let status = match manager.subscription(rx) {
        Some(known) if known == sub => manager.status(rx).unwrap_or(SubStatus::Unresolved),
        _ => SubStatus::Unresolved,
    };
    Some((sub, status.code()))
}

/// 0x3000 (inferno's `get_receive_channels`): each RX channel with, when
/// subscribed, the TX channel's and the TX device's names and the status
/// code, then its own name.
fn rx_channels(
    h: &Header,
    req: &[u8],
    state: &DeviceState,
    cfg: &DeviceConfig,
    manager: &Manager,
) -> ArcOutcome {
    let n = state.rx.len();
    page(h, req, n.min(RX_PAGE), 20, 0..n, |index, out| {
        let common = out.shared(|b| common_descriptor(b, cfg));
        let (tx_channel, tx_device, status) = match listed_subscription(state, index, manager) {
            Some((sub, status)) => (out.cstr(&sub.tx_channel), out.cstr(&sub.tx_device), status),
            None => (0, 0, 0),
        };
        let name = out.cstr(&state.rx[index]);
        let [s0, s1] = [(status >> 16) as u16, status as u16];
        Some(descriptor(&[
            wire_u16(index + 1),
            RX_CHANNEL_FLAGS,
            common,
            tx_channel,
            tx_device,
            name,
            s0,
            s1,
            0,
            0,
        ]))
    })
}

fn descriptor(fields: &[u16]) -> Vec<u8> {
    fields.iter().flat_map(|f| f.to_be_bytes()).collect()
}

/// 0x2013 and 0x3001: `[?, count]`, then `count` items (TX: `0, channel,
/// name offset`; RX: `channel, name offset`), the names behind them,
/// renamed ([`rename_channels`]): OK with `00 00` (TX) or no body (RX).
fn rename(
    h: &Header,
    req: &[u8],
    state: &mut DeviceState,
    cfg: &DeviceConfig,
    dir: Dir,
) -> ArcOutcome {
    let (item, skip) = match dir {
        Dir::Tx => (6, 2),
        Dir::Rx => (4, 0),
    };
    let count = req.get(1).map_or(0, |&c| usize::from(c));
    let mut items = Vec::with_capacity(count);
    for k in 0..count {
        let at = 2 + k * item + skip;
        let (Some(id), Some(offset)) = (read_u16(req, at), read_u16(req, at + 2)) else {
            return refuse(h, "a rename whose items are cut short");
        };
        match new_name(req, id, offset, dir) {
            Ok(name) => items.push((id, name)),
            Err(why) => return refuse(h, why),
        }
    }
    let ok: &[u8] = match dir {
        Dir::Tx => &[0, 0],
        Dir::Rx => &[],
    };
    match rename_channels(&items, state, cfg, dir) {
        Ok(change) => renamed(h, ok, change),
        Err(why) => refuse(h, why),
    }
}

/// The new name of channel `id` at packet offset `offset` in `req`; at the
/// datagram's end an empty name (netaudio 0.4.2's reset sends no byte
/// after it, dstack #220); else the reason (no name there).
fn new_name(req: &[u8], id: u16, offset: u16, dir: Dir) -> Result<&str, &'static str> {
    let at_end = usize::from(offset) == HEADER_LEN + req.len();
    string_at(req, offset)
        .or(at_end.then_some(""))
        .ok_or_else(|| {
            debug!(
                dir = dir.label(),
                channel = id,
                "rename refused: no name at its offset"
            );
            "no name at its offset"
        })
}

/// Status OK with `body`; [`ArcOutcome::Changed`] with `change` when a
/// write changed the state.
fn renamed(h: &Header, body: &[u8], change: Option<Change>) -> ArcOutcome {
    let bytes = reply(h, STATUS_OK, body);
    match change {
        Some(change) => ArcOutcome::Changed(bytes, change),
        None => ArcOutcome::Reply(bytes),
    }
}

/// Renames `items` (the channel from 1, its new name) in direction `dir`,
/// all or nothing: the reason when one cannot be made (no item, no such
/// channel, a name Dante's rules refuse, names that would clash), else the
/// change, `None` when every name was already so. An empty name is the
/// channel's default. A
/// refusal's reason is a debug line only: the refusal is a gap the server's
/// once-a-minute warning counts, and Dante Controller asks again at once,
/// so a warning per request would be a flood. 2.7's 0x2013 and 0x3001 and
/// 2.8.9's 0x3401 and 0x2438 (`arc/v289/writes.rs`) share it.
fn rename_channels(
    items: &[(u16, &str)],
    state: &mut DeviceState,
    cfg: &DeviceConfig,
    dir: Dir,
) -> Result<Option<Change>, &'static str> {
    let (names, defaults) = match dir {
        Dir::Tx => (&mut state.tx, &cfg.tx),
        Dir::Rx => (&mut state.rx, &cfg.rx),
    };
    let label = dir.label();
    if items.is_empty() {
        return Err("a rename without a channel");
    }
    let mut proposed = names.clone();
    let mut renamed = Vec::with_capacity(items.len());
    for &(id, name) in items {
        let Some(index) = usize::from(id)
            .checked_sub(1)
            .filter(|&i| i < proposed.len())
        else {
            debug!(dir = label, channel = id, "rename refused: no such channel");
            return Err("no such channel");
        };
        let name = if name.is_empty() {
            defaults[index].as_str()
        } else {
            name
        };
        if !valid_channel_name(name) {
            debug!(
                dir = label,
                channel = id,
                "rename refused: not a valid Dante channel name"
            );
            return Err("not a valid Dante channel name");
        }
        proposed[index] = name.to_string();
        renamed.push(id);
    }
    let problems = channel_problems(
        label,
        &proposed,
        (dir == Dir::Tx).then_some(defaults.as_slice()),
    );
    if !problems.is_empty() {
        debug!(dir = label, problems = %problems.join("; "), "rename refused");
        return Err("the names would clash");
    }
    if proposed == *names {
        return Ok(None);
    }
    // The channel-change notification names the RX channels renamed (inferno
    // also sends one, empty, for a rename that renamed nothing).
    let renamed_rx = match dir {
        Dir::Rx => (0..names.len())
            .filter(|&i| names[i] != proposed[i])
            .collect(),
        Dir::Tx => Vec::new(),
    };
    *names = proposed;
    info!(dir = label, channels = ?renamed, "channels renamed");
    Ok(Some(Change {
        renamed_rx,
        ..Change::default()
    }))
}

#[cfg(test)]
mod tests;
