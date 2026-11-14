// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! A transmitter's channels read through its ARC (Gen 3 phase B, plan Task
//! 9b), for a device that publishes no `_netaudio-chan` record: the band's
//! Yamaha Dante cards publish only `_netaudio-arc` (UDP 4440),
//! `_netaudio-cmc` and `_netaudio-dbc` (flow control, 4455), so a
//! subscription to one of their channels never resolved (#72, 2026-10-26).
//!
//! - [`Book`] (pure, times in ns): the devices' `_netaudio-arc` and
//!   `_netaudio-dbc` records ([`Book::set_record`], at most
//!   [`MAX_RECORDS`]), and for each device a
//!   lookup asked about (at most [`MAX_DEVICES`]) its directory and its
//!   reads. [`Book::lookup`] answers only from a directory read within
//!   [`Timing::max_age_ns`]; otherwise it registers a want and answers
//!   nothing. [`Book::next`] hands out the next read, [`Book::finished`]
//!   takes its outcome.
//! - [`ArcDirectory`]: the [`Resolver`] the manager calls every tick. A
//!   lookup only reads the book and registers a want, never the network.
//!   [`ArcDirectory::start`] (once, after a lookup first reached it, by the
//!   daemon): the direct queries of the two records of each device looked
//!   up ([`super::instance`], dstack #141: no browse of a whole service,
//!   whose answers and their refreshes burst over the whole network), and
//!   **one** reader thread that performs the reads one after another
//!   ([`crate::arc::client`]). Every lookup first takes the queries'
//!   answers for its device into the book.
//! - [`Network`]: the channel's own record first, by its direct query; a
//!   channel with no answer within the query's grace is read through its
//!   device's ARC. The daemon's resolver is `(Own, Network)`.
//!
//! **Bounds (plan Review Focus 1, and the lesson of Dante Controller's loop
//! against our refusals):** one reader for every device, so at most one
//! ARC exchange is in flight on the whole device; each request waits at
//! most [`crate::arc::client::REPLY_TIMEOUT`] (1 s), and a refusal, no
//! reply or a page that does not read ends the read (nothing is asked
//! again inside it); a read is at most 1 + 2 × [`MAX_PAGES`] requests (the
//! IEM card's is 6). A device's next read starts no earlier than
//! [`retry_after`]`(failures)` after its last one ended (the manager's 9,
//! 18, 36 and 72 s): a failed read, and a read asked because a name was not
//! listed, count a failure; a read of a missing or old directory that
//! succeeds clears them. A device's ARC is read only with both its records
//! known, its `_netaudio-arc` one for [`Timing::settle_ns`], and only for a
//! channel whose own record did not answer ([`Network`]).
//!
//! **What an ARC-resolved channel offers** ([`offer`]): flow control at the
//! `_netaudio-dbc` record's address and port; the id, rate and bits its
//! 0x2000 page gives; [`NCHAN`] channel slots, or the device's TX channels
//! when it has fewer ([`offer_in`]); [`Timing::fpp`] frames per
//! packet (`fpp_min = fpp_max`: the device config's `arc_rx_fpp`, [`FPP`]
//! when it is left out; a packet leaves the transmitter only after its last
//! frame, so fewer frames cut our RX lateness, #72 phase F); `dbcp1` 0x1102
//! (our client's start code whatever); `latency_ns` 0, so the manager plays
//! out at our own latency (`max(its, ours)`): ARC names neither frames per
//! packet nor latency (the card's captured pages; netaudio reads neither
//! from them).
//!
//! **TX flow lists** (dstack #148): [`Book::multicast`] answers whether a
//! multicast flow of a transmitter carries a channel, from the device's
//! TX flow list ([`crate::arc::flows::read_tx_flows`]: 2.8.9's 0x2600, or
//! 2.7's 0x2200 for a device that refuses 0x2600 as unknown, then asked
//! first for that device, dstack #271) read by the same
//! reader, one exchange, within the same bounds: one read at a time, a list
//! kept [`Timing::max_age_ns`], the next read [`retry_after`] its failures
//! after the last. It is asked for every subscribed channel whose device has
//! a `_netaudio-arc` record, whichever record resolved the channel. A list
//! that cannot be read (a device that answers neither list) counts as one
//! with no multicast flow until the next read; a device's first refusal is
//! logged at info, later ones at debug.
//!
//! Logs name peer ids (the status's), counts and opcodes, never a name or
//! an address: one info line per directory read, and once a minute at most
//! the reads, failures and lookups that found nothing ([`Counts`]).

use std::collections::BTreeMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[cfg(test)]
use mdns_sd::ServiceEvent;
use tracing::{debug, info};

use super::instance::{Direct, Instances};
use super::resolve::{Advertised, Applied, MulticastLookup, Resolver};
use super::retry_after;
use crate::announce::ARC_SERVICE;
use crate::arc::client::{Directory, REPLY_TIMEOUT, ReadError, TxChannel, Udp, read_directory};
use crate::arc::flows::{Flows, ListForm, read_tx_flows};
use crate::ctl::Requests;
use crate::device::DeviceConfig;
use crate::flows::control::START as DBCP1;

