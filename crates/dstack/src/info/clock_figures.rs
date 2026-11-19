// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! A device's clock figures over the info port (dstack #429, for
//! dcontroller#124): a dstack-own request on [`super::REQUEST_PORT`]
//! (8700), answered by every dstack device that knows it, with a fixed,
//! versioned binary body: which source the media clock runs on, the
//! in-process follower's state (dsync-ptp) and what the stack's own poller
//! read of the dsync service. Any other device leaves it unanswered, as it
//! does every opcode it does not know.
//!
//! **Not the heartbeat, not the info group**, as the bridge figures
//! ([`super::bridge`]): the reply goes back UNICAST to the asker's address
//! and port; a device that shares its address's 8700 ([`super::share`])
//! gets the request with its source ([`super::share::Msg::Ask`]) and hands
//! its reply to the manager to send ([`super::share::Msg::SendTo`]).
//!
//! | | opcode |
//! |---|---|
//! | request | `07 xx d5 b3 00 00 00 00` (byte 1 anything), no body |
//! | reply | `07 2a d5 b2 00 00 00 00` ([`REPLY_OPCODE`]), the body below |
//!
//! **The body, version 1** ([`figures_body`], [`parse_figures`]):
//! [`FIGURES_LEN`] bytes, big-endian, every field at a fixed offset. A
//! reader takes version 1 at exactly [`FIGURES_LEN`] bytes and refuses
//! any other version or length, a code past its table and a flag byte
//! other than 0 or 1: a change of the layout is a new version, appended
//! (`.claude/rules/info-port.md`).
//!
//! | offset | type | key | unit |
//! |---|---|---|---|
//! | 0x00 | u16 | version | 1 |
//! | 0x02 | u8 | `source` | what the media clock runs on: 0 none yet, 1 the in-process follower, 2 the dsync service, 3 the test system clock (no PTP) ([`TimeSource`]) |
//! | 0x03 | u8 | `ready` | 1 when the media clock has a time |
//! | 0x04 | u32 | `sequence` | this reply's number, from 1 at the device's start |
//! | 0x08 | i64 | `follower_error_p90_ns` | the follower's phase error, the p90 of \|e\| over its last 30 windows, ns; `i64::MIN` = not known (fewer windows) |
//! | 0x10 | i64 | `follower_offset_from_service_ns` | the follower's time less the service's while the switch to it waits to agree, ns; `i64::MIN` = not known (no such wait) |
//! | 0x18 | i32 | `follower_freq_ppb` | the follower's frequency against the PC's counter, ppb (dsync's `rate_ppm` × 1000, rounded) |
//! | 0x1c | u32 | `follower_holdovers` | the follower's entries into holdover |
//! | 0x20 | i32 | `service_freq_ppb` | the service's frequency adjustment, ppb, from its last reply |
//! | 0x24 | u32 | `service_holdovers` | the poller's runs in holdover |
//! | 0x28 | u32 | `service_reply_age_ms` | ms since the service's last reply; `u32::MAX` = no reply yet |
//! | 0x2c | 6 bytes | `follower_grandmaster` | the PTPv1 UUID of the last Sync the follower heard; zeros = none |
//! | 0x32 | 6 bytes | `service_grandmaster` | the grandmaster the service's last reply named; zeros = none |
//! | 0x38 | u8 | `follower` | 1 when the device runs the follower (a linked clock whose follower started), 0 when not: then every `follower_` field is 0 |
//! | 0x39 | u8 | `follower_locked` | dsync's lock gate: \|e\| within 50 µs for 3 windows |
//! | 0x3a | u8 | `follower_settled` | locked, and its recent error and pull within 30 µs |
//! | 0x3b | u8 | `follower_holdover` | in holdover now |
//! | 0x3c | u8 | `follower_waiting` | why the clock does not use it: 0 in use, 1 stopped, 2 no PTP heard, 3 not locked, 4 not settled, 5 waits to agree ([`Waiting`]) |
//! | 0x3d | u8 | `service_poll` | the poller's last poll: 0 none (no poller, or not yet: then every `service_` field is 0), 1 usable, 2 no reply, 3 not locked, 4 no date offset, 5 a date step near, 6 a date slew ([`PollOutcome`]) |
//! | 0x3e | u8 | `service_locked` | the last reply's lock (DSYX byte 41) |
//! | 0x3f | u8 | `service_settled` | the last reply's servo settled (DSYX byte 62) |
//! | 0x40 | u8 | `service_holdover` | the poller in holdover now |
//! | 0x41 | u8 | `service_mode` | the last reply's mode (DSYX byte 40: 0 init, 1 acquiring, 2 production, 3 lock, 4 nano, 5 NTP only), as dsync sends it |
//!
//! The follower's figures are the selector's last round (once a second);
//! the service's are the poller's last poll (once a second) and its last
//! reply, whose age says how old they are. A device on the dsync service
//! only (no linked clock) shows no follower; the test system clock shows
//! neither. dsync's own phase error and its `:8898` status are no part of
//! it: the stack does not read them. [`ClockFigures`] holds a not-known
//! value as `None` (an `i64::MIN` or a `u32::MAX` written as known reads
//! back `None`).
//!
//! **The bound:** at most [`super::limit::PER_SPAN`] replies a second, one
//! bound for every asker, a kind of its own; the figures are read after
//! it. A reader that asks again after a timeout may get the earlier ask's
//! late reply: `sequence` tells it apart.

