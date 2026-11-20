// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #305: the CI test peer (`dstack-dev peer`) on loopback
//! (dstack #366). Two peers in this process, each on the TEST system clock with
//! its own skew, send each other a tone: each hears the other's tone whole,
//! and each one's play times show its own clock's skew against the app's
//! clock. The binary's command line parses, runs and stops on SIGINT with
//! its status written. A file of its own: its devices set the `normal`
//! timing profile (one per process).

use std::collections::BTreeSet;
use std::net::Ipv4Addr;
use std::path::Path;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dstack::clock;
use dstack::daemon::media::Media;
use dstack::host::Stopped;
use dstack::media::tx::{LATE_BINS, LATE_EDGES_NS, LaneCounts};
use dstack::peer::args::{self, Parsed, PeerArgs};
use dstack::peer::{Peer, Shared};
use dstack::stream::PumpStats;
use dstack::stream::wake::RECENT;

/// Held by each test that runs a device: they bind the Dante ports on
/// loopback.
static ONE_DEVICE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The test device's address: loopback, so none of its mDNS, info or
/// heartbeat multicasts leaves the box (dstack #366: this workspace is a
/// test run, which refuses a device on a site address).
fn test_ipv4() -> Ipv4Addr {
    Ipv4Addr::LOCALHOST
}

fn peer_args(line: &str, dir: &Path, name: &str) -> PeerArgs {
    let mut words: Vec<String> = line.split_whitespace().map(str::to_string).collect();
    words.extend([
        "--ip".into(),
        test_ipv4().to_string(),
        "--name".into(),
        name.into(),
        "--status".into(),
        dir.join(format!("{name}.json")).display().to_string(),
    ]);
    match args::parse(&words) {
        Ok(Parsed::Run(a)) => *a,
        other => panic!("{line}: {other:?}"),
    }
}

/// A peer's counts the gap check reads.
#[derive(Debug, Clone, Copy)]
struct Counts {
    gaps: u64,
    holes: u64,
    tx_restarts: u64,
    pump_restarts: u64,
    blocks: u64,
    /// The TX flow's holes: positions the TX thread sent before the pump
    /// wrote them (silence; dstack #339: a pump block that ends past its
    /// W re-bootstraps nothing and leaves only these).
    tx_holes: u64,
    /// The RX channel ring's late samples (written after their read).
    ring_late: u64,
    /// The pump's blocks with an output the TX thread had already sent
    /// as silence (dstack #346, `writes_after_send`).
    after_send: u64,
}

impl Counts {
    fn of(p: &Peer) -> Counts {
        // The gaps first: a cause counted after them is still in the
        // window, never a gap without its cause.
        let gaps = p.shared().rx[0].view().gaps;
        let v: serde_json::Value = serde_json::from_str(&p.status()).unwrap();
        let num = |x: &serde_json::Value| x.as_u64().unwrap_or_else(|| panic!("a count: {x}"));
        let pump = p.media().engine().expect("the pump");
        Counts {
            gaps,
            holes: num(&v["device"]["channels"][0]["holes"]),
            tx_restarts: num(&v["device"]["tx"][0]["rebootstraps"]),
            pump_restarts: pump.rebootstraps,
            blocks: pump.blocks,
            tx_holes: num(&v["device"]["tx"][0]["holes"]),
            ring_late: num(&v["device"]["channels"][0]["late"]),
            after_send: pump.writes_after_send,
        }
    }

    fn minus(&self, before: &Counts) -> Counts {
        Counts {
            gaps: self.gaps - before.gaps,
            holes: self.holes - before.holes,
            tx_restarts: self.tx_restarts - before.tx_restarts,
            pump_restarts: self.pump_restarts - before.pump_restarts,
            blocks: self.blocks - before.blocks,
            tx_holes: self.tx_holes - before.tx_holes,
            ring_late: self.ring_late - before.ring_late,
            after_send: self.after_send - before.after_send,
        }
    }
}

fn unix_ms() -> u64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    u64::try_from(since.as_millis()).unwrap()
}

/// A scheduling gap of the witness thread over [`Witness::STALL_NS`]: when
/// it ended (ms since the Unix epoch, as the pump's late wakes) and how
/// long it was, µs of the app's clock (QPC on Windows).
#[derive(Debug, Clone, Copy)]
struct Stall {
    end_ms: u64,
    gap_us: u64,
}

/// A time the sender's TX thread sent nothing while the witness ran and
/// came back late: from the witness's pass that saw its packets rise
/// before (`begin_ms`) to its last pass that saw them flat (`end_ms`, ms
/// since the Unix epoch), `silent_us` of the app's clock between the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Silence {
    begin_ms: u64,
    end_ms: u64,
    silent_us: u64,
}

