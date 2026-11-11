// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The device's local control socket (Gen 3 phase B, plan Task 7):
//! [`SOCKET`] in the unit's runtime directory (`RuntimeDirectory=iem-dante`,
//! mode 0750; the socket itself 0660, [`bind_control`]): the service user
//! and its group (phase D: the box's admin user is in the group `iemmixer`,
//! for `ctl status`, the live guard and health). One request a connection,
//! one line each way ([`session`]);
//! `iem-dante-dev ctl <request>` is the client.
//!
//! - `status`: one JSON line, numbers, keys and a few fixed words
//!   (`counter`, `sine`, `echo`, `engine`), and the device's own name
//!   (`name`, dstack #209), never an address ([`status`](fn@status));
//!   with the engine's pump once one is attached, and the app's own part
//!   once it sets one (`app`, the app's words).
//! - `source <tx> silence|counter|sine <hz> <dbfs>|echo <rx> <samples>|engine`:
//!   what TX channel `tx` (from 1) sends ([`crate::media::source::Source`]);
//!   runtime only, every TX channel is silent at start. A sine is at most
//!   −20 dBFS (the HIL rule) and below half the sample rate; an echo reads
//!   RX channel `rx` (from 1) `samples` earlier, fewer than a ring holds;
//!   `engine` (phase C) sends the engine's ring of that channel, only on a
//!   channel the engine feeds (`error: no engine on that channel`).
//! - `check <rx> off|counter|sine <hz>`: which checker reads RX channel
//!   `rx`'s ring at its playout point ([`Checks`]); runtime only.
//! - `app <words>` (dstack #139): the app's own command, handed to its hook
//!   ([`Media::set_app_command`]) and answered by it ([`app_answer`]):
//!   `ok`, the app's one-line text, or `error: <reason>`; `error: no app
//!   command` without a hook. The one request whose answer may carry the
//!   app's words. The local endpoint takes it too ([`local`]).
//! - `tx-end <flow>` (dstack #187, dstack-dev only: a controller's E2E,
//!   dcontroller#46): ends our live TX flow `flow` (the status's `tx` flow
//!   number, from 1) as if its receiver had gone quiet
//!   ([`crate::media::tx::TxTable::end`]): it sends nothing more, its slot
//!   is free, and its receiver (ours, subscribed to our own TX, in the E2E)
//!   stops receiving, then asks for the flow again and receives by itself,
//!   on a new flow (often under another number: its stop frees the slot and
//!   a new flow takes the next one).
//!   `error: tx-end: no live TX flow there` for an empty slot or a flow
//!   expired already; every device but dstack-dev's answers
//!   `error: tx-end is dstack-dev's only` ([`Media::allow_dev_requests`]).
//!
//! Anything else is one line `error: <reason>`, the reason a fixed text
//! (the request is never echoed back). Settings answer `ok`.
//!
//! [`Requests`] counts every datagram the device's servers receive by port,
//! peer and opcode, and our own flow requests and stops and our ARC reads of
//! other devices (`crate::subscribe::directory`) by peer and opcode,
//! cumulative for the device's life: the live guard (plan Task 8) reads their rates
//! from the status every second. A peer is named by an id only (1 for the
//! first seen, the same id for the device's life; [`MAX_PEERS`], then 0),
//! the status's own, shared by what we receive and what we send.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::{Mutex, PoisonError};

use tracing::{debug, info};

use crate::daemon::media::Media;
use crate::media::check::{CounterCheck, CounterStats, SineCheck, SineStats};
use crate::media::ring::{Ring, SLOTS};
use crate::media::source::{SINE_CAP_DBFS, Source};
use crate::media::tx::MAX_FLOWS;
use crate::wire::read_u16;

mod clock;
pub mod probe;
mod status;
pub use clock::clock_source_word;
pub use status::{APP_MAX, app_object, status};