use crate::clock::linked::{Source, Waiting};
use crate::clock::{ClockStats, PollOutcome};

/// The request opcode's bytes 2 and 3 (`07 xx d5 b3 00 00 00 00`).
pub const REQUEST_CODE: [u8; 2] = [0xd5, 0xb3];
/// The reply opcode's bytes 2 and 3.
pub const REPLY_CODE: [u8; 2] = [0xd5, 0xb2];
/// The reply's whole opcode.
pub const REPLY_OPCODE: [u8; 8] = [0x07, 0x2a, 0xd5, 0xb2, 0x00, 0x00, 0x00, 0x00];
/// The body's version this stack writes and reads.
pub const VERSION: u16 = 1;
/// The version 1 body's length.
pub const FIGURES_LEN: usize = 0x42;
/// A signed figure not known on the wire.
pub const NOT_KNOWN_NS: i64 = i64::MIN;
/// The reply age not known on the wire (no reply yet).
pub const NO_REPLY_MS: u32 = u32::MAX;

/// Where the body's fields sit.
mod at {
    pub const VERSION: usize = 0x00;
    pub const SOURCE: usize = 0x02;
    pub const READY: usize = 0x03;
    pub const SEQUENCE: usize = 0x04;
    pub const FOLLOWER_ERROR_P90: usize = 0x08;
    pub const FOLLOWER_OFFSET: usize = 0x10;
    pub const FOLLOWER_FREQ: usize = 0x18;
    pub const FOLLOWER_HOLDOVERS: usize = 0x1c;
    pub const SERVICE_FREQ: usize = 0x20;
    pub const SERVICE_HOLDOVERS: usize = 0x24;
    pub const SERVICE_AGE: usize = 0x28;
    pub const FOLLOWER_GM: usize = 0x2c;
    pub const SERVICE_GM: usize = 0x32;
    pub const FOLLOWER: usize = 0x38;
    pub const FOLLOWER_LOCKED: usize = 0x39;
    pub const FOLLOWER_SETTLED: usize = 0x3a;
    pub const FOLLOWER_HOLDOVER: usize = 0x3b;
    pub const FOLLOWER_WAITING: usize = 0x3c;
    pub const SERVICE_POLL: usize = 0x3d;
    pub const SERVICE_LOCKED: usize = 0x3e;
    pub const SERVICE_SETTLED: usize = 0x3f;
    pub const SERVICE_HOLDOVER: usize = 0x40;
    pub const SERVICE_MODE: usize = 0x41;
}

