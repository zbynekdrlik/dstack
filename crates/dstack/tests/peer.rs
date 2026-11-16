// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #305: the CI test peer (`dstack-dev peer`) on the box's routed
//! address. Two peers in this process, each on the TEST system clock with
//! its own skew, send each other a tone: each hears the other's tone whole,
//! and each one's play times show its own clock's skew against the app's
//! clock. The binary's command line parses, runs and stops on SIGINT with
//! its status written. A file of its own: its devices set the `normal`
//! timing profile (one per process).

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
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

/// Held by each test that runs a device: they bind the Dante ports on the
/// routed address.
static ONE_DEVICE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The box's IPv4 address on its default route (connecting a UDP socket
/// picks the route and sends nothing).
fn routed_ipv4() -> Ipv4Addr {
    let probe = UdpSocket::bind("0.0.0.0:0").unwrap();
    probe.connect("192.0.2.1:9").unwrap();
    match probe.local_addr().expect("the routed address") {
        SocketAddr::V4(a) => *a.ip(),
        other => panic!("no routed IPv4 address on this box: {other}"),
    }
}

fn peer_args(line: &str, dir: &Path, name: &str) -> PeerArgs {
    let mut words: Vec<String> = line.split_whitespace().map(str::to_string).collect();
    words.extend([
        "--ip".into(),
        routed_ipv4().to_string(),
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

/// A sender thread the witness watches (dstack #339): the TX thread (its
/// lane's packets) or the pump (its blocks).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Thread {
    Tx,
    Pump,
}

/// A time a sender thread did nothing while the witness ran: its count
/// stayed flat for `silent_us` (µs of the app's clock), and the witness saw
/// it rise again at `end_ms` (ms since the Unix epoch).
#[derive(Debug, Clone, Copy)]
struct Silence {
    peer: usize,
    thread: Thread,
    end_ms: u64,
    silent_us: u64,
}

impl Silence {
    /// The least silence of its thread that can lose audio, µs.
    fn loss_us(&self) -> u64 {
        match self.thread {
            Thread::Tx => TX_LOSS_US,
            Thread::Pump => PUMP_LOSS_US,
        }
    }
}

/// A count a thread raises while it runs (the TX lane's packets, the
/// pump's blocks) and the thread's own record of being late (the TX
/// lane's packets sent 5 ms or more past due and its schedule's restarts,
/// the pump's late wakes), as the witness's passes see them.
#[derive(Debug, Clone, Copy)]
struct Progress {
    count: u64,
    /// The late record at the pass that saw the count rise last.
    late: u64,
    /// That pass (ns, the app's clock).
    rose_ns: u64,
    /// The last pass since then that saw the count flat.
    flat_ns: Option<u64>,
}

impl Progress {
    fn new(count: u64, late: u64, now_ns: u64) -> Progress {
        Progress {
            count,
            late,
            rose_ns: now_ns,
            flat_ns: None,
        }
    }

    /// A pass at `now_ns` reads `count` and `late`. When the count rose
    /// after passes that saw it flat, and the late record rose with it: the
    /// silence the witness saw, µs, from the rise before to the last flat
    /// pass. A lower bound of the thread's own silence that never holds the
    /// witness's own gap into the rising pass, so a stall of the whole
    /// process reads as the witness's stall, never as a thread's silence.
    /// A thread that ran on time but sent or wrote nothing (a stack fault
    /// dropping packets) keeps its late record flat: no silence.
    fn look(&mut self, count: u64, late: u64, now_ns: u64) -> Option<u64> {
        if count == self.count {
            self.flat_ns = Some(now_ns);
            return None;
        }
        let silent = self
            .flat_ns
            .take()
            .filter(|_| late > self.late)
            .map(|flat| flat.saturating_sub(self.rose_ns) / 1_000);
        self.count = count;
        self.late = late;
        self.rose_ns = now_ns;
        silent
    }
}

/// The TX lane's count and late record for [`Progress`]: its packets, and
/// its packets sent 5 ms or more past due plus its schedule's restarts (a
/// lag past the latency sends nothing, it restarts).
fn tx_progress(lane: &LaneCounts) -> (u64, u64) {
    const _: () = assert!(LATE_EDGES_NS[LATE_BINS - 2] == 5_000_000);
    (
        lane.packets,
        lane.late_bins[LATE_BINS - 1] + lane.rebootstraps,
    )
}

/// One peer's threads as the witness watches them.
struct Sender {
    media: Arc<Media>,
    pump: Arc<PumpStats>,
    tx: Progress,
    blocks: Progress,
    /// The pump's late wakes at the last merge.
    late_wakes: u64,
    /// `writes_after_send` at the last pass.
    after_send: u64,
}

impl Sender {
    fn new(media: Arc<Media>, now_ns: u64) -> Sender {
        let pump = media.engine_stats().expect("the pump");
        let view = pump.view();
        let (packets, tx_late) = tx_progress(&media.tx.lane(0));
        let late_wakes = pump.wakes().late_wakes();
        Sender {
            tx: Progress::new(packets, tx_late, now_ns),
            blocks: Progress::new(view.blocks, late_wakes, now_ns),
            late_wakes,
            after_send: view.writes_after_send,
            media,
            pump,
        }
    }

    /// One pass of the witness at `now_ns`: peer `peer`'s silences that
    /// ended, its `writes_after_send` rise and its new late wakes, into
    /// `seen`.
    fn look(&mut self, peer: usize, now_ns: u64, seen: &mut Seen) {
        // The late wakes before the blocks: a wake is recorded before its
        // block ends, so a block's rise never comes without its late wake.
        let late = self.pump.wakes().late_wakes();
        let view = self.pump.view();
        let (packets, tx_late) = tx_progress(&self.media.tx.lane(0));
        let at = unix_ms();
        let progress = [
            (Thread::Tx, &mut self.tx, packets, tx_late),
            (Thread::Pump, &mut self.blocks, view.blocks, late),
        ];
        for (thread, p, count, late) in progress {
            if let Some(silent_us) = p.look(count, late, now_ns)
                && silent_us >= Witness::STALL_NS / 1_000
            {
                seen.silences.push(Silence {
                    peer,
                    thread,
                    end_ms: at,
                    silent_us,
                });
            }
        }
        if view.writes_after_send > self.after_send {
            self.after_send = view.writes_after_send;
            seen.after_send[peer].push(at);
        }
        // The status keeps the last 32 late wakes only: merge them as they
        // come, so a busy window keeps every one.
        if late > self.late_wakes {
            self.late_wakes = late;
            for w in self.pump.wakes().recent() {
                seen.late_wakes[peer].insert((w.unix_ms, w.behind_us));
            }
        }
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
    /// The peers' TX threads' and pumps' silences over
    /// [`Witness::STALL_NS`] while the witness ran.
    silences: Vec<Silence>,
    /// Each peer's pump late wakes, `(unix_ms, behind_us)`.
    late_wakes: [BTreeSet<(u64, u64)>; 2],
    /// When each peer's `writes_after_send` rose (ms since the Unix epoch).
    after_send: [Vec<u64>; 2],
}

/// A thread at the test's own priority that sleeps 1 ms at a time on the
/// app's clock ([`clock::app_monotonic_ns`]) and keeps every pass gap over
/// [`Witness::STALL_NS`] (the runner did not run it), when each peer's
/// loss rose (its tone gaps or ring holes), and each peer's own threads
/// (dstack #339): its TX thread's and its pump's silences while the
/// witness ran with their late records (a runner can leave one thread
/// unrun while it runs another), the pump's late wakes and its
/// `writes_after_send`. A failure names the stall or silence at each
/// loss, or none.
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
            let first = u64::try_from(last).unwrap_or(0);
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
                let stall = Stall {
                    end_ms: unix_ms(),
                    gap_us: gap_ns / 1_000,
                };
                if seen.largest.is_none_or(|l| stall.gap_us > l.gap_us) {
                    seen.largest = Some(stall);
                }
                if gap_ns > Witness::STALL_NS {
                    seen.stalls.push(stall);
                }
                let now_ns = u64::try_from(now).unwrap_or(0);
                for (peer, s) in senders.iter_mut().enumerate() {
                    s.look(peer, now_ns, &mut seen);
                }
                for (i, count) in counted.iter_mut().enumerate() {
                    let now = lost(i, &senders);
                    if now > *count {
                        seen.rises[i].push(unix_ms());
                        *count = now;
                    }
                }
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

/// Whether a thread not run for `us` until `end_ms` can be the cause of a
/// loss counted at `at` (ms since the Unix epoch): it began at or before
/// the count and ended at most [`CAUSE_WINDOW_MS`] before it. It may end
/// after the count: a TX thread stalled past the latency leaves a loss the
/// receiver counts while the sender is still not run (dev1, load ~45: a
/// 14.3 ms TX silence ended 3 ms after its gap was counted).
fn cause_of(at: u64, end_ms: u64, us: u64) -> bool {
    end_ms.saturating_sub(us.div_ceil(1_000)) <= at && at <= end_ms + CAUSE_WINDOW_MS
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

/// The peers' pump block, µs (`dstack::peer`'s 128 frames at 48 kHz).
const BLOCK_US: u64 = 128 * 1_000_000 / 48_000;

/// The least pump silence that can lose audio, µs: its blocks end a block
/// apart, so a pump that woke [`LOSS_GAP_US`] late ended none for a block
/// and that.
const PUMP_LOSS_US: u64 = BLOCK_US + LOSS_GAP_US;

/// The least TX thread silence that can lose audio, µs: a packet that
/// leaves about this late past its due reaches the receiver past its
/// latency (10 ms, less fpp 0.67 ms and the TX margin 0.25 ms), and one
/// over the latency late restarts the TX schedule; the witness can see a
/// silence up to a pass (1 ms) short.
const TX_LOSS_US: u64 = 10_000 - 1_000 - 1_000;

/// Peer `me`'s failure message (dstack #339): the verdict, each loss's
/// count time (a tone gap's or a ring hole's) with the evidence around it
/// ([`cause_of`]), the counts, what the witness saw of the threads and both
/// peers' statuses. Evidence is a thread late enough to lose audio: the
/// witness's own stall (the whole process), the sender's TX thread or
/// either peer's pump silent while the witness ran with its own late
/// record risen ([`Progress`], [`TX_LOSS_US`], [`PUMP_LOSS_US`]), or either
/// pump's late wake ([`LOSS_GAP_US`]). Either pump, since the receiver's
/// pump restart is a counted cause too; never the receiver's TX thread,
/// which sends, never hears. `RUNNER` when the loss has a counted cause
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
                .filter(|s| s.gap_us >= LOSS_GAP_US + 1_000 && cause_of(at, s.end_ms, s.gap_us))
                .map(|s| format!("witness {} us, {}", s.gap_us, when(at, s.end_ms)))
                .collect();
            for s in seen
                .silences
                .iter()
                .filter(|s| s.thread == Thread::Pump || s.peer == other)
                .filter(|s| s.silent_us >= s.loss_us() && cause_of(at, s.end_ms, s.silent_us))
            {
                stalls.push(format!(
                    "peer {}'s {:?} thread silent {} us, {}",
                    s.peer,
                    s.thread,
                    s.silent_us,
                    when(at, s.end_ms)
                ));
            }
            for (peer, w) in seen.late_wakes.iter().enumerate() {
                for &(end, us) in w
                    .iter()
                    .filter(|&&(end, us)| us >= LOSS_GAP_US && cause_of(at, end, us))
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
    let silences: Vec<String> = seen
        .silences
        .iter()
        .map(|s| {
            format!(
                "peer {} {:?} {} us at +{} ms",
                s.peer,
                s.thread,
                s.silent_us,
                s.end_ms.saturating_sub(started_ms)
            )
        })
        .collect();
    format!(
        "{verdict}\npeer {me}: {mine:?}\nsender (peer {other}): {theirs:?}\n{gaps:?}\n\
         witness: {} stalls over {} us, largest {:?}\n\
         threads silent over {} us while the witness ran: {silences:?}\n\
         writes_after_send rose at (ms after the start): peer 0 {:?}, peer 1 {:?}\n\
         pump late wakes seen: peer 0 {}, peer 1 {}\n\
         peer {me} status: {}\npeer {other} status: {}",
        seen.stalls.len(),
        Witness::STALL_NS / 1_000,
        seen.largest,
        Witness::STALL_NS / 1_000,
        since(&seen.after_send[0]),
        since(&seen.after_send[1]),
        seen.late_wakes[0].len(),
        seen.late_wakes[1].len(),
        status[me],
        status[other]
    )
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

/// The witness's silence of a thread (dstack #339) runs from its rise
/// before to the last pass that saw it flat: none for a count that rose
/// each pass, never the witness's own gap into the rising pass, and only
/// when the thread's own late record rose by the rise that ends it.
#[test]
fn a_thread_s_silence_is_what_the_witness_saw_flat_and_late() {
    let ms = |m: u64| m * 1_000_000;
    let mut p = Progress::new(10, 0, 0);
    assert_eq!(p.look(11, 0, ms(1)), None);
    assert_eq!(p.look(12, 0, ms(2)), None);
    assert_eq!(p.look(12, 0, ms(3)), None);
    // The late record may rise while the count is still flat.
    assert_eq!(p.look(12, 1, ms(11)), None);
    // Flat from the rise at 2 ms to the pass at 11 ms; the witness itself
    // was not run from 11 to 40 ms, which is not the thread's.
    assert_eq!(p.look(13, 1, ms(40)), Some(9_000));
    assert_eq!(p.look(14, 1, ms(41)), None);
    assert_eq!(p.look(14, 1, ms(42)), None);
    // Late in the rising pass itself.
    assert_eq!(p.look(15, 2, ms(43)), Some(1_000));
    // Flat 20 ms with no late record (a thread that ran and sent nothing):
    // no silence; the next flat stretch is judged on its own.
    assert_eq!(p.look(15, 2, ms(63)), None);
    assert_eq!(p.look(16, 2, ms(64)), None);
    assert_eq!(p.look(16, 2, ms(70)), None);
    assert_eq!(p.look(17, 3, ms(71)), Some(6_000));
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
    assert!(cause_of(at, at - CAUSE_WINDOW_MS, 6_000));
    assert!(!cause_of(at, at - CAUSE_WINDOW_MS - 1, 6_000));
    // Began before the count and ended after it (a TX thread still stalled).
    assert!(cause_of(at, at + 3, 14_300));
    assert!(cause_of(at, at + 15, 15_000));
    // Began after the count: not its cause (a part of a ms rounds up).
    assert!(!cause_of(at, at + 15, 14_000));
    assert!(cause_of(at, at + 15, 14_001));
    assert_eq!(when(at, at + 3), "ended 3 ms after");
    assert_eq!(when(at, at - 7), "ended 7 ms before");
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
    assert_eq!(v["device"]["engine"]["play_at_steps"], 0);
    assert!(
        v["device"]["engine"]["play_at_max_dev_us"].is_u64(),
        "{}",
        status[0]
    );
}

/// The binary: `peer --help` exits 0, a usage error 2, and a run on the
/// routed address writes its status and ends on SIGINT with exit 0.
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
            &routed_ipv4().to_string(),
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