pub use crate::arc::client::MAX_PAGES;

mod flows;
pub use flows::carried;
use flows::flows_read;

/// A Dante device's flow-control record (UDP 4455).
pub const DBC_SERVICE: &str = "_netaudio-dbc._udp.local.";
/// The frames per packet an ARC-resolved transmitter is asked for when the
/// device config leaves `arc_rx_fpp` out ([`Timing::fpp`]): the IEM card's
/// performance property 0x0210 reads 16, most likely its transmit frames per
/// packet (evidence, not proof); ARC names none.
pub const FPP: u16 = 16;
/// The channel slots of a flow from an ARC-resolved transmitter: the IEM
/// card's own flows to a rack carry 4; a transmitter that takes 8 takes 4.
pub const NCHAN: u16 = 4;
/// How long a device's `_netaudio-arc` record is known before its ARC is
/// read.
pub const SETTLE_NS: u64 = 5_000_000_000;
/// How long a directory answers after its read.
pub const MAX_AGE_NS: u64 = 60_000_000_000;
/// The most devices whose records are kept.
pub const MAX_RECORDS: usize = 256;
/// How long after the first multicast lookup a device with no
/// `_netaudio-arc` record yet is waited for ([`Book::multicast`]): the
/// direct query's first answers come within it, so no unicast flow is asked for
/// at start before a transmitter's multicast flows are known.
pub const RECORD_GRACE_NS: u64 = 2_000_000_000;
/// The most devices a lookup asked about (their directories and reads).
pub const MAX_DEVICES: usize = 64;
/// The reader looks at least this often at the minute's report (a new
/// want and the stop wake it at once).
pub const IDLE: Duration = Duration::from_secs(1);
/// The counts are logged at most once this often.
pub const REPORT_NS: u64 = 60_000_000_000;

/// The directory's times (tests shorten them) and the frames per packet it
/// offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    pub settle_ns: u64,
    pub max_age_ns: u64,
    /// How long one request waits for its reply.
    pub reply: Duration,
    /// The reader's longest wait ([`IDLE`]).
    pub idle: Duration,
    /// The frames per packet asked of an ARC-resolved transmitter, the
    /// device config's `arc_rx_fpp` ([`FPP`] by default).
    pub fpp: u16,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            settle_ns: SETTLE_NS,
            max_age_ns: MAX_AGE_NS,
            reply: REPLY_TIMEOUT,
            idle: IDLE,
            fpp: FPP,
        }
    }
}

impl Timing {
    /// The default times with the frames per packet `cfg` asks of an
    /// ARC-resolved transmitter (its `arc_rx_fpp`).
    pub fn of(cfg: &DeviceConfig) -> Self {
        Timing {
            fpp: cfg.arc_rx_fpp.unwrap_or(FPP),
            ..Timing::default()
        }
    }
}

/// What a channel of `ch` offers the manager, with flow control at `dbc`
/// and `fpp` frames per packet.
pub fn offer(ch: &TxChannel, dbc: SocketAddrV4, fpp: u16) -> Advertised {
    Advertised {
        control: dbc,
        tx_channel_id: ch.id,
        nchan: NCHAN,
        bits: ch.bits,
        rate: ch.rate,
        dbcp1: DBCP1,
        fpp_min: fpp,
        fpp_max: fpp,
        latency_ns: 0,
    }
}

/// What `ch` of the device whose directory is `dir` offers: [`offer`], with
/// no more slots than the device has TX channels, as Dante's own records
/// count them (`min(the most a flow carries, the TX channels)`). A
/// 2-channel wireless mic refused a flow of 4 slots with 0x0311 (iemmixer,
/// 31.10.2026, dstack #1).
pub fn offer_in(dir: &Directory, ch: &TxChannel, dbc: SocketAddrV4, fpp: u16) -> Advertised {
    let channels = u16::try_from(dir.channels().len()).unwrap_or(u16::MAX);
    Advertised {
        nchan: NCHAN.min(channels),
        ..offer(ch, dbc, fpp)
    }
}

/// A device's records.
#[derive(Debug, Clone, Default)]
struct Records {
    arc: Option<SocketAddrV4>,
    /// Every IPv4 address of its `_netaudio-arc` record (at most
    /// [`MAX_ARC_ADDRESSES`]), lowest first; `arc` holds the lowest.
    arc_ips: Vec<Ipv4Addr>,
    dbc: Option<SocketAddrV4>,
    /// When its `_netaudio-arc` record was kept (again).
    since: u64,
}

/// The most IPv4 addresses kept of a device's `_netaudio-arc` record (a PC
/// with several network cards announces one per card).
pub const MAX_ARC_ADDRESSES: usize = 8;