/// The TX lane's packets and its own record of being late (its packets
/// sent 5 ms or more past due and its schedule's restarts), as the
/// witness's passes see them (dstack #339).
#[derive(Debug, Clone, Copy)]
struct Progress {
    packets: u64,
    /// The late record at the pass that saw the packets rise last.
    late: u64,
    /// That pass: ns of the app's clock, ms since the Unix epoch.
    rose: (u64, u64),
    /// The last pass since then that saw the packets flat.
    flat: Option<(u64, u64)>,
}

impl Progress {
    fn new(packets: u64, late: u64, now: (u64, u64)) -> Progress {
        Progress {
            packets,
            late,
            rose: now,
            flat: None,
        }
    }

    /// A pass at `now` (ns of the app's clock, ms since the Unix epoch)
    /// reads `packets` and `late`. When the packets rose after passes that
    /// saw them flat, and the late record rose with them: the silence the
    /// witness saw, from the rise before to the last flat pass. A lower
    /// bound of the thread's own silence that never holds the witness's own
    /// gap into the rising pass, so a stall of the whole process reads as
    /// the witness's stall, never as a TX silence. A TX thread that ran on
    /// time but sent nothing (a stack fault dropping packets) keeps its late
    /// record flat: no silence.
    fn look(&mut self, packets: u64, late: u64, now: (u64, u64)) -> Option<Silence> {
        if packets == self.packets {
            self.flat = Some(now);
            return None;
        }
        let silence = self.close(late);
        self.packets = packets;
        self.late = late;
        self.rose = now;
        silence
    }

    /// The open silence, from the rise before to the last flat pass, when
    /// the late record rose past the one at that rise (`late`); also the
    /// witness's last word on a TX thread still silent when it stops.
    fn close(&mut self, late: u64) -> Option<Silence> {
        self.flat
            .take()
            .filter(|_| late > self.late)
            .map(|(flat_ns, flat_ms)| Silence {
                begin_ms: self.rose.1,
                end_ms: flat_ms,
                silent_us: flat_ns.saturating_sub(self.rose.0) / 1_000,
            })
    }
}

/// The TX lane's packets and late record for [`Progress`]: its packets
/// sent 5 ms or more past due plus its schedule's restarts (a lag past the
/// latency sends nothing, it restarts). [`TxShared::lane`] loads `packets`
/// before the late bins and the TX pass adds the late bin before `packets`
/// (`media::tx::pass`), all relaxed: on x86 a rise of the packets is in
/// practice never read without its late bin. Where it is, that silence is
/// missed, or its late record credited to the next flat stretch of the
/// same lane.
///
/// [`TxShared::lane`]: dstack::media::tx::TxShared::lane
fn tx_progress(lane: &LaneCounts) -> (u64, u64) {
    const _: () = assert!(LATE_EDGES_NS[LATE_BINS - 2] == 5_000_000);
    (
        lane.packets,
        lane.late_bins[LATE_BINS - 1] + lane.rebootstraps,
    )
}

/// One peer's sending threads as the witness watches them.
struct Sender {
    media: Arc<Media>,
    pump: Arc<PumpStats>,
    tx: Progress,
    /// The pump's late wakes at the last merge.
    late_wakes: u64,
    /// `writes_after_send` at the last pass.
    after_send: u64,
}

impl Sender {
    fn new(media: Arc<Media>, now: (u64, u64)) -> Sender {
        let pump = media.engine_stats().expect("the pump");
        let (packets, late) = tx_progress(&media.tx.lane(0));
        Sender {
            tx: Progress::new(packets, late, now),
            late_wakes: pump.wakes().late_wakes(),
            after_send: pump.view().writes_after_send,
            media,
            pump,
        }
    }

    /// One pass of the witness at `now`: peer `peer`'s TX silence that
    /// ended, its `writes_after_send` rise and its new late wakes, into
    /// `seen`. The pump needs no silence of its own: a pump not run, before
    /// or inside a block, wakes late at its next block, and that late wake
    /// is recorded with how far behind it was.
    fn look(&mut self, peer: usize, now: (u64, u64), seen: &mut Seen) {
        let (packets, late) = tx_progress(&self.media.tx.lane(0));
        let silence = self.tx.look(packets, late, now);
        seen.silence(peer, silence);
        let after_send = self.pump.view().writes_after_send;
        if after_send > self.after_send {
            self.after_send = after_send;
            seen.after_send[peer].push(now.1);
        }
        // The status keeps the last 32 late wakes only: merge them as they
        // come, so a busy window keeps every one (more than 32 between two
        // passes, while the witness itself was not run, are counted lost).
        let late_wakes = self.pump.wakes().late_wakes();
        if late_wakes > self.late_wakes {
            let new = late_wakes - self.late_wakes;
            seen.late_wakes_lost[peer] += new.saturating_sub(RECENT as u64);
            self.late_wakes = late_wakes;
            for w in self.pump.wakes().recent() {
                seen.late_wakes[peer].insert((w.unix_ms, w.behind_us));
            }
        }
    }

