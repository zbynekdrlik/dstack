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
//! | 0x1000 | counts: TX, RX, the TX and RX flows the device has (its config's `flow_slots`, at most [`MAX_FLOWS`], dstack #227) |
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
//! | 0x2201, 0x2202 | ARC 2.7's create, delete of a multicast TX flow (Dante Controller's on a device without segmented flows, dstack #222): the flow made in the slot its id names, or ended ([`answer`]); OK, no body. [`handle`] alone, which cannot change the TX table, refuses them as inferno does when it creates or deletes none |
//! | 0x2600 (start 0x2809) | ARC 2.8.9's TX flow list ([`flows::list_reply_page`]): receivers' flows unicast, ours multicast with their group; paged from the request's first flow, each page one datagram (dstack #227) |
//! | 0x2601 (start 0x2809) | ARC 2.8.9's multicast create (dstack #193): a multicast flow in the asked slot ([`answer`]); the reply is its record, no destination, in a table of our flow slots (dstack #227) |
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
//! ([`MAX_DATAGRAM`](crate::wire::MAX_DATAGRAM)): no item is longer than
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
use crate::device::{Device, DeviceConfig, DeviceState, Subscription, valid_device_name};
use crate::info::Changes;
use crate::media::tx::{TxShared, TxTable};
use crate::subscribe::Manager;
use crate::wire::{Body, Header, STATUS_ERROR, STATUS_MORE, STATUS_OK, WireError, read_u16, reply};
use gaps::{Gap, Gaps};

mod channels;
pub mod client;
pub mod control;
mod counts;
pub mod flows;
pub mod gaps;
mod multicast;
mod settings;
mod v289;
mod writes;

use channels::{
    Heap, common_descriptor, granted_flow, listed_subscription, page, paged, rx_channels,
    tx_channels, tx_names,
};
use counts::{counts, device_names, flow_slots, table_slots};
use settings::{key_table, settings_body};
use writes::{rename, subscribe, unsubscribe};

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
/// The most flows each way (inferno's TX and RX `MAX_FLOWS`); a device
/// has its config's `flow_slots` of them (dstack #227).
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
/// A flow socket descriptor's first word: its length in bytes (8) and
/// its address family (2, IPv4), then the port and the IPv4 address. A
/// real ARC 2.7 card writes `08 02`, netaudio reads the two bytes so, and
/// inferno's own flow request writes it (dstack #274; up to v0.6.122 we
/// wrote inferno's ARC server's `0x8002`, which netaudio reads as a
/// 128-byte descriptor).
const SOCKET_DESCRIPTOR: u16 = 0x0802;
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
/// An RX channel's flags in both generations' records (0x3000's second
/// word, 0x3400's flags; a reader sees no change from the generation):
/// inferno's 6 (netaudio: rename allowed) and 0x0008, netaudio's
/// can-subscribe-self bit (dstack #220). Our RX channels take a
/// subscription to our own TX, and netaudio refuses to send one without
/// the bit.
const RX_CHANNEL_FLAGS: u16 = 0x000e;
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
        flows::LIST if h.start == flows::START_2809 => flows_2809(&h, body, cfg, live.tx),
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
/// delete (0x2601, 0x2602 under [`flows::START_2809`], dstack #193) and
/// ARC 2.7's create and delete (0x2201, 0x2202 under any start code,
/// dstack #222) with
/// the TX table to change ([`Flows::change_tx`]), saved to the state file
/// before it is answered (dstack #207: a restart makes the flow again); a
/// change that cannot be saved is undone, on the TX table too, and
/// refused, as [`respond`] does for a rename. Anything else by [`respond`]
/// with the flows as they are.
pub fn answer<F: Flows>(device: &mut Device, running: &F, req: &[u8]) -> ArcOutcome {
    match Header::parse(req) {
        Ok((h, body))
            if h.op2 == 0
                && (matches!(h.op1, flows::CREATE_27 | flows::DELETE_27)
                    || h.start == flows::START_2809
                        && matches!(h.op1, flows::CREATE | flows::DELETE)) =>
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

/// A flow socket descriptor: `08 02` ([`SOCKET_DESCRIPTOR`]), the port,
/// the IPv4 address.
fn socket_descriptor(out: &mut Body, at: std::net::SocketAddrV4) {
    out.u16(SOCKET_DESCRIPTOR);
    out.u16(at.port());
    out.bytes(&at.ip().octets());
}

/// Our TX flows as an ARC 2.8.9 list (0x2600, [`flows::list_reply_page`]):
/// each a unicast flow to its receiver or a multicast flow to its group
/// (dstack #193), its slot (from 1) as both ids. Paged (dstack #227): the
/// page lists the flows from the one the request names (body bytes 10–11,
/// [`flows::list_body_from`]; a request without it, or with 0, from flow
/// 1, as every request was answered before pages) as far as one datagram
/// holds them, status [`STATUS_MORE`] when flows are left. A reader up to
/// v0.6.97 asks once, from flow 1, and takes status OK only: it reads our
/// list while every flow fits page one (21 flows of 8 channels, 24 of 4).
fn flows_2809(h: &Header, req: &[u8], cfg: &DeviceConfig, tx: &TxTable) -> ArcOutcome {
    let first = read_u16(req, 10).map_or(1, |f| f.max(1));
    let listed: Vec<flows::TxFlow> = (usize::from(first) - 1..usize::from(MAX_FLOWS))
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
    let (body, more) = flows::list_reply_page(table_slots(cfg), &listed);
    let status = if more { STATUS_MORE } else { STATUS_OK };
    ArcOutcome::Reply(reply(h, status, &body))
}

/// 0x2200: the TX flows receivers asked for on 4455, expired ones too
/// (listed until a new destination takes the slot), by slot. Each flow,
/// from a multiple of 4: the flow descriptor (flow, `0x11` unicast or
/// `0x02` multicast (dstack #193), our rate, 0, our encoding, 1, the
/// slots, the socket descriptor's offset, each slot's TX channel or 0, the
/// names descriptor's offset); at a multiple of 4 the socket descriptor
/// (the receiver's address, a multicast flow's group); the names
/// descriptor (`0x0a00, 1`, the receiver's device and flow names, the
/// flow's frames per packet (dstack #274: a real 2.7 card states each
/// flow's; we wrote a fixed 0x10 up to v0.6.122), ours, 8 zeros); then
/// the three names: ours `<flow>_<process>`, the receiver's device and
/// flow names (cut to [`MAX_REMOTE_NAME`]). A reader
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
            out.set_u16(names + 8, f.fpp);
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

/// A count or a channel number as the wire's u16 (the config allows far
/// fewer channels than that).
fn wire_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests;