/// Where the device listens.
pub const SOCKET: &str = "/run/dstack/control.sock";
/// The longest request line read, bytes.
pub const MAX_LINE: usize = 256;
/// The status's peers with an id of their own; any further peer is 0.
pub const MAX_PEERS: usize = 64;
/// The (port, peer, opcode) and (peer, opcode) entries counted; datagrams
/// past them are only summed (`uncounted`).
pub const MAX_ENTRIES: usize = 256;
/// Blind samples before a counter check says `no_return`: 10 s at the
/// device's rate (songplayer's).
pub const NO_RETURN_S: u64 = 10;
/// The samples a check reads at a time.
pub const CHECK_CHUNK: usize = 1024;
/// A check this far behind its playout point (or ahead of it, the clock
/// gone back) starts again at it: half the ring, which a slow step must
/// never let the RX thread overwrite under it.
pub const MAX_BEHIND: u64 = (SLOTS / 2) as u64;

/// The device's numbers a request is checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub tx: usize,
    pub rx: usize,
    pub rate: u32,
}

/// A checker an RX channel can have.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CheckKind {
    /// [`CounterCheck`]: the counter source read back.
    Counter,
    /// [`SineCheck`] of a sine at `hz`.
    Sine { hz: f64 },
}

/// A request.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Request {
    Status,
    /// TX channel `tx` (from 0) sends `source`.
    Source {
        tx: usize,
        source: Source,
    },
    /// RX channel `rx` (from 0) is checked by `check` (`None`: by none).
    Check {
        rx: usize,
        check: Option<CheckKind>,
    },
    /// The app's own command: the words after `app` ([`app_words`]).
    App,
    /// dstack-dev's own: our TX flow in slot `slot` (from 0) ends
    /// ([`Media::allow_dev_requests`]).
    TxEnd {
        slot: usize,
    },
}

const UNKNOWN: &str = "unknown request: status, source, check, app or tx-end";
const APP_USAGE: &str = "app <words>";
/// The answer without an app command hook.
pub const NO_APP_COMMAND: &str = "no app command";
/// The longest app answer, bytes (one line; the rest is cut).
pub const APP_ANSWER_MAX: usize = 256;
const SOURCE_USAGE: &str =
    "source <tx> silence|counter|sine <hz> <dbfs>|echo <rx> <samples>|engine";
const NO_ENGINE: &str = "no engine on that channel";
const CHECK_USAGE: &str = "check <rx> off|counter|sine <hz>";
const TX_END_USAGE: &str = "tx-end <flow>";
/// The answer to `tx-end` on a device that is not dstack-dev's.
pub const TX_END_DEV_ONLY: &str = "tx-end is dstack-dev's only";
/// The answer to `tx-end` for a slot without a live TX flow.
pub const NO_LIVE_FLOW: &str = "tx-end: no live TX flow there";
const HZ: &str = "the sine's frequency must be above 0 and below half the sample rate";

/// Reads a request line within `lim`; the error is a fixed text.
pub fn parse(line: &str, lim: Limits) -> Result<Request, &'static str> {
    let words: Vec<&str> = line.split_ascii_whitespace().collect();
    match words.as_slice() {
        ["status"] => Ok(Request::Status),
        ["status", ..] => Err("status takes nothing more"),
        ["source", tx, rest @ ..] => {
            let tx = channel(tx, lim.tx).ok_or("source: no such TX channel")?;
            let source = match rest {
                ["silence"] => Source::Silence,
                ["counter"] => Source::Counter,
                ["sine", hz, dbfs] => Source::Sine {
                    hz: frequency(hz, lim.rate).ok_or(HZ)?,
                    dbfs: level(dbfs)
                        .ok_or("the sine's level must be a number of dBFS of at most -20")?,
                },
                ["echo", rx, offset] => Source::Echo {
                    rx: channel(rx, lim.rx).ok_or("source: no such RX channel to echo")?,
                    offset: offset
                        .parse::<u32>()
                        .ok()
                        .filter(|&o| u64::from(o) < SLOTS as u64)
                        .ok_or("the echo's offset must be a whole number of samples below 65536")?,
                },
                ["engine"] => Source::Engine,
                _ => return Err(SOURCE_USAGE),
            };
            Ok(Request::Source { tx, source })
        }
        ["check", rx, rest @ ..] => {
            let rx = channel(rx, lim.rx).ok_or("check: no such RX channel")?;
            let check = match rest {
                ["off"] => None,
                ["counter"] => Some(CheckKind::Counter),
                ["sine", hz] => Some(CheckKind::Sine {
                    hz: frequency(hz, lim.rate).ok_or(HZ)?,
                }),
                _ => return Err(CHECK_USAGE),
            };
            Ok(Request::Check { rx, check })
        }
        ["tx-end", flow] => Ok(Request::TxEnd {
            slot: channel(flow, MAX_FLOWS).ok_or("tx-end: no such TX flow (1 to 32)")?,
        }),
        ["tx-end", ..] => Err(TX_END_USAGE),
        ["app"] => Err(APP_USAGE),
        ["app", ..] => Ok(Request::App),
        ["source"] => Err(SOURCE_USAGE),
        ["check"] => Err(CHECK_USAGE),
        _ => Err(UNKNOWN),
    }
}

