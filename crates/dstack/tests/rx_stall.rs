//! A stalled RX helper thread and the samples the pump reads (dstack #326,
//! strih-snv 15-16.11.2026): under OBS's load the scheduler left the
//! normal-priority RX thread behind by up to 17.5 ms, more than the 13 ms
//! between a packet's arrival and the pump's read of its samples, and the
//! thread then wrote them behind the reader (`written_late`, the ring's
//! `late`, and as many holes).
//!
//! Real threads on loopback: a sender sends a flow in real time to our RX
//! slot, [`LEAD`] ahead of its media time, the RX thread and the engine's
//! pump run on the media clock, and the RX thread is stalled [`STALL`] at
//! random moments (`stall_thread`), past the time a packet waits in its
//! socket before its read (the lead, the 8 ms receive slack and a block:
//! 57 ms). In the normal profile the pump drains the flow sockets itself,
//! so no sample is late and none is a hole; in the
//! low-latency profile (the RX thread writes, on real-time priority in the
//! field) the same stalls land samples behind the reader, which proves the
//! stalls bite.
//!
//! Which thread lost a sample (dstack #339: a windows-2025 run lost 1920
//! samples in the normal profile, every one written after its read): the
//! sender keeps each packet's send time and its own loop's largest gap, and
//! the processor the positions it read as holes with the time of that read
//! (each sample encodes its position and is never 0). A hole is the
//! sender's when its packet's send began at or after the read (so after
//! that block's drain: not in its socket), else ours. Either way the run
//! fails: a window where the sender did not run tested nothing; the message
//! says whose it was.

use std::net::{Ipv4Addr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dstack::clock::{self, PtpClock, SkewedClock, TestClockPpm};
use dstack::device::{DeviceConfig, TimingProfile};
use dstack::media::packet;
use dstack::media::ring::{Counters, Ring};
use dstack::media::rx::{self, RxCounts, RxPlan, RxRoute, RxShared};
use dstack::media::tx::{Scheduling, TxShared};
use dstack::stream::wake::LateWake;
use dstack::stream::{self, Map, Pump, PumpStats, PumpView, Timing, to_f64};
use dstack_audio_io::{Block, Process};

const RATE: u32 = 48_000;
/// Frames a packet.
const FPP: u64 = 16;
/// The pump's block, its playout point (the receive slack, 8 ms) and its
/// margin, samples.
const BLOCK: u64 = 64;
const PLAYOUT: u64 = 384;
const MARGIN: u64 = 288;
/// How long the RX thread stalls each time: past the time a packet waits
/// in its socket before its read ([`LEAD`] + [`PLAYOUT`] + [`BLOCK`],
/// 57 ms), so a stalled RX thread that writes lands samples behind the
/// reader (the low-latency control).
const STALL: Duration = Duration::from_millis(70);
const STALLS: usize = 6;
/// The flow's length.
const RUN: Duration = Duration::from_secs(3);
/// How early the sender sends a packet, samples (48 ms, dstack #339): a
/// sender thread late by less than this plus the slack and a block (57 ms)
/// still lands in time, so only the RX side decides the counts. A shared
/// runner left the sender unrun for 16-35 ms (windows-2025), which a 4 ms
/// lead turned into holes. The ring stores a write up to its `SLOTS`
/// (65536 samples) ahead of the reader, so every early packet is kept
/// (`early` stays 0, asserted). The normal-profile test therefore catches
/// only an RX writer later than the lead plus the slack and a block (57 ms,
/// was 13 ms): the RX thread's [`STALL`] is past it, which the low-latency
/// control proves; a pump that drains late by less goes unseen here.
const LEAD: u64 = 2304;
// The stall stays at least 10 ms past that wait (the control's margin), in
// samples so nothing rounds.
const _: () = assert!(
    LEAD + PLAYOUT + BLOCK + RATE as u64 / 100 <= STALL.as_millis() as u64 * RATE as u64 / 1000
);
// Every early packet fits the ring ahead of its reader.
const _: () = assert!(LEAD + PLAYOUT + BLOCK < dstack::media::ring::SLOTS as u64);

/// The sample the sender sends at position `pos` (left-justified 24 bit):
/// the position mod 65535, plus one, so no sample is 0, which a hole reads.
fn sample_at(pos: u64) -> i32 {
    i32::try_from((pos % 65_535 + 1) << 8).unwrap()
}

/// A run of positions one block read as holes after the flow was first
/// heard: from `first`, `n` samples, read at media time `read` (samples,
/// the clock read in the processor, after the block's drain and read).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Hole {
    first: u64,
    n: u64,
    read: u64,
}