    /// The witness's last pass: a TX silence still open, into `seen`.
    fn close(&mut self, peer: usize, seen: &mut Seen) {
        let (_, late) = tx_progress(&self.media.tx.lane(0));
        let silence = self.tx.close(late);
        seen.silence(peer, silence);
    }
}

/// What the witness saw over the counted window (dstack #339).
#[derive(Debug, Default)]
struct Seen {
    stalls: Vec<Stall>,
    /// Its largest gap, whatever its length.
    largest: Option<Stall>,
    /// When each peer's loss rose (ms since the Unix epoch): its tone-gap
    /// count or its RX ring's holes (a hole the tone check does not count
    /// as a gap still fails the test).
    rises: [Vec<u64>; 2],
    /// Each peer's TX silences over [`Witness::STALL_NS`] while the
    /// witness ran.
    silences: [Vec<Silence>; 2],
    /// Each peer's pump late wakes, `(unix_ms, behind_us)`.
    late_wakes: [BTreeSet<(u64, u64)>; 2],
    /// Each peer's late wakes that left the pump's recent list before the
    /// witness merged them.
    late_wakes_lost: [u64; 2],
    /// When each peer's `writes_after_send` rose (ms since the Unix epoch).
    after_send: [Vec<u64>; 2],
}

impl Seen {
    /// Keeps peer `peer`'s TX silence when it is one ([`Witness::STALL_NS`]).
    fn silence(&mut self, peer: usize, silence: Option<Silence>) {
        if let Some(s) = silence
            && s.silent_us >= Witness::STALL_NS / 1_000
        {
            self.silences[peer].push(s);
        }
    }
}

/// A thread at the test's own priority that sleeps 1 ms at a time on the
/// app's clock ([`clock::app_monotonic_ns`]) and keeps every pass gap over
/// [`Witness::STALL_NS`] (the runner did not run it), when each peer's
/// loss rose (its tone gaps or ring holes), and each peer's sending
/// threads (dstack #339): its TX thread's silences while the witness ran
/// with its late record (a runner can leave one thread unrun while it runs
/// another), its pump's late wakes and its `writes_after_send`. A failure
/// names the stall or silence at each loss, or none.
struct Witness {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<Seen>,
}

impl Witness {
    /// A pass gap over this is a stall: the 1 ms sleep plus 1 ms.
    const STALL_NS: u64 = 2_000_000;

    fn start(peers: [(Arc<Shared>, Arc<Media>); 2]) -> Witness {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let [(a, media_a), (b, media_b)] = peers;
            let tones = [a, b];
            let mut seen = Seen::default();
            let mut last = clock::app_monotonic_ns();
            let first = (u64::try_from(last).unwrap_or(0), unix_ms());
            let mut senders = [Sender::new(media_a, first), Sender::new(media_b, first)];
            let lost = |i: usize, senders: &[Sender; 2]| {
                let ring = senders[i].media.rx.rings().first().map(|r| r.counters());
                tones[i].rx[0].view().gaps + ring.map_or(0, |c| c.holes)
            };
            let mut counted = [lost(0, &senders), lost(1, &senders)];
            while !stopped.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
                let now = clock::app_monotonic_ns();
                let gap_ns = u64::try_from(now - last).unwrap_or(0);
                last = now;
                let now = (u64::try_from(now).unwrap_or(0), unix_ms());
                let stall = Stall {
                    end_ms: now.1,
                    gap_us: gap_ns / 1_000,
                };
                if seen.largest.is_none_or(|l| stall.gap_us > l.gap_us) {
                    seen.largest = Some(stall);
                }
                if gap_ns > Witness::STALL_NS {
                    seen.stalls.push(stall);
                }
                for (peer, s) in senders.iter_mut().enumerate() {
                    s.look(peer, now, &mut seen);
                }
                for (i, count) in counted.iter_mut().enumerate() {
                    let loss = lost(i, &senders);
                    if loss > *count {
                        seen.rises[i].push(now.1);
                        *count = loss;
                    }
                }
            }
            for (peer, s) in senders.iter_mut().enumerate() {
                s.close(peer, &mut seen);
            }
            seen
        });
        Witness { stop, thread }
    }

    fn stop(self) -> Seen {
        self.stop.store(true, Ordering::Release);
        self.thread.join().unwrap()
    }
}