/// The words of an `app` request line: what follows the word `app`, its
/// spaces at both ends trimmed (inner spaces kept: an app's words may be a
/// name).
pub fn app_words(line: &str) -> &str {
    let line = line.trim_start();
    line.strip_prefix("app").unwrap_or(line).trim()
}

/// The answer line to an app command: `error: no app command` without a
/// hook (`None`), `ok` for an empty `Ok`, the text of another, `error:
/// <reason>` for an `Err`; control characters (a line break) as spaces,
/// cut to [`APP_ANSWER_MAX`] bytes at a character's edge.
pub fn app_answer(r: Option<Result<String, String>>) -> String {
    let text = match r {
        None => format!("error: {NO_APP_COMMAND}"),
        Some(Ok(t)) if t.is_empty() => "ok".to_string(),
        Some(Ok(t)) => t,
        Some(Err(e)) => format!("error: {e}"),
    };
    let mut out = String::new();
    for c in text.chars() {
        if out.len() + c.len_utf8() > APP_ANSWER_MAX {
            break;
        }
        out.push(if c.is_control() { ' ' } else { c });
    }
    out
}

/// A channel number from 1 to `count`, as an index from 0.
fn channel(text: &str, count: usize) -> Option<usize> {
    let n: usize = text.parse().ok()?;
    (1..=count).contains(&n).then(|| n - 1)
}

/// A frequency above 0 and below half `rate` (NaN and infinities fail).
fn frequency(text: &str, rate: u32) -> Option<f64> {
    let hz: f64 = text.parse().ok()?;
    (hz > 0.0 && hz < f64::from(rate) / 2.0).then_some(hz)
}

/// A finite level of at most [`SINE_CAP_DBFS`].
fn level(text: &str) -> Option<f64> {
    let dbfs: f64 = text.parse().ok()?;
    (dbfs.is_finite() && dbfs <= SINE_CAP_DBFS).then_some(dbfs)
}

/// The answer line to `line` (without its newline): `ok`, the status, or
/// `error: <reason>`.
pub fn answer(line: &str, media: &Media) -> String {
    match parse(line, media.limits()) {
        Ok(Request::Status) => status(media),
        Ok(Request::Source { tx, source }) => {
            if !media.tx.set_source(tx, source) {
                // The channel is the device's (parsed): only the engine's
                // source is refused on a channel without its ring.
                let reason = if source == Source::Engine {
                    NO_ENGINE
                } else {
                    "source: no such TX channel"
                };
                return format!("error: {reason}");
            }
            info!(tx = tx + 1, source = source_word(source), "TX source set");
            "ok".to_string()
        }
        Ok(Request::Check { rx, check }) => {
            if !lock(&media.checks).set(rx, check) {
                return "error: check: no such RX channel".to_string();
            }
            info!(rx = rx + 1, check = check_word(check), "RX check set");
            "ok".to_string()
        }
        Ok(Request::App) => {
            let words = app_words(line);
            let answer = app_answer(media.app_command(words));
            info!(words, answer, "app command");
            answer
        }
        Ok(Request::TxEnd { slot }) => {
            if !media.dev_requests() {
                debug!("tx-end refused: not dstack-dev");
                return format!("error: {TX_END_DEV_ONLY}");
            }
            if !media.tx.end_flow(slot) {
                return format!("error: {NO_LIVE_FLOW}");
            }
            info!(
                flow = slot + 1,
                "TX flow ended by the control socket (tx-end): its receiver is to ask again"
            );
            "ok".to_string()
        }
        Err(reason) => {
            debug!(reason, "control request refused");
            format!("error: {reason}")
        }
    }
}