/// What the processor read: the hole runs, the holes before the flow was
/// first heard (its start: reads before any write, a new flow's first
/// drain dropping what waited), and samples that are neither their
/// position's value nor a hole.
#[derive(Debug, Default)]
struct Heard {
    holes: Vec<Hole>,
    start: u64,
    misplaced: u64,
}

/// The app's processor: hears the inputs, sends nothing; records into
/// [`Heard`], at most one lock a block (only a block with a hole).
struct Listen {
    clock: Arc<dyn PtpClock>,
    heard: Arc<Mutex<Heard>>,
    /// A sample of the flow was read: holes from here on are the flow's.
    started: bool,
    /// This block's runs, `(first, n)` (capacity kept).
    runs: Vec<(u64, u64)>,
}

impl Process for Listen {
    fn process(&mut self, block: &mut Block<'_>) {
        let Some(start) = block.start() else { return };
        self.runs.clear();
        let (mut before, mut misplaced) = (0, 0);
        for (i, &x) in block.input(0).iter().enumerate() {
            let pos = start + i as u64;
            if x == to_f64(sample_at(pos)) {
                self.started = true;
            } else if x != 0.0 {
                misplaced += 1;
            } else if !self.started {
                before += 1;
            } else {
                match self.runs.last_mut() {
                    Some((first, n)) if *first + *n == pos => *n += 1,
                    _ => self.runs.push((pos, 1)),
                }
            }
        }
        if before + misplaced == 0 && self.runs.is_empty() {
            return;
        }
        let read = self
            .clock
            .now_ns()
            .map_or(0, |ns| clock::to_samples(ns, RATE));
        let mut heard = self.heard.lock().unwrap_or_else(PoisonError::into_inner);
        heard.start += before;
        heard.misplaced += misplaced;
        heard
            .holes
            .extend(self.runs.iter().map(|&(first, n)| Hole { first, n, read }));
    }
}

/// What one flow left: the device's counts and each thread's evidence.
struct Run {
    ring: Counters,
    lane: RxCounts,
    pump: PumpView,
    /// The pump's recent late wakes.
    late_wakes: Vec<LateWake>,
    /// The flow's start, ms since the Unix epoch.
    began_unix_ms: u64,
    heard: Heard,
    /// The first packet's position.
    first: u64,
    /// Each packet's send time (media samples, read just before its send),
    /// in position order from `first`, one per [`FPP`].
    sent: Vec<u64>,
    /// The sender loop's largest gap between two passes, ns of the app's
    /// clock (QPC on Windows), and when it ended, ms since the flow began.
    sender_gap: (u64, u64),
}

/// Whose a lost sample is ([`attribute`]).
#[derive(Debug, Default, PartialEq, Eq)]
struct Blame {
    /// Hole samples whose packet left after their read.
    sender: u64,
    /// Hole samples past the last packet sent (the flow had ended).
    unsent: u64,
    /// Hole samples whose packet may have left before their read: the
    /// device's, and the first position of each run that has any.
    ours: u64,
    ours_at: Vec<u64>,
}