const _: () = assert!(at::FOLLOWER_GM + 6 == at::SERVICE_GM);
const _: () = assert!(at::SERVICE_GM + 6 == at::FOLLOWER);
const _: () = assert!(at::SERVICE_MODE + 1 == FIGURES_LEN);

/// What a device's media clock runs on (the body's `source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeSource {
    /// Nothing ready yet (a linked clock before its first source; wire 0).
    #[default]
    None,
    /// dsync's in-process follower (wire 1).
    Internal,
    /// The dsync service on the PC (wire 2): a linked clock on it, or a
    /// device on the service only.
    Service,
    /// The test system clock: no PTP at all (wire 3).
    System,
}

impl TimeSource {
    /// The wire byte.
    pub fn code(self) -> u8 {
        match self {
            TimeSource::None => 0,
            TimeSource::Internal => 1,
            TimeSource::Service => 2,
            TimeSource::System => 3,
        }
    }

    /// The source of wire byte `code`; `None` for a byte no version writes.
    pub fn of_code(code: u8) -> Option<TimeSource> {
        match code {
            0 => Some(TimeSource::None),
            1 => Some(TimeSource::Internal),
            2 => Some(TimeSource::Service),
            3 => Some(TimeSource::System),
            _ => None,
        }
    }

    /// The source of a clock that is the test system clock (`system`), else
    /// linked to `linked` (`None`: the service only, not linked).
    pub fn of(system: bool, linked: Option<Source>) -> TimeSource {
        match (system, linked) {
            (true, _) => TimeSource::System,
            (false, None | Some(Source::Service)) => TimeSource::Service,
            (false, Some(Source::Internal)) => TimeSource::Internal,
            (false, Some(Source::None)) => TimeSource::None,
        }
    }
}

/// The follower's wait reason's wire byte (0: in use).
pub fn waiting_code(w: Option<Waiting>) -> u8 {
    match w {
        None => 0,
        Some(Waiting::Stopped) => 1,
        Some(Waiting::NoPtp) => 2,
        Some(Waiting::NotLocked) => 3,
        Some(Waiting::NotSettled) => 4,
        Some(Waiting::WaitsToAgree) => 5,
    }
}

/// The wait reason of wire byte `code` (`Some(None)`: in use); `None` for
/// a byte no version writes.
pub fn waiting_of_code(code: u8) -> Option<Option<Waiting>> {
    match code {
        0 => Some(None),
        1 => Some(Some(Waiting::Stopped)),
        2 => Some(Some(Waiting::NoPtp)),
        3 => Some(Some(Waiting::NotLocked)),
        4 => Some(Some(Waiting::NotSettled)),
        5 => Some(Some(Waiting::WaitsToAgree)),
        _ => None,
    }
}

/// The last poll's wire byte (0: none).
pub fn poll_code(p: Option<PollOutcome>) -> u8 {
    match p {
        None => 0,
        Some(PollOutcome::Usable) => 1,
        Some(PollOutcome::NoReply) => 2,
        Some(PollOutcome::NotLocked) => 3,
        Some(PollOutcome::NoDateState) => 4,
        Some(PollOutcome::StepNear) => 5,
        Some(PollOutcome::Slewing) => 6,
    }
}

/// The last poll of wire byte `code` (`Some(None)`: none); `None` for a
/// byte no version writes.
pub fn poll_of_code(code: u8) -> Option<Option<PollOutcome>> {
    match code {
        0 => Some(None),
        1 => Some(Some(PollOutcome::Usable)),
        2 => Some(Some(PollOutcome::NoReply)),
        3 => Some(Some(PollOutcome::NotLocked)),
        4 => Some(Some(PollOutcome::NoDateState)),
        5 => Some(Some(PollOutcome::StepNear)),
        6 => Some(Some(PollOutcome::Slewing)),
        _ => None,
    }
}