fn source_word(s: Source) -> &'static str {
    match s {
        Source::Silence => "silence",
        Source::Counter => "counter",
        Source::Sine { .. } => "sine",
        Source::Echo { .. } => "echo",
        Source::Engine => "engine",
    }
}

fn check_word(c: Option<CheckKind>) -> &'static str {
    match c {
        None => "off",
        Some(CheckKind::Counter) => "counter",
        Some(CheckKind::Sine { .. }) => "sine",
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The opcode of a datagram with the 10-byte header (ARC, CMC, flow
/// control): its op1; 0 when it is shorter.
pub fn header_op(datagram: &[u8]) -> u16 {
    read_u16(datagram, 6).unwrap_or(0)
}

/// The opcode of an info request: bytes 26–27 (`0x0061` board info, `0x00c1`
/// product info, `0x0013` network info, …; byte 25 varies by sender); 0
/// when it is shorter.
pub fn info_op(datagram: &[u8]) -> u16 {
    read_u16(datagram, 26).unwrap_or(0)
}

/// One count of what the device received: datagrams to `port` from `peer`
/// with opcode `op`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Received {
    pub port: u16,
    pub peer: usize,
    pub op: u16,
    pub n: u64,
}

/// One count of what our RX sent to `peer`: flow-control requests (`op`
/// 0x0100) and stops (0x0101), and the ARC reads of a transmitter's channels
/// (0x1000 once a directory read, 0x2000 and 0x2010 once a page).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sent {
    pub peer: usize,
    pub op: u16,
    pub n: u64,
}

/// [`Requests`] read at one moment, in (port, peer, opcode) and (peer,
/// opcode) order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestCounts {
    pub received: Vec<Received>,
    pub sent: Vec<Sent>,
    /// Datagrams past the [`MAX_ENTRIES`] of either table.
    pub uncounted: u64,
}

/// The device's request counts (cumulative; see the module).
#[derive(Debug, Default)]
pub struct Requests(Mutex<Table>);

#[derive(Debug, Default)]
struct Table {
    /// Peer `k + 1` is `peers[k]`.
    peers: Vec<IpAddr>,
    received: BTreeMap<(u16, usize, u16), u64>,
    sent: BTreeMap<(usize, u16), u64>,
    uncounted: u64,
}

impl Table {
    fn peer_id(&mut self, peer: IpAddr) -> usize {
        if let Some(k) = self.peers.iter().position(|&p| p == peer) {
            return k + 1;
        }
        if self.peers.len() < MAX_PEERS {
            self.peers.push(peer);
            return self.peers.len();
        }
        0
    }
}

/// One more of `key` in `map`, or past the table.
fn bump<K: Ord>(map: &mut BTreeMap<K, u64>, key: K, uncounted: &mut u64) {
    if let Some(n) = map.get_mut(&key) {
        *n += 1;
    } else if map.len() < MAX_ENTRIES {
        map.insert(key, 1);
    } else {
        *uncounted += 1;
    }
}

impl Requests {
    /// One datagram to `port` from `peer` with opcode `op`; returns the
    /// peer's id.
    pub fn received(&self, port: u16, peer: IpAddr, op: u16) -> usize {
        let mut t = lock(&self.0);
        let id = t.peer_id(peer);
        let Table {
            received,
            uncounted,
            ..
        } = &mut *t;
        bump(received, (port, id, op), uncounted);
        id
    }

    /// One datagram of ours to `peer` with opcode `op` (a flow-control
    /// request or stop, an ARC read); returns the peer's id.
    pub fn sent(&self, peer: IpAddr, op: u16) -> usize {
        let mut t = lock(&self.0);
        let id = t.peer_id(peer);
        let Table {
            sent, uncounted, ..
        } = &mut *t;
        bump(sent, (id, op), uncounted);
        id
    }

    pub fn counts(&self) -> RequestCounts {
        let t = lock(&self.0);
        RequestCounts {
            received: t
                .received
                .iter()
                .map(|(&(port, peer, op), &n)| Received { port, peer, op, n })
                .collect(),
            sent: t
                .sent
                .iter()
                .map(|(&(peer, op), &n)| Sent { peer, op, n })
                .collect(),
            uncounted: t.uncounted,
        }
    }
}