/// Splits the holes between the sender and us: a hole at `x` (from the
/// flow's `first` position) is the sender's when its packet's send time
/// (`sent`, samples, read before the send, one per [`FPP`] from `first`)
/// is at or after the hole's `read` (the processor's clock read, after
/// that block's drain): the packet was not in its socket at the drain.
/// Any earlier send is ours, since the packet may have waited there. A
/// hole past the last packet sent is `unsent`.
fn attribute(holes: &[Hole], first: u64, sent: &[u64]) -> Blame {
    let mut blame = Blame::default();
    for hole in holes {
        let mut run_ours = false;
        for x in hole.first..hole.first + hole.n {
            let packet = x
                .checked_sub(first)
                .map(|k| usize::try_from(k / FPP).unwrap());
            match packet.and_then(|k| sent.get(k)) {
                Some(&at) if at >= hole.read => blame.sender += 1,
                None if x >= first => blame.unsent += 1,
                _ => {
                    blame.ours += 1;
                    run_ours = true;
                }
            }
        }
        if run_ours {
            blame.ours_at.push(hole.first);
        }
    }
    blame
}

impl Run {
    /// The failure message: the verdict, then the evidence.
    fn report(&self) -> String {
        let blame = attribute(&self.heard.holes, self.first, &self.sent);
        let verdict = if blame.ours > 0 {
            format!(
                "OURS: {} hole samples came in packets that may have been in their socket \
                 at the read (runs from {:?})",
                blame.ours, blame.ours_at
            )
        } else if blame.sender > 0 {
            format!(
                "SENDER: all {} hole samples came in packets the test's sender sent after \
                 their read (its loop's largest gap {:.1} ms, ending at +{} ms): the runner \
                 did not run the sender, so this run tested nothing there",
                blame.sender,
                self.sender_gap.0 as f64 / 1e6,
                self.sender_gap.1
            )
        } else {
            "OURS: no hole the processor read explains the counts".to_string()
        };
        let ms = |pos: u64| pos.saturating_sub(self.first) * 1000 / u64::from(RATE);
        // The blocks' runs, joined where they continue each other.
        let mut joined: Vec<(u64, u64)> = Vec::new();
        for h in &self.heard.holes {
            match joined.last_mut() {
                Some((first, n)) if *first + *n == h.first => *n += h.n,
                _ => joined.push((h.first, h.n)),
            }
        }
        let runs: Vec<String> = joined
            .iter()
            .map(|&(first, n)| format!("{n} at +{} ms", ms(first)))
            .collect();
        let wakes: Vec<String> = self
            .late_wakes
            .iter()
            .map(|w| {
                format!(
                    "{} us at +{} ms{}",
                    w.behind_us,
                    w.unix_ms.saturating_sub(self.began_unix_ms),
                    if w.rebootstrap { " (re-bootstrap)" } else { "" }
                )
            })
            .collect();
        format!(
            "{verdict}\nhole runs: {runs:?}; {} past the flow's end; {} before it was heard; \
             misplaced {}\nsender: largest gap {:.1} ms ending at +{} ms\npump: {:?}\n\
             pump late wakes: {wakes:?}\nring {:?}\nlane {:?}",
            blame.unsent,
            self.heard.start,
            self.heard.misplaced,
            self.sender_gap.0 as f64 / 1e6,
            self.sender_gap.1,
            self.pump,
            self.ring,
            self.lane
        )
    }
}

fn unix_ms() -> u64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    u64::try_from(since.as_millis()).unwrap()
}