/// Why a device's ARC is not at an address ([`Book::arc_at`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArcMiss {
    /// No `_netaudio-arc` record of the device.
    NoRecord,
    /// Its record announces other addresses only.
    OtherAddress,
}

/// Why a read is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Want {
    /// No directory yet.
    Missing,
    /// The directory is older than [`Timing::max_age_ns`].
    Old,
    /// A name the directory does not list.
    NotListed,
}

/// Where a device's reads stand.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum State {
    /// Nothing asked for.
    #[default]
    Idle,
    /// A lookup wants a read (one reason, the first).
    Wanted(Want),
    /// A read runs; no lookup asks for another meanwhile.
    Reading,
}

/// A device's directory and reads.
#[derive(Debug, Default)]
struct Reads {
    /// The directory and when its read ended.
    directory: Option<(Directory, u64)>,
    state: State,
    /// When the last read ended.
    ended: Option<u64>,
    /// Failures in a row.
    failures: u32,
}

/// A device's TX flow lists (dstack #148): the last one and when it was
/// read, and its reads, as [`Reads`] for its channels.
#[derive(Debug, Default)]
struct FlowReads {
    flows: Option<(Flows, u64)>,
    state: State,
    ended: Option<u64>,
    failures: u32,
    /// The list form the device last answered, asked first (dstack #271).
    form: ListForm,
    /// A refusal was logged at info (once per device).
    refusal_told: bool,
}

/// What a read reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The TX channels (the directory).
    Channels,
    /// The TX flows (0x2600), for their multicast flows.
    Flows,
}

/// A read to make: the device (its name, case aside), its ARC, why, and
/// what it reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub device: String,
    pub arc: SocketAddrV4,
    pub want: Want,
    pub kind: Kind,
    /// The TX flow list form asked first ([`Kind::Flows`]; the default
    /// for a channel read).
    pub form: ListForm,
}

/// A read's outcome.
#[derive(Debug)]
pub struct Outcome {
    pub result: Result<Directory, ReadError>,
    /// The requests it sent.
    pub requests: usize,
}

/// What the directory did, cumulative (the minute's report is the
/// difference).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Directory reads that ended with a directory.
    pub reads: u64,
    /// Reads that ended without one, by why.
    pub no_reply: u64,
    pub refused: u64,
    pub malformed: u64,
    pub socket: u64,
    /// Requests sent.
    pub requests: u64,
    /// Lookups of a name a fresh directory does not list.
    pub not_listed: u64,
    /// Lookups of a device without both records, or past [`MAX_DEVICES`].
    pub unknown: u64,
}

impl Counts {
    /// What happened since `before`.
    pub fn since(&self, before: &Counts) -> Counts {
        Counts {
            reads: self.reads - before.reads,
            no_reply: self.no_reply - before.no_reply,
            refused: self.refused - before.refused,
            malformed: self.malformed - before.malformed,
            socket: self.socket - before.socket,
            requests: self.requests - before.requests,
            not_listed: self.not_listed - before.not_listed,
            unknown: self.unknown - before.unknown,
        }
    }

    /// The reads that ended without a directory.
    pub fn failed(&self) -> u64 {
        self.no_reply + self.refused + self.malformed + self.socket
    }

    /// A read that ended with `e`, counted by why.
    fn count_error(&mut self, e: ReadError) {
        match e {
            ReadError::Timeout { .. } | ReadError::Stopped => self.no_reply += 1,
            ReadError::Refused { .. } => self.refused += 1,
            ReadError::Malformed { .. } => self.malformed += 1,
            ReadError::Io { .. } => self.socket += 1,
        }
    }
}

/// The devices' records, directories and reads (see the module).
#[derive(Debug)]
pub struct Book {
    timing: Timing,
    records: BTreeMap<String, Records>,
    reads: BTreeMap<String, Reads>,
    flow_reads: BTreeMap<String, FlowReads>,
    /// When [`Book::multicast`] was first asked about each device (case
    /// aside) that had no `_netaudio-arc` record then, at most
    /// [`MAX_DEVICES`].
    first_asked: BTreeMap<String, u64>,
    /// Where the flow list of a device with no `_netaudio-arc` record is
    /// read: its channel's flow-control address on ARC's port (dstack
    /// #148: the DT_20 answers no query for that record).
    flow_arcs: BTreeMap<String, SocketAddrV4>,
    counts: Counts,
    /// Wants registered so far (the reader is woken when it moves).
    wants: u64,
}

/// Which of the two services a record is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    /// `_netaudio-arc`.
    Arc,
    /// `_netaudio-dbc`.
    Dbc,
}