/// Where an RX channel's samples are played: its flow's latency in samples
/// (a packet later than that is late) and its bits per sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Playout {
    pub latency: u32,
    pub bits: u8,
}

/// A check's counts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CheckStats {
    /// Its channel has had no playing flow yet: nothing read.
    Waiting,
    Counter(CounterStats),
    Sine(SineStats),
}

/// One RX channel's check as the status shows it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CheckView {
    /// The RX channel, from 0.
    pub rx: usize,
    pub kind: CheckKind,
    /// Samples passed over: the check fell [`MAX_BEHIND`] behind.
    pub skipped: u64,
    pub stats: CheckStats,
}

#[derive(Debug)]
enum Checker {
    Counter(CounterCheck),
    Sine(SineCheck),
}

#[derive(Debug)]
struct Check {
    kind: CheckKind,
    /// Built at the channel's first playing flow (a sine's floor is its
    /// flow's bits).
    checker: Option<Checker>,
    /// The next position to read; `None` until the channel plays.
    cursor: Option<u64>,
    skipped: u64,
}

/// The RX channels' checks: each reads its channel's ring at the playout
/// point (media time less its flow's latency), so it judges what a player
/// of the ring would play, the way the echo and phase C's engine read it.
#[derive(Debug)]
pub struct Checks {
    rate: u32,
    checks: Vec<Option<Check>>,
    buf: Vec<i32>,
}

impl Checks {
    /// No check on any of `rx` channels at `rate`.
    pub fn new(rx: usize, rate: u32) -> Self {
        Checks {
            rate,
            checks: (0..rx).map(|_| None).collect(),
            buf: vec![0; CHECK_CHUNK],
        }
    }

    /// RX channel `rx` (from 0) is checked by `kind` from now (a fresh
    /// check), or by none; `false` for a channel we do not have.
    pub fn set(&mut self, rx: usize, kind: Option<CheckKind>) -> bool {
        let Some(slot) = self.checks.get_mut(rx) else {
            return false;
        };
        *slot = kind.map(|kind| Check {
            kind,
            checker: None,
            cursor: None,
            skipped: 0,
        });
        true
    }

    /// One round at media time `now` (samples): each checked channel with
    /// a playing flow (`plays[rx]`) reads its ring (`rings[rx]`) from where
    /// it left off up to the playout point; one without a flow reads
    /// nothing and starts at the playout point when it plays again.
    pub fn step(&mut self, now: u64, rings: &[Ring], plays: &[Option<Playout>]) {
        let Checks { rate, checks, buf } = self;
        for (rx, slot) in checks.iter_mut().enumerate() {
            let Some(check) = slot.as_mut() else { continue };
            let (Some(Some(play)), Some(ring)) = (plays.get(rx), rings.get(rx)) else {
                check.cursor = None;
                continue;
            };
            let end = now.saturating_sub(u64::from(play.latency));
            let kind = check.kind;
            let checker = check.checker.get_or_insert_with(|| match kind {
                CheckKind::Counter => {
                    Checker::Counter(CounterCheck::new(NO_RETURN_S * u64::from(*rate)))
                }
                CheckKind::Sine { hz } => Checker::Sine(SineCheck::new(hz, *rate, play.bits)),
            });
            let mut pos = match check.cursor {
                // Within the bound: behind, go on; ahead (the clock went
                // back a little), wait for the playout point to pass it.
                Some(c) if c.abs_diff(end) <= MAX_BEHIND => c,
                // Further: start again at the playout point; the samples a
                // cursor behind it would have read are passed over.
                Some(c) => {
                    check.skipped += end.saturating_sub(c);
                    end
                }
                // The first read.
                None => end,
            };
            while pos < end {
                let n = (end - pos).min(buf.len() as u64) as usize;
                let out = &mut buf[..n];
                ring.read_at(pos, out);
                match checker {
                    Checker::Counter(c) => c.check(pos, out),
                    Checker::Sine(c) => c.check(pos, out),
                }
                pos += n as u64;
            }
            check.cursor = Some(pos);
        }
    }