/// The ring's, the slot's and the threads' evidence after a flow whose RX
/// thread stalled [`STALLS`] times, under `profile`.
fn stalled_run(profile: TimingProfile) -> Run {
    let mut cfg = DeviceConfig::from_toml(include_str!("../../../config/test-device.toml"))
        .expect("config/test-device.toml");
    cfg.sample_rate = RATE;
    cfg.block = BLOCK;
    cfg.timing = profile;
    let rings: Arc<[Ring]> = (0..cfg.rx.len())
        .map(|_| Ring::new())
        .collect::<Vec<_>>()
        .into();
    let shared = Arc::new(RxShared::new(RATE, Ipv4Addr::LOCALHOST, Arc::clone(&rings)).unwrap());
    shared.publish(
        |slot| {
            if slot == 0 {
                RxPlan {
                    src: Some(Ipv4Addr::LOCALHOST),
                    bps: 3,
                    nslots: 1,
                    latency: PLAYOUT as u32,
                    delay: 0,
                    block: BLOCK as u16,
                    live: false,
                    generation: 1,
                    group: None,
                }
            } else {
                RxPlan::default()
            }
        },
        |rx| (rx == 0).then_some(RxRoute { flow: 0, slot: 0 }),
    );
    shared.set_device_latency(PLAYOUT * 1_000_000_000 / u64::from(RATE));
    let tx = Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, rings, 1, &[]).unwrap());
    let timing = Timing::new(BLOCK, PLAYOUT, MARGIN);
    let stats = Arc::new(PumpStats::new(timing, RATE));
    let map = Map {
        rx: vec![0],
        tx: Vec::new(),
    };
    let media: Arc<dyn PtpClock> = Arc::new(SkewedClock::new(
        clock::app_monotonic_ns(),
        0,
        TestClockPpm(0.0),
    ));
    let heard = Arc::new(Mutex::new(Heard::default()));
    let listen = Listen {
        clock: Arc::clone(&media),
        heard: Arc::clone(&heard),
        started: false,
        runs: Vec::with_capacity(BLOCK as usize),
    };
    let pump = Pump::new(
        listen,
        map,
        timing,
        24,
        Arc::clone(&shared),
        tx,
        Arc::clone(&stats),
    );
    let rx_thread = rx::spawn(
        Arc::clone(&media),
        Arc::clone(&shared),
        None,
        Arc::new(Scheduling::new()),
    )
    .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let pump = stream::spawn(Arc::clone(&media), pump, Arc::clone(&stop)).unwrap();

    // The sender: each packet [`LEAD`] before its last frame is due, in
    // real time.
    let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let dest = shared.local_addr(0).unwrap();
    let samples = |ns: u64| clock::to_samples(ns, RATE);
    let first = samples(media.now_ns().unwrap()) / FPP * FPP;
    let mut next = first;
    let mut sent = Vec::with_capacity(12_000);
    let mut out = [0u8; 1500];
    // The stalls' moments: spread over the run after a settled start,
    // jittered by a fixed LCG.
    let mut seed = 0x2b7e_1516_28ae_d2a6u64;
    let stalls: Vec<Duration> = (0..STALLS)
        .map(|k| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            Duration::from_millis(500 + 400 * k as u64 + (seed >> 33) % 200)
        })
        .collect();
    let began = Instant::now();
    let began_unix_ms = unix_ms();
    let began_app = clock::app_monotonic_ns();
    let mut last_pass = began_app;
    let mut sender_gap = (0, 0);
    let mut stalled = 0;
    while began.elapsed() < RUN {
        let pass = clock::app_monotonic_ns();
        let gap = u64::try_from(pass - last_pass).unwrap_or(0);
        if gap > sender_gap.0 {
            sender_gap = (
                gap,
                u64::try_from(pass - began_app).unwrap_or(0) / 1_000_000,
            );
        }
        last_pass = pass;
        if stalls.get(stalled).is_some_and(|&at| began.elapsed() >= at) {
            shared.stall_thread(u64::try_from(STALL.as_nanos()).unwrap());
            stalled += 1;
        }
        let now = samples(media.now_ns().unwrap());
        while next + FPP <= now + LEAD {
            let (secs, sub) = clock::stamp(next, RATE);
            let n = packet::write(&mut out, secs, sub, 1, 3, FPP as usize, |f, _| {
                sample_at(next + f as u64)
            });
            sent.push(samples(media.now_ns().unwrap()));
            sender.send_to(&out[..n], dest).unwrap();
            next += FPP;
        }
        std::thread::sleep(Duration::from_micros(250));
    }
    assert_eq!(stalled, STALLS);
    // The pump stops before the flow does: its last reads find the flow.
    stop.store(true, Ordering::Release);
    pump.join().unwrap();
    shared.stop();
    rx_thread.join().unwrap();
    let heard = std::mem::take(&mut *heard.lock().unwrap_or_else(PoisonError::into_inner));
    Run {
        ring: shared.rings()[0].counters(),
        lane: shared.lane(0),
        pump: stats.view(),
        late_wakes: stats.wakes().recent(),
        began_unix_ms,
        heard,
        first,
        sent,
        sender_gap,
    }
}