/// The service `ty` names, and the device of its record `fullname` (case
/// aside).
#[cfg(test)]
fn record_of(ty: &str, fullname: &str) -> Option<(Service, String)> {
    let service = if ty.eq_ignore_ascii_case(ARC_SERVICE) {
        Service::Arc
    } else if ty.eq_ignore_ascii_case(DBC_SERVICE) {
        Service::Dbc
    } else {
        return None;
    };
    let name = fullname.to_ascii_lowercase();
    let device = name
        .strip_suffix(&ty.to_ascii_lowercase())?
        .strip_suffix('.')
        .filter(|d| !d.is_empty())?;
    Some((service, device.to_string()))
}

impl Book {
    pub fn new(timing: Timing) -> Self {
        Book {
            timing,
            records: BTreeMap::new(),
            reads: BTreeMap::new(),
            flow_reads: BTreeMap::new(),
            first_asked: BTreeMap::new(),
            flow_arcs: BTreeMap::new(),
            counts: Counts::default(),
            wants: 0,
        }
    }

    pub fn counts(&self) -> Counts {
        self.counts
    }

    /// Wants registered so far.
    pub fn wants(&self) -> u64 {
        self.wants
    }

    /// Keeps what a browse event of `_netaudio-arc` or `_netaudio-dbc` says,
    /// at `now` (the tests' records).
    #[cfg(test)]
    pub fn apply(&mut self, ev: &ServiceEvent, now: u64) -> Applied {
        match ev {
            ServiceEvent::ServiceResolved(r) => {
                let Some((service, device)) = record_of(&r.ty_domain, r.get_fullname()) else {
                    return Applied::Ignored;
                };
                self.set_record(service, &device, Some((r.get_port(), &ipv4s(r))), now)
            }
            ServiceEvent::ServiceRemoved(ty, fullname) => {
                let Some((service, device)) = record_of(ty, fullname) else {
                    return Applied::Ignored;
                };
                self.set_record(service, &device, None, now)
            }
            _ => Applied::Ignored,
        }
    }

    /// Keeps `device`'s (case aside) record of `service` at `now`: its port
    /// and addresses, or `None` when it is gone. A record with no address
    /// is not kept; of its addresses the lowest [`MAX_ARC_ADDRESSES`] are.
    pub fn set_record(
        &mut self,
        service: Service,
        device: &str,
        record: Option<(u16, &[Ipv4Addr])>,
        now: u64,
    ) -> Applied {
        let device = device.to_ascii_lowercase();
        let Some((port, ips)) = record else {
            let Some(rec) = self.records.get_mut(&device) else {
                return Applied::Ignored;
            };
            let gone = match service {
                Service::Arc => {
                    rec.arc_ips.clear();
                    rec.arc.take()
                }
                Service::Dbc => rec.dbc.take(),
            };
            if rec.arc.is_none() && rec.dbc.is_none() {
                self.records.remove(&device);
            }
            return match gone {
                Some(_) => Applied::Removed,
                None => Applied::Ignored,
            };
        };
        let mut ips = ips.to_vec();
        ips.sort_unstable();
        ips.dedup();
        ips.truncate(MAX_ARC_ADDRESSES);
        let Some(&lowest) = ips.first() else {
            return Applied::Ignored;
        };
        if self.records.len() >= MAX_RECORDS && !self.records.contains_key(&device) {
            return Applied::Ignored;
        }
        let rec = self.records.entry(device).or_default();
        let at = Some(SocketAddrV4::new(lowest, port));
        match service {
            Service::Arc => {
                if rec.arc.is_none() {
                    rec.since = now;
                }
                rec.arc = at;
                rec.arc_ips = ips;
            }
            Service::Dbc => rec.dbc = at,
        }
        Applied::Resolved
    }

    /// `device`'s ARC (case aside) at `ip`: its `_netaudio-arc` record's
    /// port, when the record announces `ip` among its addresses.
    pub fn arc_at(&self, device: &str, ip: Ipv4Addr) -> Result<SocketAddrV4, ArcMiss> {
        let rec = self
            .records
            .get(&device.to_ascii_lowercase())
            .filter(|rec| rec.arc.is_some())
            .ok_or(ArcMiss::NoRecord)?;
        match (rec.arc, rec.arc_ips.contains(&ip)) {
            (Some(arc), true) => Ok(SocketAddrV4::new(ip, arc.port())),
            _ => Err(ArcMiss::OtherAddress),
        }
    }