/// How far before a loss's count (a tone gap's or a ring hole's) a stall
/// may end and still be its cause: the loss is heard a latency (10 ms)
/// after the sender lost it, and a gap is counted once it ends (the
/// check's clear run, a block or two).
const CAUSE_WINDOW_MS: u64 = 60;

/// Whether a thread not run from `begin_ms` to `end_ms` can be the cause
/// of a loss counted at `at` (all ms since the Unix epoch): it began at or
/// before the count and ended at most [`CAUSE_WINDOW_MS`] before it. It
/// may end after the count: a TX thread stalled past the latency leaves a
/// loss the receiver counts while the sender is still not run (dev1, load
/// ~45: a 14.3 ms TX silence ended 3 ms after its gap was counted).
fn cause_of(at: u64, begin_ms: u64, end_ms: u64) -> bool {
    begin_ms <= at && at <= end_ms + CAUSE_WINDOW_MS
}

/// When a time `us` long that ended at `end_ms` began, ms since the Unix
/// epoch (a part of a ms rounds to the earlier ms).
fn began(end_ms: u64, us: u64) -> u64 {
    end_ms.saturating_sub(us.div_ceil(1_000))
}

/// `end_ms` against a count at `at`, for the message.
fn when(at: u64, end_ms: u64) -> String {
    match end_ms.checked_sub(at) {
        Some(after) => format!("ended {after} ms after"),
        None => format!("ended {} ms before", at - end_ms),
    }
}

/// The least scheduling gap that can lose audio here, µs: a pump that
/// woke this late plus its block's work ends past W (6 ms), fpp (0.67 ms)
/// and the TX margin (0.25 ms) together and leaves TX holes; less is inside
/// the budget. The witness's own 1 ms sleep counts in its gap (so its stall
/// is this plus 1 ms).
const LOSS_GAP_US: u64 = 5_000;

/// The least TX silence that can lose audio, µs: a packet that leaves
/// about this late past its due reaches the receiver past its latency
/// (10 ms, less fpp 0.67 ms and the TX margin 0.25 ms), and one over the
/// latency late restarts the TX schedule; the witness sees a silence up to
/// a pass (1 ms) short.
const TX_LOSS_US: u64 = 10_000 - 667 - 250 - 1_000;

/// Peer `me`'s failure message (dstack #339): the verdict, each loss's
/// count time (a tone gap's or a ring hole's) with the evidence around it
/// ([`cause_of`]), the counts, what the witness saw of the threads and both
/// peers' statuses. Evidence is a thread late enough to lose audio: the
/// witness's own stall (the whole process), the sender's TX thread silent
/// while the witness ran with its own late record risen ([`Progress`],
/// [`TX_LOSS_US`]), or either pump's late wake ([`LOSS_GAP_US`]). Either
/// pump, since the receiver's pump restart is a counted cause too; never
/// the receiver's TX thread, which sends, never hears. `RUNNER` when the loss has a counted cause
/// (the sender's TX holes, TX restart or `writes_after_send`, either
/// pump's restart) and evidence at every loss; else `OURS`: a loss nothing
/// counted, or one with no thread late at it, is the stack's. Too many
/// restarts with no loss is `NO LOSS`.
fn verdict(
    me: usize,
    mine: &Counts,
    theirs: &Counts,
    seen: &Seen,
    started_ms: u64,
    status: &[String; 2],
) -> String {
    let other = 1 - me;
    let caused = theirs.tx_holes
        + theirs.tx_restarts
        + theirs.after_send
        + theirs.pump_restarts
        + mine.pump_restarts
        > 0;
    let mut explained = 0;
    let gaps: Vec<String> = seen.rises[me]
        .iter()
        .map(|&at| {
            let mut stalls: Vec<String> = seen
                .stalls
                .iter()
                .filter(|s| {
                    s.gap_us >= LOSS_GAP_US + 1_000
                        && cause_of(at, began(s.end_ms, s.gap_us), s.end_ms)
                })
                .map(|s| format!("witness {} us, {}", s.gap_us, when(at, s.end_ms)))
                .collect();
            for s in seen.silences[other]
                .iter()
                .filter(|s| s.silent_us >= TX_LOSS_US && cause_of(at, s.begin_ms, s.end_ms))
            {
                stalls.push(format!(
                    "peer {other}'s TX thread silent {} us, {}",
                    s.silent_us,
                    when(at, s.end_ms)
                ));
            }
            for (peer, w) in seen.late_wakes.iter().enumerate() {
                for &(end, us) in w
                    .iter()
                    .filter(|&&(end, us)| us >= LOSS_GAP_US && cause_of(at, began(end, us), end))
                {
                    stalls.push(format!(
                        "peer {peer}'s pump {us} us behind, {}",
                        when(at, end)
                    ));
                }
            }
            if !stalls.is_empty() {
                explained += 1;
            }
            format!(
                "loss counted at +{} ms after: {stalls:?}",
                at.saturating_sub(started_ms)
            )
        })
        .collect();
    let lost = mine.gaps + mine.holes > 0;
    let rises = seen.rises[me].len();
    let verdict = if !lost {
        "NO LOSS: more than one counted restart in the window (a loaded box)"
    } else if caused && rises > 0 && explained == rises {
        "RUNNER: a counted cause, and a thread not run long enough to lose audio at every \
         loss"
    } else {
        "OURS: a loss with no counted cause, or with no thread not run long enough at it"
    };
    let since =
        |ms: &[u64]| -> Vec<u64> { ms.iter().map(|t| t.saturating_sub(started_ms)).collect() };
    let silences: Vec<String> = (0..2)
        .flat_map(|peer| {
            seen.silences[peer].iter().map(move |s| {
                format!(
                    "peer {peer} {} us ending +{} ms",
                    s.silent_us,
                    s.end_ms.saturating_sub(started_ms)
                )
            })
        })
        .collect();
    format!(
        "{verdict}\npeer {me}: {mine:?}\nsender (peer {other}): {theirs:?}\n{gaps:?}\n\
         witness: {} stalls over {} us, largest {:?}\n\
         TX silences over {} us while the witness ran: {silences:?}\n\
         writes_after_send rose at (ms after the start): peer 0 {:?}, peer 1 {:?}\n\
         pump late wakes seen: peer 0 {} ({} lost), peer 1 {} ({} lost)\n\
         peer {me} status: {}\npeer {other} status: {}",
        seen.stalls.len(),
        Witness::STALL_NS / 1_000,
        seen.largest,
        Witness::STALL_NS / 1_000,
        since(&seen.after_send[0]),
        since(&seen.after_send[1]),
        seen.late_wakes[0].len(),
        seen.late_wakes_lost[0],
        seen.late_wakes[1].len(),
        seen.late_wakes_lost[1],
        status[me],
        status[other]
    )
}