/// The in-process follower's figures (the module's table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FollowerFigures {
    pub locked: bool,
    pub settled: bool,
    /// In holdover now.
    pub holdover: bool,
    /// Why the clock does not use it; `None` while in use.
    pub waiting: Option<Waiting>,
    /// The grandmaster of the last Sync it heard; `None` before one.
    pub grandmaster: Option<[u8; 6]>,
    pub holdovers: u32,
    /// Its frequency against the PC's counter, ppb.
    pub freq_ppb: i32,
    /// The p90 of its |e| over its last 30 windows, ns; `None` before.
    pub error_p90_ns: Option<i64>,
    /// Its time less the service's while the switch to it waits to agree,
    /// ns; `None` otherwise.
    pub offset_from_service_ns: Option<i64>,
}

/// The dsync service as the device's own poller read it (the module's
/// table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceFigures {
    /// The last poll's outcome.
    pub poll: PollOutcome,
    /// ms since the last reply; `None` before one.
    pub reply_age_ms: Option<u32>,
    /// The last reply's lock, settled and mode (false, false and 0 before
    /// one).
    pub locked: bool,
    pub settled: bool,
    pub mode: u8,
    /// The poller in holdover now, and its runs in holdover.
    pub holdover: bool,
    pub holdovers: u32,
    /// The last reply's frequency adjustment, ppb.
    pub freq_ppb: i32,
    /// The grandmaster the last reply named; `None` before one.
    pub grandmaster: Option<[u8; 6]>,
}

/// A device's clock figures (the module's table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClockFigures {
    /// This reply's number, from 1 at the device's start.
    pub sequence: u32,
    pub source: TimeSource,
    /// The media clock has a time.
    pub ready: bool,
    /// `None` when the device runs no follower.
    pub follower: Option<FollowerFigures>,
    /// `None` when the device polls no service, or before its first poll.
    pub service: Option<ServiceFigures>,
}

/// Why a body does not read as clock figures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ClockFiguresError {
    #[error("clock figures: {0} bytes, version 1 has {FIGURES_LEN}")]
    Length(usize),
    #[error("clock figures: version {0}, this reader reads {VERSION}")]
    Version(u16),
    #[error("clock figures: byte {byte} at {at:#04x} is no {what}")]
    Code {
        at: usize,
        byte: u8,
        what: &'static str,
    },
}

/// A UUID's bytes: zeros for none.
fn uuid_bytes(u: Option<[u8; 6]>) -> [u8; 6] {
    u.unwrap_or([0; 6])
}

/// A UUID read: `None` for zeros.
fn uuid_of(b: [u8; 6]) -> Option<[u8; 6]> {
    (b != [0; 6]).then_some(b)
}