    /// `channel` of `device` at `now`, from its directory when one was read
    /// within [`Timing::max_age_ns`]; otherwise a read is wanted (unless one
    /// runs) and the answer is `None`. A device without both records, or
    /// whose `_netaudio-arc` record is younger than [`Timing::settle_ns`],
    /// is not read.
    pub fn lookup(&mut self, device: &str, channel: &str, now: u64) -> Option<Advertised> {
        let key = device.to_ascii_lowercase();
        let Some(&Records {
            arc: Some(_),
            dbc: Some(dbc),
            since,
            ..
        }) = self.records.get(&key)
        else {
            self.counts.unknown += 1;
            return None;
        };
        if now.saturating_sub(since) < self.timing.settle_ns {
            return None;
        }
        if self.reads.len() >= MAX_DEVICES && !self.reads.contains_key(&key) {
            self.counts.unknown += 1;
            return None;
        }
        let reads = self.reads.entry(key).or_default();
        let want = match &reads.directory {
            Some((dir, at)) if now.saturating_sub(*at) < self.timing.max_age_ns => {
                match dir.find(channel) {
                    Some(ch) => return Some(offer_in(dir, ch, dbc, self.timing.fpp)),
                    None => {
                        self.counts.not_listed += 1;
                        Want::NotListed
                    }
                }
            }
            Some(_) => Want::Old,
            None => Want::Missing,
        };
        if reads.state == State::Idle {
            reads.state = State::Wanted(want);
            self.wants += 1;
        }
        None
    }

    /// The next read to make at `now`, if any device wants one and its wait
    /// is over; that device's read then runs until [`Book::finished`].
    pub fn next(&mut self, now: u64) -> Option<Job> {
        for (device, r) in &mut self.reads {
            let State::Wanted(want) = r.state else {
                continue;
            };
            if now < Self::due(r) {
                continue;
            }
            // A device whose ARC record went meanwhile is not read: its want
            // is dropped (a lookup asks again once the record is back).
            let Some(arc) = self.records.get(device).and_then(|rec| rec.arc) else {
                r.state = State::Idle;
                continue;
            };
            r.state = State::Reading;
            return Some(Job {
                device: device.clone(),
                arc,
                want,
                kind: Kind::Channels,
                form: ListForm::default(),
            });
        }
        for (device, r) in &mut self.flow_reads {
            let State::Wanted(want) = r.state else {
                continue;
            };
            if now
                < r.ended
                    .map_or(0, |t| t.saturating_add(retry_after(r.failures)))
            {
                continue;
            }
            let record = self.records.get(device).and_then(|rec| rec.arc);
            let Some(arc) = record.or_else(|| self.flow_arcs.get(device).copied()) else {
                r.state = State::Idle;
                continue;
            };
            r.state = State::Reading;
            return Some(Job {
                device: device.clone(),
                arc,
                want,
                kind: Kind::Flows,
                form: r.form,
            });
        }
        None
    }

    /// The multicast flow of `device` (case aside) that carries its TX
    /// channel `a` at `now`, from its flow list (dstack #148): read within
    /// [`Timing::max_age_ns`], or the last one while a new read is wanted;
    /// [`MulticastLookup::Unknown`] while the first read is wanted or runs.
    /// Only its `_netaudio-arc` record is needed, read at once (no settle:
    /// the list needs no other record). A device without that record is
    /// [`MulticastLookup::Unknown`] for [`RECORD_GRACE_NS`] after the first
    /// multicast lookup (its query may not have answered yet), then
    /// [`MulticastLookup::None`]; so is one past [`MAX_DEVICES`].
    pub fn multicast(&mut self, device: &str, a: &Advertised, now: u64) -> MulticastLookup {
        let key = device.to_ascii_lowercase();
        if !self.records.get(&key).is_some_and(|r| r.arc.is_some()) {
            if self.first_asked.len() >= MAX_DEVICES && !self.first_asked.contains_key(&key) {
                return MulticastLookup::None;
            }
            let first = *self.first_asked.entry(key.clone()).or_insert(now);
            if now.saturating_sub(first) < RECORD_GRACE_NS {
                return MulticastLookup::Unknown;
            }
            // No record within the grace: its ARC at the channel's address.
            self.flow_arcs.insert(
                key.clone(),
                SocketAddrV4::new(*a.control.ip(), crate::arc::PORT),
            );
        }
        if self.flow_reads.len() >= MAX_DEVICES && !self.flow_reads.contains_key(&key) {
            return MulticastLookup::None;
        }
        let max_age = self.timing.max_age_ns;
        let r = self.flow_reads.entry(key).or_default();
        let answer = match &r.flows {
            Some((flows, _)) => {
                carried(flows, a).map_or(MulticastLookup::None, MulticastLookup::Found)
            }
            None => MulticastLookup::Unknown,
        };
        let want = match &r.flows {
            Some((_, at)) if now.saturating_sub(*at) < max_age => return answer,
            Some(_) => Want::Old,
            None => Want::Missing,
        };
        if r.state == State::Idle {
            r.state = State::Wanted(want);
            self.wants += 1;
        }
        answer
    }