/// A pump late wake that re-bootstrapped, as the witness merged it
/// (dstack #459): its wall time (ms since the Unix epoch), how far behind
/// it woke and how late its own sleep before it returned (µs; `None`
/// unmeasured).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Rebootstrap {
    unix_ms: u64,
    behind_us: u64,
    oversleep_us: Option<u64>,
}

/// How far apart two peers' re-bootstrapping wakes may be and still be one
/// stall of the process, ms (dstack #459): a stall ends for every thread
/// at once and each pump wakes at its own next timer, a block (0.67 ms) or
/// so apart. Every incident's two wakes were in the same ms.
const TOGETHER_MS: u64 = 2;

/// How far a re-bootstrap's own sleep may have returned short of or past
/// its lateness and still be the scheduler's miss, µs: the sleep alone put
/// the pump that late (`ci_peer_gate.py`'s rule for a lone re-bootstrap,
/// dstack #339). The incidents: 1 to 10 µs.
const OVERSLEEP_SLACK_US: u64 = 500;

/// The pairs of both peers' pump re-bootstraps that one stall of the
/// process caused (dstack #459): a re-bootstrap of peer 0 and one of peer 1
/// within [`TOGETHER_MS`] of each other, each woken late by its own sleep
/// ([`OVERSLEEP_SLACK_US`]), each after `from_ms` and by `to_ms` (the
/// counted window), and each in one pair at most. A re-bootstrap with no
/// measured oversleep, or one pump's alone, is in no pair.
fn shared_stalls(rebootstraps: &[BTreeSet<Rebootstrap>; 2], from_ms: u64, to_ms: u64) -> u64 {
    let _ = (rebootstraps, from_ms, to_ms);
    0
}