/// The version 1 body of `f` (a `None` written as its not-known value, an
/// absent part as zeros).
pub fn figures_body(f: &ClockFigures) -> [u8; FIGURES_LEN] {
    let mut c = [0u8; FIGURES_LEN];
    c[at::VERSION..at::VERSION + 2].copy_from_slice(&VERSION.to_be_bytes());
    c[at::SOURCE] = f.source.code();
    c[at::READY] = u8::from(f.ready);
    c[at::SEQUENCE..at::SEQUENCE + 4].copy_from_slice(&f.sequence.to_be_bytes());
    if let Some(w) = &f.follower {
        let wide: [(usize, Option<i64>); 2] = [
            (at::FOLLOWER_ERROR_P90, w.error_p90_ns),
            (at::FOLLOWER_OFFSET, w.offset_from_service_ns),
        ];
        for (o, v) in wide {
            c[o..o + 8].copy_from_slice(&v.unwrap_or(NOT_KNOWN_NS).to_be_bytes());
        }
        c[at::FOLLOWER_FREQ..at::FOLLOWER_FREQ + 4].copy_from_slice(&w.freq_ppb.to_be_bytes());
        c[at::FOLLOWER_HOLDOVERS..at::FOLLOWER_HOLDOVERS + 4]
            .copy_from_slice(&w.holdovers.to_be_bytes());
        c[at::FOLLOWER_GM..at::FOLLOWER_GM + 6].copy_from_slice(&uuid_bytes(w.grandmaster));
        c[at::FOLLOWER] = 1;
        c[at::FOLLOWER_LOCKED] = u8::from(w.locked);
        c[at::FOLLOWER_SETTLED] = u8::from(w.settled);
        c[at::FOLLOWER_HOLDOVER] = u8::from(w.holdover);
        c[at::FOLLOWER_WAITING] = waiting_code(w.waiting);
    }
    if let Some(s) = &f.service {
        c[at::SERVICE_FREQ..at::SERVICE_FREQ + 4].copy_from_slice(&s.freq_ppb.to_be_bytes());
        c[at::SERVICE_HOLDOVERS..at::SERVICE_HOLDOVERS + 4]
            .copy_from_slice(&s.holdovers.to_be_bytes());
        c[at::SERVICE_AGE..at::SERVICE_AGE + 4]
            .copy_from_slice(&s.reply_age_ms.unwrap_or(NO_REPLY_MS).to_be_bytes());
        c[at::SERVICE_GM..at::SERVICE_GM + 6].copy_from_slice(&uuid_bytes(s.grandmaster));
        c[at::SERVICE_POLL] = poll_code(Some(s.poll));
        c[at::SERVICE_LOCKED] = u8::from(s.locked);
        c[at::SERVICE_SETTLED] = u8::from(s.settled);
        c[at::SERVICE_HOLDOVER] = u8::from(s.holdover);
        c[at::SERVICE_MODE] = s.mode;
    }
    c
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn i32_at(b: &[u8], o: usize) -> i32 {
    i32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// A signed figure: `None` for [`NOT_KNOWN_NS`].
fn ns_at(b: &[u8], o: usize) -> Option<i64> {
    let mut w = [0u8; 8];
    w.copy_from_slice(&b[o..o + 8]);
    let v = i64::from_be_bytes(w);
    (v != NOT_KNOWN_NS).then_some(v)
}

fn uuid_at(b: &[u8], o: usize) -> Option<[u8; 6]> {
    let mut u = [0u8; 6];
    u.copy_from_slice(&b[o..o + 6]);
    uuid_of(u)
}

/// A code byte at `o` read by `of`, or the refusal naming `what`.
fn code_at<T>(
    b: &[u8],
    o: usize,
    what: &'static str,
    of: impl Fn(u8) -> Option<T>,
) -> Result<T, ClockFiguresError> {
    of(b[o]).ok_or(ClockFiguresError::Code {
        at: o,
        byte: b[o],
        what,
    })
}

/// A flag byte at `o`: 0 or 1.
fn flag_at(b: &[u8], o: usize) -> Result<bool, ClockFiguresError> {
    code_at(b, o, "flag", |v| match v {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    })
}

/// The figures in a reply body: version 1 at exactly [`FIGURES_LEN`]
/// bytes, each code a known one and each flag 0 or 1. An absent part's
/// fields (`follower` 0, `service_poll` 0) are not read.
pub fn parse_figures(body: &[u8]) -> Result<ClockFigures, ClockFiguresError> {
    let Some(v) = body.get(at::VERSION..at::VERSION + 2) else {
        return Err(ClockFiguresError::Length(body.len()));
    };
    let version = u16::from_be_bytes([v[0], v[1]]);
    if version != VERSION {
        return Err(ClockFiguresError::Version(version));
    }
    if body.len() != FIGURES_LEN {
        return Err(ClockFiguresError::Length(body.len()));
    }
    let follower = if flag_at(body, at::FOLLOWER)? {
        Some(FollowerFigures {
            locked: flag_at(body, at::FOLLOWER_LOCKED)?,
            settled: flag_at(body, at::FOLLOWER_SETTLED)?,
            holdover: flag_at(body, at::FOLLOWER_HOLDOVER)?,
            waiting: code_at(body, at::FOLLOWER_WAITING, "wait reason", waiting_of_code)?,
            grandmaster: uuid_at(body, at::FOLLOWER_GM),
            holdovers: u32_at(body, at::FOLLOWER_HOLDOVERS),
            freq_ppb: i32_at(body, at::FOLLOWER_FREQ),
            error_p90_ns: ns_at(body, at::FOLLOWER_ERROR_P90),
            offset_from_service_ns: ns_at(body, at::FOLLOWER_OFFSET),
        })
    } else {
        None
    };
    let service = match code_at(body, at::SERVICE_POLL, "poll outcome", poll_of_code)? {
        None => None,
        Some(poll) => {
            let age = u32_at(body, at::SERVICE_AGE);
            Some(ServiceFigures {
                poll,
                reply_age_ms: (age != NO_REPLY_MS).then_some(age),
                locked: flag_at(body, at::SERVICE_LOCKED)?,
                settled: flag_at(body, at::SERVICE_SETTLED)?,
                mode: body[at::SERVICE_MODE],
                holdover: flag_at(body, at::SERVICE_HOLDOVER)?,
                holdovers: u32_at(body, at::SERVICE_HOLDOVERS),
                freq_ppb: i32_at(body, at::SERVICE_FREQ),
                grandmaster: uuid_at(body, at::SERVICE_GM),
            })
        }
    };
    Ok(ClockFigures {
        sequence: u32_at(body, at::SEQUENCE),
        source: code_at(body, at::SOURCE, "source", TimeSource::of_code)?,
        ready: flag_at(body, at::READY)?,
        follower,
        service,
    })
}

/// Whether `opcode` is dstack's clock-figures request (byte 1 anything).
pub fn is_request(opcode: &[u8; 8]) -> bool {
    matches!(opcode, [0x07, _, 0xd5, 0xb3, 0, 0, 0, 0])
}

/// The ms from the monotonic time `at_ns` to `now_ns`: 0 for a time ahead,
/// at most `u32::MAX - 1` (about 49 days; `u32::MAX` is "no reply").
pub fn age_ms(at_ns: i64, now_ns: i64) -> u32 {
    let ms = now_ns.saturating_sub(at_ns).max(0) / 1_000_000;
    u32::try_from(ms).map_or(NO_REPLY_MS - 1, |m| m.min(NO_REPLY_MS - 1))
}

/// The figures of a clock with the stats `stats` (`sequence` 0: the reply
/// sets it), `ready` when it has a time, at the monotonic time `now_ns`
/// (for the reply's age).
pub fn figures_of(stats: &ClockStats, ready: bool, now_ns: i64) -> ClockFigures {
    let follower = stats.follower().map(|c| FollowerFigures {
        locked: c.locked,
        settled: c.settled,
        holdover: c.counts.in_holdover,
        waiting: c.waiting,
        grandmaster: c.grandmaster,
        holdovers: c.counts.holdovers,
        freq_ppb: c.freq_ppb,
        error_p90_ns: c.recent_abs_e_ns,
        offset_from_service_ns: c.offset_from_service_ns,
    });
    let read = stats.service();
    let polled = stats.counts();
    let service = read.outcome.map(|poll| {
        let reply = read.reply.map(|(r, _)| r);
        ServiceFigures {
            poll,
            reply_age_ms: read.reply.map(|(_, at)| age_ms(at, now_ns)),
            locked: reply.is_some_and(|r| r.locked),
            settled: reply.is_some_and(|r| r.settled),
            mode: reply.map_or(0, |r| r.mode),
            holdover: polled.in_holdover,
            holdovers: polled.holdovers,
            freq_ppb: reply.map_or(0, |r| r.freq_ppb),
            grandmaster: stats.grandmaster(),
        }
    });
    ClockFigures {
        sequence: 0,
        source: TimeSource::of(stats.is_system(), stats.linked()),
        ready,
        follower,
        service,
    }
}