    /// The flow list read `job` ended at `now` with `result` after
    /// `requests` requests. A list that could not be read is a failure (the
    /// next read waits longer); the last list read stays (a multicast flow
    /// is not left over one lost reply), and a device never read counts as
    /// one with no flow until the next read (a device that answers no
    /// list gets unicast flows). The form that answered is asked first
    /// next time (dstack #271). True when the read was refused and the
    /// device had no refusal told yet: the reader logs that one at info,
    /// later ones at debug.
    pub fn finished_flows(
        &mut self,
        job: &Job,
        result: Result<(Flows, ListForm), ReadError>,
        requests: usize,
        now: u64,
    ) -> bool {
        self.counts.requests += requests as u64;
        let Some(r) = self.flow_reads.get_mut(&job.device) else {
            return false;
        };
        r.state = State::Idle;
        r.ended = Some(now);
        match result {
            Ok((list, form)) => {
                r.failures = 0;
                r.form = form;
                r.flows = Some((list, now));
                false
            }
            Err(e) => {
                let tell = matches!(e, ReadError::Refused { .. }) && !r.refusal_told;
                if tell {
                    r.refusal_told = true;
                }
                self.counts.count_error(e);
                r.failures = r.failures.saturating_add(1);
                if r.flows.is_none() {
                    r.flows = Some((Flows::default(), now));
                }
                tell
            }
        }
    }

    /// When the reader should look again: the earliest time a wanted read
    /// may start; `None` with nothing wanted.
    pub fn next_due(&self) -> Option<u64> {
        let flows = self
            .flow_reads
            .values()
            .filter(|r| matches!(r.state, State::Wanted(_)))
            .map(|r| {
                r.ended
                    .map_or(0, |t| t.saturating_add(retry_after(r.failures)))
            });
        self.reads
            .values()
            .filter(|r| matches!(r.state, State::Wanted(_)))
            .map(Self::due)
            .chain(flows)
            .min()
    }

    /// When a device's next read may start: [`retry_after`] its failures
    /// after its last read ended.
    fn due(r: &Reads) -> u64 {
        r.ended
            .map_or(0, |t| t.saturating_add(retry_after(r.failures)))
    }