#[test]
fn a_stalled_rx_thread_loses_no_sample_in_the_normal_profile() {
    let run = stalled_run(TimingProfile::Normal);
    // About 9000 packets in 3 s.
    assert!(run.lane.packets > 8_000, "{:?}", run.lane);
    // The lead is inside what the ring stores ahead of its reader.
    assert_eq!(run.ring.early, 0, "{}", run.report());
    assert_eq!(
        (
            run.ring.late,
            run.ring.holes,
            run.lane.late_at_read,
            run.heard.misplaced
        ),
        (0, 0, 0, 0),
        "{}",
        run.report()
    );
}

#[test]
fn a_stalled_rx_thread_writes_behind_the_reader_when_it_writes() {
    // The control: the same stalls with the RX thread as the writer.
    let run = stalled_run(TimingProfile::LowLatency);
    assert!(run.lane.packets > 8_000, "{:?}", run.lane);
    assert_eq!(run.ring.early, 0, "{}", run.report());
    assert!(run.ring.late > 0, "{}", run.report());
    assert!(run.lane.late_at_read > 0, "{}", run.report());
    // Every 0 the processor read is a ring hole or start hole, and the
    // stalls' holes are ours (the sender sent them in time): the evidence
    // names the right thread.
    let holes: u64 = run.heard.holes.iter().map(|h| h.n).sum();
    assert_eq!(
        holes + run.heard.start,
        run.ring.holes + run.ring.start_holes,
        "{}",
        run.report()
    );
    let blame = attribute(&run.heard.holes, run.first, &run.sent);
    assert!(blame.ours > 0, "{}", run.report());
    assert_eq!(run.heard.misplaced, 0, "{}", run.report());
}

/// [`attribute`] on numbers: packets of [`FPP`] from 100.
#[test]
fn a_hole_is_the_sender_s_only_when_its_packet_left_after_its_read() {
    let hole = |first, n, read| Hole { first, n, read };
    // Packet 0 (100..116) left at 110, packet 1 (116..132) at 126,
    // packet 2 (132..148) at 200.
    let sent = [110, 126, 200];
    // Read at 126: packet 0 (sent 110) may have waited in its socket.
    assert_eq!(
        attribute(&[hole(115, 1, 126)], 100, &sent),
        Blame {
            ours: 1,
            ours_at: vec![115],
            ..Blame::default()
        }
    );
    // Sent at the read: the sender's; one sample before it: ours.
    assert_eq!(attribute(&[hole(115, 1, 110)], 100, &sent).sender, 1);
    assert_eq!(attribute(&[hole(115, 1, 111)], 100, &sent).ours, 1);
    // One block's run over packets 1 and 2, read at 150: 116..132 (sent
    // 126) ours, 132..148 (sent 200) the sender's.
    assert_eq!(
        attribute(&[hole(116, 32, 150)], 100, &sent),
        Blame {
            sender: 16,
            ours: 16,
            ours_at: vec![116],
            ..Blame::default()
        }
    );
    // Past the last packet sent: unsent, no one's verdict.
    assert_eq!(
        attribute(&[hole(148, 2, 300)], 100, &sent),
        Blame {
            unsent: 2,
            ..Blame::default()
        }
    );
    // Before the first packet: ours (the processor records none there).
    assert_eq!(attribute(&[hole(99, 1, 300)], 100, &sent).ours, 1);
    // Two runs, one each.
    assert_eq!(
        attribute(&[hole(100, 1, 200), hole(140, 2, 150)], 100, &sent),
        Blame {
            sender: 2,
            ours: 1,
            ours_at: vec![100],
            ..Blame::default()
        }
    );
}

/// The sample values: never 0 (a hole), and distinct for 65535 positions
/// in a row.
#[test]
fn every_sent_sample_reads_apart_from_a_hole() {
    assert_eq!(sample_at(0), 1 << 8);
    assert_eq!(sample_at(65_534), 65_535 << 8);
    assert_eq!(sample_at(65_535), 1 << 8);
    assert!((0..200_000).all(|p| sample_at(p) != 0));
}