    /// Every check, in RX channel order.
    pub fn views(&self) -> Vec<CheckView> {
        self.checks
            .iter()
            .enumerate()
            .filter_map(|(rx, c)| {
                let c = c.as_ref()?;
                Some(CheckView {
                    rx,
                    kind: c.kind,
                    skipped: c.skipped,
                    stats: match &c.checker {
                        None => CheckStats::Waiting,
                        Some(Checker::Counter(k)) => CheckStats::Counter(k.stats()),
                        Some(Checker::Sine(k)) => CheckStats::Sine(k.stats()),
                    },
                })
            })
            .collect()
    }
}

/// The status endpoint for local readers (dtray, dstack #104): TCP on
/// 127.0.0.1 at a port the OS picks, announced in the device's ARC record
/// (`dstack_ctl`). It answers `status`, and `app <words>` (dstack #139: the
/// app's own command, such as dlisten's output, which dtray's menu sends;
/// [`answer`]). The test requests (`source`, `check`) stay on the
/// Unix socket, so nothing on the PC changes the device's audio through
/// it. Every platform has it.
pub mod local {
    use std::io;
    use std::net::Ipv4Addr;
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::net::{TcpListener, TcpStream};

    use super::{answer, status};
    use crate::daemon::media::Media;

    /// How long a connection may take to send its request, and its answer
    /// to be taken.
    pub const WAIT: Duration = Duration::from_secs(1);
    /// The answer to anything but `status` and `app <words>`.
    pub const ONLY_STATUS_AND_APP: &str = "this endpoint answers status and app only";

    /// The answer line to `line` (without its newline).
    pub fn reply(line: &str, media: &Media) -> String {
        if line.trim() == "status" {
            status(media)
        } else if line.split_ascii_whitespace().next() == Some("app") {
            answer(line, media)
        } else {
            format!("error: {ONLY_STATUS_AND_APP}")
        }
    }

    /// A listener on 127.0.0.1 at a port the OS picks, made from a std one
    /// (inside the device's runtime).
    pub fn bind() -> io::Result<TcpListener> {
        let std = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        std.set_nonblocking(true)?;
        TcpListener::from_std(std)
    }

    /// Answers every connection, each on its own task within [`WAIT`].
    /// Ends only with an error accepting.
    pub async fn serve(listener: TcpListener, media: Arc<Media>) -> io::Result<()> {
        loop {
            let (stream, _) = listener.accept().await?;
            let media = Arc::clone(&media);
            tokio::spawn(async move { session(stream, &media).await });
        }
    }

    /// One connection: its request line within [`WAIT`], the answer line
    /// ([`reply`]), then closed.
    async fn session(stream: TcpStream, media: &Media) {
        super::line::exchange(&stream, WAIT, |line| reply(line, media)).await;
    }
}

/// One request line in, one answer line out, on a stream read and written
/// by readiness: the Unix socket's sessions and the local endpoint's.
mod line {
    use std::io::{self, ErrorKind};
    use std::time::Duration;

    use tracing::debug;

    use super::MAX_LINE;

    /// A stream read and written by readiness (tokio's TCP and Unix ones).
    pub(super) trait Ready {
        async fn readable(&self) -> io::Result<()>;
        fn try_read(&self, buf: &mut [u8]) -> io::Result<usize>;
        async fn writable(&self) -> io::Result<()>;
        fn try_write(&self, buf: &[u8]) -> io::Result<usize>;
    }

    macro_rules! ready_for {
        ($t:ty) => {
            impl Ready for $t {
                async fn readable(&self) -> io::Result<()> {
                    <$t>::readable(self).await
                }
                fn try_read(&self, buf: &mut [u8]) -> io::Result<usize> {
                    <$t>::try_read(self, buf)
                }
                async fn writable(&self) -> io::Result<()> {
                    <$t>::writable(self).await
                }
                fn try_write(&self, buf: &[u8]) -> io::Result<usize> {
                    <$t>::try_write(self, buf)
                }
            }
        };
    }
    ready_for!(tokio::net::TcpStream);
    #[cfg(unix)]
    ready_for!(tokio::net::UnixStream);

    pub(super) const TOO_LONG: &str = "the request is longer than 256 bytes";