    /// The read `job` ended at `now` with `outcome`.
    pub fn finished(&mut self, job: &Job, outcome: Outcome, now: u64) {
        self.counts.requests += outcome.requests as u64;
        let Some(r) = self.reads.get_mut(&job.device) else {
            return;
        };
        r.state = State::Idle;
        r.ended = Some(now);
        match outcome.result {
            Ok(dir) => {
                self.counts.reads += 1;
                r.failures = match job.want {
                    Want::NotListed => r.failures.saturating_add(1),
                    Want::Missing | Want::Old => 0,
                };
                r.directory = Some((dir, now));
            }
            Err(e) => {
                self.counts.count_error(e);
                r.failures = r.failures.saturating_add(1);
            }
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the lookups and the reader share.
#[derive(Debug)]
struct Shared {
    book: Mutex<Book>,
    /// The reader waits on it; a new want or the stop wakes it.
    wake: Condvar,
    origin: Instant,
    stop: AtomicBool,
    timing: Timing,
}

impl Shared {
    /// ns since the directory was made.
    fn now(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

/// A record's IPv4 addresses ([`Book::set_record`] orders and bounds them).
#[cfg(test)]
fn ipv4s(r: &mdns_sd::ResolvedService) -> Vec<Ipv4Addr> {
    r.get_addresses()
        .iter()
        .filter_map(|a| match a.to_ip_addr() {
            std::net::IpAddr::V4(v4) => Some(v4),
            std::net::IpAddr::V6(_) => None,
        })
        .collect()
}

/// A device's ARC address from the directory's `_netaudio-arc` records
/// ([`ArcDirectory::records`]).
#[derive(Debug, Clone)]
pub struct ArcRecords {
    shared: Arc<Shared>,
    asked: Arc<AtomicBool>,
    records: Arc<Mutex<Instances>>,
}

impl ArcRecords {
    /// `device`'s ARC at `ip` ([`Book::arc_at`]). Asking marks the directory
    /// asked: the daemon then starts its queries, as for an RX lookup.
    pub fn arc_at(&self, device: &str, ip: Ipv4Addr) -> Result<SocketAddrV4, ArcMiss> {
        self.asked.store(true, Ordering::Relaxed);
        sync(&self.shared, &self.records, device);
        lock(&self.shared.book).arc_at(device, ip)
    }

    /// Keeps a browse event as a record (tests).
    #[cfg(test)]
    pub(crate) fn keep(&self, ev: &ServiceEvent) {
        lock(&self.shared.book).apply(ev, 0);
    }
}

/// The transmitters' channels read through their ARC (see the module).
#[derive(Debug)]
pub struct ArcDirectory {
    shared: Arc<Shared>,
    asked: Arc<AtomicBool>,
    /// The direct queries of the records of the devices looked up.
    records: Arc<Mutex<Instances>>,
    /// Its start was tried (once, whatever came of it).
    tried: bool,
    threads: Vec<JoinHandle<()>>,
}

impl Default for ArcDirectory {
    fn default() -> Self {
        Self::new(Timing::default())
    }
}

impl ArcDirectory {
    pub fn new(timing: Timing) -> Self {
        ArcDirectory {
            shared: Arc::new(Shared {
                book: Mutex::new(Book::new(timing)),
                wake: Condvar::new(),
                origin: Instant::now(),
                stop: AtomicBool::new(false),
                timing,
            }),
            asked: Arc::new(AtomicBool::new(false)),
            records: Arc::default(),
            tried: false,
            threads: Vec::new(),
        }
    }

    /// A handle on its `_netaudio-arc` records for another reader (the TX
    /// side's receiver latencies, [`crate::flows::receivers`]).
    pub fn records(&self) -> ArcRecords {
        ArcRecords {
            shared: Arc::clone(&self.shared),
            asked: Arc::clone(&self.asked),
            records: Arc::clone(&self.records),
        }
    }

    /// The timing it was made with.
    pub fn timing(&self) -> Timing {
        self.shared.timing
    }

    /// Whether a lookup has reached it (the daemon starts it then).
    pub fn asked(&self) -> bool {
        self.asked.load(Ordering::Relaxed)
    }

    pub fn started(&self) -> bool {
        !self.threads.is_empty()
    }

    /// Its start was tried: a start that failed is not tried again (the
    /// channels with records of their own still resolve).
    pub fn tried(&self) -> bool {
        self.tried
    }

    /// The directory's counts so far.
    pub fn counts(&self) -> Counts {
        lock(&self.shared.book).counts()
    }

    /// Starts the direct queries of the devices' `_netaudio-arc` and
    /// `_netaudio-dbc` records (from our address `ip` to `mdns`, mDNS's
    /// group or a test's responder) and the reader (reads from `ip`, every
    /// request counted in `requests`), once for this directory's life:
    /// `true` when it started now, `false` when it had.
    pub fn start(
        &mut self,
        ip: Ipv4Addr,
        mdns: SocketAddrV4,
        requests: Arc<Requests>,
    ) -> io::Result<bool> {
        if self.started() {
            return Ok(false);
        }
        self.tried = true;
        lock(&self.records).start_to(ip, mdns)?;
        self.start_reader(ip, requests)?;
        Ok(true)
    }

    /// The reader alone (reads from `ip`).
    fn start_reader(&mut self, ip: Ipv4Addr, requests: Arc<Requests>) -> io::Result<()> {
        let shared = Arc::clone(&self.shared);
        self.threads.push(
            std::thread::Builder::new()
                .name("dstack-arc-read".to_string())
                .spawn(move || reader(&shared, ip, &requests))?,
        );
        Ok(())
    }

    /// Waits for every thread to end (the stop asked, the daemon's events
    /// ended), at most `limit`.
    #[cfg(test)]
    fn join_within(&mut self, limit: Duration) {
        ask_stop(&self.shared);
        let deadline = Instant::now() + limit;
        for thread in self.threads.drain(..) {
            while !thread.is_finished() {
                assert!(Instant::now() < deadline, "a thread never ended");
                std::thread::sleep(Duration::from_millis(5));
            }
            thread.join().unwrap();
        }
    }
}

impl Drop for ArcDirectory {
    /// The reader sends nothing more and ends (woken at once, or after the
    /// request it waits on); the query thread ends with the last holder of
    /// the records (a receiver reader keeps a handle).
    fn drop(&mut self) {
        ask_stop(&self.shared);
    }
}

/// Asks the reader to stop and wakes it, under the book's lock: the reader
/// holds it from its look at the stop to its wait, so the stop never lands
/// in between and is lost.
fn ask_stop(shared: &Shared) {
    let _book = lock(&shared.book);
    shared.stop.store(true, Ordering::Relaxed);
    shared.wake.notify_all();
}

/// The book takes what the direct queries know of `device`'s two records
/// now (a record not fresh is dropped), once the queries run.
fn sync(shared: &Shared, records: &Mutex<Instances>, device: &str) {
    let records = lock(records);
    if !records.started() {
        return;
    }
    let found = [
        (Service::Arc, records.record(device, ARC_SERVICE)),
        (Service::Dbc, records.record(device, DBC_SERVICE)),
    ];
    let now = shared.now();
    let mut book = lock(&shared.book);
    for (service, record) in found {
        book.set_record(
            service,
            device,
            record.as_ref().map(|r| (r.port, r.ips.as_slice())),
            now,
        );
    }
}

impl Resolver for ArcDirectory {
    fn lookup(&self, tx_device: &str, tx_channel: &str) -> Option<Advertised> {
        self.asked.store(true, Ordering::Relaxed);
        sync(&self.shared, &self.records, tx_device);
        let now = self.shared.now();
        let mut book = lock(&self.shared.book);
        let wants = book.wants();
        let found = book.lookup(tx_device, tx_channel, now);
        if book.wants() != wants {
            self.shared.wake.notify_all();
        }
        found
    }

    fn multicast(&self, tx_device: &str, _tx_channel: &str, a: &Advertised) -> MulticastLookup {
        self.asked.store(true, Ordering::Relaxed);
        sync(&self.shared, &self.records, tx_device);
        let now = self.shared.now();
        let mut book = lock(&self.shared.book);
        let wants = book.wants();
        let found = book.multicast(tx_device, a, now);
        if book.wants() != wants {
            self.shared.wake.notify_all();
        }
        found
    }
}

/// The reader: the book's reads one after another, until the stop.
fn reader(shared: &Shared, ip: Ipv4Addr, requests: &Requests) {
    let seq = AtomicU16::new(1);
    let mut report = Report::new(shared.now());
    while let Some(job) = wait_for_job(shared, &mut report) {
        let started = Instant::now();
        let (result, sent, peer) = match Udp::open(
            ip,
            job.arc,
            &seq,
            shared.timing.reply,
            requests,
            &shared.stop,
        ) {
            Ok(mut ex) if job.kind == Kind::Flows => {
                let result = read_tx_flows(&mut ex, job.form);
                flows_read(shared, &job, result, ex.sent, ex.peer, started);
                continue;
            }
            Ok(mut ex) => {
                let result = read_directory(&mut ex);
                (result, ex.sent, ex.peer)
            }
            Err(e) => {
                let err = |op| ReadError::Io { op, kind: e.kind() };
                if job.kind == Kind::Flows {
                    flows_read(shared, &job, Err(err(job.form.op())), 0, 0, started);
                    continue;
                }
                (Err(err(crate::arc::op::COUNTS)), 0, 0)
            }
        };
        let ms = started.elapsed().as_millis();
        match &result {
            Ok(dir) => info!(
                peer,
                channels = dir.channels().len(),
                named = dir.named(),
                requests = sent,
                ms,
                "a transmitter's channels read through its ARC"
            ),
            Err(e) => {
                debug!(peer, requests = sent, ms, error = %e, "an ARC read of a transmitter's channels failed")
            }
        }
        let now = shared.now();
        lock(&shared.book).finished(
            &job,
            Outcome {
                result,
                requests: sent,
            },
            now,
        );
    }
}

/// Waits for the next read (a want, a wait over), looking at the minute's
/// report at least every [`Timing::idle`]; `None` at the stop (which, as a
/// new want, wakes it at once).
fn wait_for_job(shared: &Shared, report: &mut Report) -> Option<Job> {
    let mut book = lock(&shared.book);
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            return None;
        }
        let now = shared.now();
        report.look(now, book.counts());
        if let Some(job) = book.next(now) {
            return Some(job);
        }
        let idle = shared.timing.idle;
        let wait = book
            .next_due()
            .map_or(idle, |due| Duration::from_nanos(due.saturating_sub(now)))
            .min(idle);
        book = shared
            .wake
            .wait_timeout(book, wait)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

/// The minute's report: what the counts did since the last one, logged at
/// most once a [`REPORT_NS`] and only when something happened.
#[derive(Debug)]
struct Report {
    at: u64,
    reported: Counts,
}

impl Report {
    fn new(now: u64) -> Self {
        Report {
            at: now,
            reported: Counts::default(),
        }
    }

    /// The counts to log at `now`, if a report is due and anything
    /// happened since the last.
    fn due(&mut self, now: u64, counts: Counts) -> Option<Counts> {
        if now.saturating_sub(self.at) < REPORT_NS {
            return None;
        }
        self.at = now;
        let d = counts.since(&self.reported);
        self.reported = counts;
        (d != Counts::default()).then_some(d)
    }

    fn look(&mut self, now: u64, counts: Counts) {
        if let Some(d) = self.due(now, counts) {
            info!(
                reads = d.reads,
                failed = d.failed(),
                no_reply = d.no_reply,
                refused = d.refused,
                malformed = d.malformed,
                socket = d.socket,
                requests = d.requests,
                not_listed = d.not_listed,
                unknown_device = d.unknown,
                "ARC directory reads in a minute"
            );
        }
    }
}

/// The network's channels: each one's record asked directly
/// ([`super::instance`]); a channel with no answer within the query's grace
/// is read through its device's ARC.
#[derive(Debug, Default)]
pub struct Network {
    pub instances: Instances,
    pub arc: ArcDirectory,
}

impl Resolver for Network {
    fn lookup(&self, tx_device: &str, tx_channel: &str) -> Option<Advertised> {
        // One read of the record and its grace (dstack #269).
        match self.instances.direct(tx_device, tx_channel) {
            Direct::Found(a) => Some(a),
            // Its direct query may still answer: no ARC yet.
            Direct::Waiting => None,
            Direct::Missing => self.arc.lookup(tx_device, tx_channel),
        }
    }

    /// Read from the transmitter's ARC, whichever record named the channel.
    fn multicast(&self, tx_device: &str, tx_channel: &str, a: &Advertised) -> MulticastLookup {
        self.arc.multicast(tx_device, tx_channel, a)
    }
}

#[cfg(test)]
mod tests;