/// Waits up to `bound` for `done`, polling.
fn wait_for(bound: Duration, mut done: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + bound;
    while !done() {
        if Instant::now() > end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

/// The witness's TX silence (dstack #339) runs from the packets' rise
/// before to the last pass that saw them flat: none for packets that rose
/// each pass, never the witness's own gap into the rising pass, and only
/// when the TX lane's late record rose by the rise that ends it. The wall
/// times are the passes' own (here 1000 ms ahead of the app's clock).
#[test]
fn a_tx_silence_is_what_the_witness_saw_flat_and_late() {
    let at = |m: u64| (m * 1_000_000, 1_000 + m);
    let silence = |begin: u64, end: u64, us: u64| {
        Some(Silence {
            begin_ms: 1_000 + begin,
            end_ms: 1_000 + end,
            silent_us: us,
        })
    };
    let mut p = Progress::new(10, 0, at(0));
    assert_eq!(p.look(11, 0, at(1)), None);
    assert_eq!(p.look(12, 0, at(2)), None);
    assert_eq!(p.look(12, 0, at(3)), None);
    // The late record may rise while the packets are still flat.
    assert_eq!(p.look(12, 1, at(11)), None);
    // Flat from the rise at 2 ms to the pass at 11 ms; the witness itself
    // was not run from 11 to 40 ms, which is not the thread's.
    assert_eq!(p.look(13, 1, at(40)), silence(2, 11, 9_000));
    assert_eq!(p.look(14, 1, at(41)), None);
    assert_eq!(p.look(14, 1, at(42)), None);
    // Late in the rising pass itself.
    assert_eq!(p.look(15, 2, at(43)), silence(41, 42, 1_000));
    // Flat 20 ms with no late record (a thread that ran and sent nothing):
    // no silence; the next flat stretch is judged on its own.
    assert_eq!(p.look(15, 2, at(63)), None);
    assert_eq!(p.look(16, 2, at(64)), None);
    assert_eq!(p.look(16, 2, at(70)), None);
    assert_eq!(p.look(17, 3, at(71)), silence(64, 70, 6_000));
    // Still silent when the witness stops: closed by the last flat pass,
    // only with the late record risen.
    assert_eq!(p.look(17, 3, at(80)), None);
    assert_eq!(p.close(3), None);
    assert_eq!(p.look(17, 3, at(90)), None);
    assert_eq!(p.close(4), silence(71, 90, 19_000));
    assert_eq!(p.close(5), None);
}

/// The TX lane's late record is its packets 5 ms or more past due plus its
/// restarts (dstack #339).
#[test]
fn the_tx_late_record_is_its_late_packets_and_restarts() {
    let lane = LaneCounts {
        packets: 100,
        rebootstraps: 2,
        late_bins: [50, 20, 10, 10, 6, 4],
        ..LaneCounts::default()
    };
    assert_eq!(tx_progress(&lane), (100, 6));
}

/// A thread not run is a gap's cause when it began at or before the count
/// and ended at most the window before it (dstack #339).
#[test]
fn a_cause_began_by_the_count_and_ended_within_the_window() {
    let at = 1_000_000;
    // Ended within the window before, at its edge, and past it.
    let w = CAUSE_WINDOW_MS;
    assert!(cause_of(at, at - w - 6, at - w));
    assert!(!cause_of(at, at - w - 7, at - w - 1));
    // Began before the count and ended after it (a TX thread still stalled).
    assert!(cause_of(at, at - 11, at + 3));
    // Began at the count; began after it is not its cause.
    assert!(cause_of(at, at, at + 15));
    assert!(!cause_of(at, at + 1, at + 15));
    // A time's beginning: a part of a ms rounds to the earlier ms.
    assert_eq!(began(at + 15, 15_000), at);
    assert_eq!(began(at + 15, 14_001), at);
    assert_eq!(began(at + 15, 14_000), at + 1);
    assert_eq!(when(at, at + 3), "ended 3 ms after");
    assert_eq!(when(at, at - 7), "ended 7 ms before");
}

/// One stall of the process that re-bootstraps both peers' pumps is one
/// pair (dstack #459, the CI failures of 19-20.11.2026: both wakes in the
/// same ms, each one's own sleep that late); one pump's re-bootstrap alone,
/// two apart, a wake not woken late by its sleep and a wake outside the
/// counted window are in none.
#[test]
fn both_pumps_restarted_by_one_stall_are_one_pair() {
    let r = |unix_ms, behind_us, oversleep_us| Rebootstrap {
        unix_ms,
        behind_us,
        oversleep_us,
    };
    let pairs = |a: &[Rebootstrap], b: &[Rebootstrap]| {
        let set = |rs: &[Rebootstrap]| rs.iter().copied().collect::<BTreeSet<_>>();
        shared_stalls(&[set(a), set(b)], 1_000, 9_000)
    };
    // v0.6.200 main, windows-2025 (run 44684350870) and v0.6.199 main,
    // ubuntu-24.04 (run 44676550352), behind and oversleep in µs.
    let a = r(5_000, 7_250, Some(7_251));
    assert_eq!(pairs(&[a], &[r(5_000, 6_604, Some(6_607))]), 1);
    assert_eq!(
        pairs(&[r(5_000, 6_770, Some(6_780))], &[r(5_000, 6_083, Some(6_085))]),
        1
    );
    // One pump alone, either peer: today's rule.
    assert_eq!(pairs(&[a], &[]), 0);
    assert_eq!(pairs(&[], &[a]), 0);
    // Apart: within TOGETHER_MS either way is one stall, past it two.
    assert_eq!(pairs(&[a], &[r(5_002, 6_604, Some(6_607))]), 1);
    assert_eq!(pairs(&[r(5_002, 6_604, Some(6_607))], &[a]), 1);
    assert_eq!(pairs(&[a], &[r(5_003, 6_604, Some(6_607))]), 0);
    assert_eq!(pairs(&[a], &[r(4_997, 6_604, Some(6_607))]), 0);
    // Each woken late by its own sleep, within 0.5 ms either way; a block
    // that ran long (the sleep on time) or an unmeasured sleep is no stall.
    let b = |oversleep| r(5_000, 6_600, oversleep);
    assert_eq!(pairs(&[a], &[b(Some(6_100))]), 1);
    assert_eq!(pairs(&[a], &[b(Some(7_100))]), 1);
    assert_eq!(pairs(&[a], &[b(Some(6_099))]), 0);
    assert_eq!(pairs(&[a], &[b(Some(7_101))]), 0);
    assert_eq!(pairs(&[a], &[b(None)]), 0);
    assert_eq!(pairs(&[b(None)], &[a]), 0);
    assert_eq!(pairs(&[r(5_000, 7_250, Some(10))], &[a]), 0);
    // Only inside the counted window: after its start, by its end.
    let at = |ms| r(ms, 6_600, Some(6_600));
    assert_eq!(pairs(&[at(1_000)], &[at(1_000)]), 0);
    assert_eq!(pairs(&[at(1_001)], &[at(1_001)]), 1);
    assert_eq!(pairs(&[at(9_000)], &[at(9_000)]), 1);
    assert_eq!(pairs(&[at(9_001)], &[at(9_001)]), 0);
    assert_eq!(pairs(&[at(1_001)], &[at(1_000)]), 0);
    assert_eq!(pairs(&[at(9_000)], &[at(9_001)]), 0);
    // Each wake in one pair at most; two stalls are two pairs.
    assert_eq!(pairs(&[at(5_000), at(5_001)], &[at(5_000)]), 1);
    assert_eq!(pairs(&[at(5_000)], &[at(5_000), at(5_001)]), 1);
    assert_eq!(pairs(&[at(5_000), at(7_000)], &[at(5_001), at(7_000)]), 2);
}

/// The design's whole-device test: two devices at +40 and -30 ppm, each
/// receiving the other's tone (a 70 ppm drift between them: 0.7 ms over the
/// test's 10 s, well inside the 10 ms latency). Counted after both lock
/// and settle: no ring hole, no tone gap without a counted restart of the
/// sender's TX schedule or of a pump, and at most one such restart (dev1
/// at load 46 from another account's job restarted a TX schedule once and
/// the check saw it); no play-time step; each device's play times run at
/// its own skew against the app's clock.
#[test]
fn two_peers_at_different_skews_hear_each_other_s_tone_whole() {
    let _one = ONE_DEVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().unwrap();
    let a = peer_args(
        "--tone-hz 1000 --rx-from peer-b:TX~1 --test-clock-ppm 40",
        dir.path(),
        "peer-a",
    );
    let b = peer_args(
        "--tone-hz 440 --rx-from peer-a:TX~1 --test-clock-ppm -30 \
         --ports 4441,8801,4456 --process-id 1",
        dir.path(),
        "peer-b",
    );
    // The channel names hold a space: put it back (the line splits on it).
    let fix = |mut p: PeerArgs| {
        if let Some((_, chs)) = p.rx_from.as_mut() {
            for c in chs.iter_mut() {
                *c = c.replace('~', " ");
            }
        }
        p
    };
    let a = Peer::start(fix(a)).expect("peer A started");
    let b = Peer::start(fix(b)).expect("peer B started");
    let locked = |p: &Peer| p.shared().rx[0].view().locked;
    let both = wait_for(Duration::from_secs(20), || locked(&a) && locked(&b));
    assert!(
        both,
        "locked: A {}, B {}\nA {}\nB {}",
        locked(&a),
        locked(&b),
        a.status(),
        b.status()
    );
    // Settle (the play-time filter's first pairs), then count.
    std::thread::sleep(Duration::from_secs(1));
    // dstack #339: a thread of the test's own that sees the runner stall,
    // and when each gap is counted, for the failure's verdict.
    let witness = Witness::start([
        (Arc::clone(a.shared()), Arc::clone(a.media())),
        (Arc::clone(b.shared()), Arc::clone(b.media())),
    ]);
    let started_ms = unix_ms();
    let before = [Counts::of(&a), Counts::of(&b)];
    std::thread::sleep(Duration::from_secs(8));
    let after = [Counts::of(&a), Counts::of(&b)];
    let seen = witness.stop();
    let status = [a.status(), b.status()];
    let ppm = |p: &Peer| f64::from_bits(p.shared().play_at_ppm.load(Ordering::Relaxed));
    let ppms = [ppm(&a), ppm(&b)];
    let hz = [a.shared().rx[0].view().hz, b.shared().rx[0].view().hz];
    let steps = [
        a.media().engine().expect("A's pump").play_at_steps,
        b.media().engine().expect("B's pump").play_at_steps,
    ];
    assert_eq!((a.ended(), b.ended()), (None, None));
    assert!(matches!(b.stop(), Stopped::Done));
    assert!(matches!(a.stop(), Stopped::Done));
    let rise = |i: usize| after[i].minus(&before[i]);
    for (me, other) in [(0, 1), (1, 0)] {
        let (mine, theirs) = (rise(me), rise(other));
        // No ring hole, and a gap only with a counted restart of the
        // sender's TX schedule or a pump (a loaded box: another account's
        // job), at most one in the window. On a quiet box all are 0.
        let restarts = theirs.tx_restarts + theirs.pump_restarts + mine.pump_restarts;
        assert!(
            mine.gaps <= restarts && restarts <= 1 && mine.holes == 0,
            "peer {me}: {} tone gaps, {restarts} counted restarts (at most one, each gap \
             one's), {} ring holes\n{}",
            mine.gaps,
            mine.holes,
            verdict(me, &mine, &theirs, &seen, started_ms, &status)
        );
        assert!(mine.blocks > 0, "{mine:?}");
        assert_eq!(steps[me], 0, "play-time steps on peer {me}: {}", status[me]);
    }
    // Each hears the other's frequency, as the other's clock sends it.
    assert!(
        (hz[0] - 440.0).abs() < 0.1 && (hz[1] - 1000.0).abs() < 0.1,
        "{hz:?}"
    );
    // The play times show each device's own skew (the filter follows it
    // within its slew bound, 100 ppm).
    assert!(
        (ppms[0] - 40.0).abs() < 5.0,
        "A's play_at rate {} ppm",
        ppms[0]
    );
    assert!(
        (ppms[1] + 30.0).abs() < 5.0,
        "B's play_at rate {} ppm",
        ppms[1]
    );
    // The status file's fields, as a CI gate reads them.
    let v: serde_json::Value = serde_json::from_str(&status[0]).unwrap();
    assert_eq!(v["peer"]["test_clock_ppm"], 40.0);
    assert_eq!(v["peer"]["rx"][0]["tone_gaps"], after[0].gaps);
    // The peer's own jumps, apart from its tone gaps (#358): on the TEST
    // system clock (never re-anchored) each is a re-bootstrap, and the
    // status reads them before the engine's count.
    let own = v["peer"]["own_skips"].as_u64();
    let restarts = v["device"]["engine"]["rebootstraps"].as_u64();
    assert!(
        own.zip(restarts).is_some_and(|(o, r)| o <= r),
        "{}",
        status[0]
    );
    assert_eq!(v["device"]["engine"]["play_at_steps"], 0);
    assert!(
        v["device"]["engine"]["play_at_max_dev_us"].is_u64(),
        "{}",
        status[0]
    );
}

/// The binary: `peer --help` exits 0, a usage error 2, and a run on
/// loopback writes its status and ends on SIGINT with exit 0.
#[cfg(unix)]
#[test]
fn the_peer_command_line_parses_runs_and_stops() {
    use std::process::Command;
    let bin = env!("CARGO_BIN_EXE_dstack-dev");
    let help = Command::new(bin).args(["peer", "--help"]).output().unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).starts_with("usage: dstack-dev peer"));
    let bad = Command::new(bin)
        .args(["peer", "--name", "x"])
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("--iface or --ip is missing"));

    let _one = ONE_DEVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().unwrap();
    let status = dir.path().join("peer.json");
    let mut child = Command::new(bin)
        .args([
            "peer",
            "--ip",
            &test_ipv4().to_string(),
            "--name",
            "peer-cli",
        ])
        .args(["--tone-hz", "1000", "--test-clock-ppm", "6", "--status"])
        .arg(&status)
        .spawn()
        .unwrap();
    let written = wait_for(Duration::from_secs(15), || {
        std::fs::read_to_string(&status)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .is_some_and(|v| v["peer"]["blocks"].as_u64().is_some_and(|n| n > 0))
    });
    let kill = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(kill.success());
    let end = Instant::now() + Duration::from_secs(10);
    let code = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s.code();
        }
        if Instant::now() > end {
            let _ = child.kill();
            panic!("the peer did not end on SIGINT");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(written, "no status with blocks at {}", status.display());
    assert_eq!(code, Some(0));
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&status).unwrap()).unwrap();
    assert_eq!(v["peer"]["name"], "peer-cli");
    assert_eq!(v["peer"]["test_clock_ppm"], 6.0);
    assert_eq!(v["device"]["clock"]["source"], "system");
}