    /// The connection's request line within `wait`, `answer`'s line back
    /// within `wait`; failures are answered or logged, never raised.
    pub(super) async fn exchange(
        stream: &impl Ready,
        wait: Duration,
        answer: impl FnOnce(&str) -> String,
    ) {
        let reply = match tokio::time::timeout(wait, read_request(stream)).await {
            Ok(Ok(line)) => answer(&line),
            Ok(Err(reason)) => format!("error: {reason}"),
            Err(_) => "error: no request line within the wait".to_string(),
        };
        let mut out = reply.into_bytes();
        out.push(b'\n');
        match tokio::time::timeout(wait, write_all(stream, &out)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => debug!(error = %e, "a control answer could not be sent"),
            Err(_) => debug!("a control answer was not taken within the wait"),
        }
    }

    pub(super) async fn read_request(stream: &impl Ready) -> Result<String, &'static str> {
        let mut line = Vec::with_capacity(MAX_LINE);
        let mut buf = [0u8; 512];
        loop {
            stream
                .readable()
                .await
                .map_err(|_| "the request could not be read")?;
            match stream.try_read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    line.extend_from_slice(&buf[..n]);
                    if let Some(end) = line.iter().position(|&b| b == b'\n') {
                        line.truncate(end);
                        break;
                    }
                    if line.len() > MAX_LINE {
                        return Err(TOO_LONG);
                    }
                }
                Err(e) => would_block(e).map_err(|_| "the request could not be read")?,
            }
        }
        if line.len() > MAX_LINE {
            return Err(TOO_LONG);
        }
        // A trailing CR is white space to the parse.
        String::from_utf8(line).map_err(|_| "the request is not UTF-8")
    }

    pub(super) async fn write_all(stream: &impl Ready, mut data: &[u8]) -> io::Result<()> {
        while !data.is_empty() {
            stream.writable().await?;
            match stream.try_write(data) {
                Ok(n) => data = &data[n..],
                Err(e) => would_block(e)?,
            }
        }
        Ok(())
    }

    /// A readiness that was not (would block): wait again; any other error
    /// ends the session.
    pub(super) fn would_block(e: io::Error) -> io::Result<()> {
        if e.kind() == ErrorKind::WouldBlock {
            Ok(())
        } else {
            Err(e)
        }
    }
}

#[cfg(unix)]
pub use socket::{WAIT, bind_control, serve_control, session};

#[cfg(unix)]
mod socket {
    use std::io::{self, ErrorKind};
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::net::{UnixListener, UnixStream};

    use super::answer;
    use crate::daemon::media::Media;

    /// How long a connection may take to send its request, and its answer
    /// to be taken.
    pub const WAIT: Duration = Duration::from_secs(1);

    /// Listens at `path`: a socket an earlier run left there is replaced;
    /// anything else there is refused (never removed). The socket is the
    /// service user's and its group's (0660, whatever the umask): who may
    /// reach it at all is the runtime folder's (0750) group.
    pub fn bind_control(path: &Path) -> io::Result<UnixListener> {
        if let Ok(m) = std::fs::symlink_metadata(path) {
            if !m.file_type().is_socket() {
                return Err(io::Error::new(
                    ErrorKind::AlreadyExists,
                    "something that is not a socket is at the control socket's path",
                ));
            }
            std::fs::remove_file(path)?;
        }
        // Nothing there, or a path that cannot be read: the bind says why.
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
        Ok(listener)
    }

    /// The control server: one connection at a time, each a
    /// [`session`] bounded by [`WAIT`]. Ends only with an error accepting.
    /// Glue: every answer is [`answer`]'s.
    pub async fn serve_control(listener: UnixListener, media: Arc<Media>) -> io::Result<()> {
        loop {
            let (stream, _) = listener.accept().await?;
            session(stream, &media, WAIT).await;
        }
    }

    /// One connection: its request line (at most [`super::MAX_LINE`] bytes, ended
    /// by a newline or the end of what it sends) within `wait`, and the
    /// answer line, given `wait` to be taken; then it is closed.
    pub async fn session(stream: UnixStream, media: &Media, wait: Duration) {
        super::line::exchange(&stream, wait, |line| answer(line, media)).await;
    }
}

#[cfg(test)]
mod tests;
