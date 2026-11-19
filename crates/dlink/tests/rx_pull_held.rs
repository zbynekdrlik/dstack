// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #307 (field, 17.11.2026): mbc's dlisten counted 71 bridge
//! underruns and 53 re-centres in 13.5 h, in bursts at the seconds its
//! threads were held 19 to 46 ms (every timer wake there goes through a
//! CPU that system work holds), while the packets arrived on time. dlisten
//! pulls its RX in the output callback ([`dlink::bridge::RxPull`]), so the
//! thread held there is the output callback itself, not the pump
//! (`tests/rx_pull.rs` holds the pump).
//!
//! Here a dlink TX device sends a constant and a pulled RX device (an idle
//! pump, as dlisten's) is read by a simulated WASAPI shared-mode output:
//! the card's engine reads the client's buffer one period at each of its
//! ticks (every 10 ms, on its own clock; a tick that finds less plays
//! silence for what is missing), and each event, signalled at a tick, runs
//! the callback, which fills the room the buffer has (cpal's
//! `max_frames_in_buffer - padding`). The buffer is 1056 frames, as
//! Windows sizes it for a 480-frame period, so a tick can find part of a
//! period. A callback held past what the buffer still holds leaves the
//! card playing silence meanwhile (the card's gap, not the bridge's). Some
//! events are held 46 ms (the field's worst) or 20 ms, or 25 ms with the
//! next event 6 ms late, or (dstack #433, windows-2025's timer: every wake
//! about 1 ms late) every event 1 ms late and the held one to 0.9 ms past
//! a tick, less late than the calls before it. The bridge must play every
//! frame it is asked for at the level (no underrun, no re-centre), and the
//! latency after a hold must be the latency before it: the frames due
//! while the card played silence are dropped, counted as the output's own
//! gap ([`dlink::bridge::Stats::output_gaps`]), never played late.
//!
//! The level is judged in the callbacks whose read met no ring hole (nor
//! the one after it, which the converter's filter reaches): a sender
//! thread not run long enough (a loaded box) leaves ring holes, which play
//! as samples off the level with no underrun (the `rx_pull.rs` rule: holes
//! with no underrun is the sender), and is still judged on the bridge's
//! own counts. A sender's pump restart after a late wake leaves its engine
//! ring's positions unwritten, which its TX thread sends as silence: whole
//! packets, so no RX counter sees them, and they play as samples off in a
//! judged callback (dstack #436: plain at dev1 load ~24 that failed 8 of 9
//! runs, read `OURS`). The judged window therefore also reads the sender's
//! pump restarts, the silence it sent and its TX restarts with their lines
//! (`tests/consumer/sender.rs` and `tests/tx_restart/restarts.rs`, shared
//! with the consumer witness), and
//! a failure prints `verdict SENDER|OURS` ([`who`]); the asserts are the
//! same either way. A file of its own (its process sets the normal
//! profile), on loopback (dstack #366) and its own system clock.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dlink::bridge::player::SETTLE_CALLS;
use dlink::bridge::{Player, Stats, fifo_for};
use dlink::{Block, Clock, Config, Link, Map, Media, Place, Ports, Process, Stopped, SubStatus};

#[path = "tx_restart/restarts.rs"]
mod restarts;
use restarts::Restarts;
#[path = "consumer/sender.rs"]
mod sender;
use sender::{GAP_EDGES, Noted, Pump, SenderAt};

/// One device set at a time in this process: both tests bind the same
/// ports on 127.0.0.1.
static ONE_DEVICE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The test device's address: loopback (dstack #366).
fn test_ipv4() -> std::net::Ipv4Addr {
    std::net::Ipv4Addr::LOCALHOST
}

/// The level the sender plays.
const LEVEL: f64 = 0.5;
/// The app's, the network's and the card's rate.
const RATE: u32 = 48_000;
/// The card's engine period: 10 ms, WASAPI shared mode's default.
const PERIOD: Duration = Duration::from_millis(10);
const PERIOD_FRAMES: u64 = 480;
/// The client's buffer: two periods and a fraction, as Windows sizes a
/// shared-mode buffer (1056 frames for a 480-frame period at 48 kHz), so a
/// tick can find part of a period.
const CARD_FRAMES: u64 = 2 * PERIOD_FRAMES + 96;
/// The settling (priming, the converter's filters), then the window
/// judged.
const SETTLE: Duration = Duration::from_secs(2);
const JUDGED: Duration = Duration::from_secs(8);
/// A held event about every this many events (0.7 s). Not a multiple of
/// 4: the converter's 128-frame blocks meet the 480-frame calls in four
/// phases (480 mod 128 = 96), and the holds meet each of them (dstack
/// #433: at 70 they alternated two, and a dry call a period over the
/// excess bound in another phase never ran).
const HOLD_EVERY: u64 = 71;
/// The events after a hold that wake late in the late-wake case (more
/// than the gap's settling calls: a held PC's burst).
const LATE_EVENTS: u32 = 6;

/// [`LEVEL`] on output 0.
struct Constant;

impl Process for Constant {
    fn process(&mut self, block: &mut Block<'_>) {
        block.output(0).fill(LEVEL as f32 as f64);
    }
}

/// dlisten's processor: the pump has nothing to do.
struct Idle;

impl Process for Idle {
    fn process(&mut self, _block: &mut Block<'_>) {}
}

/// What the simulated output saw over the judged window.
#[derive(Debug, Default)]
struct Heard {
    /// Samples the callback wrote at the level and off it, in callbacks
    /// whose read met no ring hole (nor the one before: the converter's
    /// filter carries a hole into the next callback).
    on: u64,
    off: u64,
    /// Callbacks whose read met a ring hole (the sender's, not judged).
    hole_calls: u64,
    /// The bridge's underruns and re-centres in the judged window.
    underruns: u64,
    recentres: u64,
    /// The output's gaps the player counted, and the input frames it
    /// dropped for them.
    output_gaps: u64,
    output_lost: u64,
    /// The RX ring's holes in the judged window (the sender's audio that
    /// never came), and those written later (late packets).
    ring_holes: u64,
    ring_late: u64,
    /// What the sender did in the judged window.
    sender: Sent,
    /// Held events, and the frames the card played as silence meanwhile
    /// (its own gap: the callback was not there to fill it).
    holds: u64,
    card_silence: u64,
    /// The whole wait of a sample from its due time to the card's output,
    /// frames, summed at each callback (the FIFO's depth after the pull and
    /// the card's padding before the write; the FIFO's depth after the
    /// fill and the card's padding after the write), split into the looks
    /// in the second before the first hold and the looks after a hold's
    /// first two callbacks.
    before: (u64, u64),
    after: (u64, u64),
}

impl Heard {
    fn mean_ms(sum: (u64, u64)) -> f64 {
        sum.0 as f64 / sum.1.max(1) as f64 * 1e3 / f64::from(RATE)
    }

    /// The ring's holes never written in the window: packets the sender
    /// never sent.
    fn never_written(&self) -> u64 {
        self.ring_holes.saturating_sub(self.ring_late)
    }
}

/// What the sender did in a judged window (dstack #436): its pump's
/// restarts and the silence they sent, from [`sender::SILENCE_DELAY`]
/// before the open, and its TX schedule's restarts with their lines
/// (dstack #401).
#[derive(Debug, Clone, Default)]
struct Sent {
    pump: Pump,
    tx: Restarts,
}

/// Who lost a judged window's audio (dstack #436).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Who {
    /// The sender's pump or TX thread was not run.
    Sender,
    /// The bridge (or nothing explains it).
    Ours,
}

/// Who lost `heard`'s audio, with the bridge's own counts `bridge_clean`
/// (no underrun, re-centre or pull restart, the gaps' drops the card's
/// silence, the latency kept). `SENDER` when the bridge is clean and every
/// loss has the sender's evidence, the `rx_pull` rule (the consumer
/// witness, `tests/consumer/witness.rs`) plus the sender's pump:
/// - a pump restart is the sender's only after a late wake (more than W
///   behind, `rebootstraps_late`), and its TX thread sends the positions
///   it left unwritten as silence, which no RX counter sees;
/// - ring holes never written only within the sender's TX restarts
///   ([`Restarts::explain`]: every restart's sleep returned the flow's
///   latency late, its bound at least its own receiver's, the holes within
///   the restarts' bound); any TX restart without that evidence is `OURS`;
/// - samples off only within what those gaps can cost: the silence of the
///   window's late pump restarts and the holes never written (a hole's
///   call and the next are not judged, yet the FIFO holds over a period,
///   so a hole's tail can play in a judged call), and [`GAP_EDGES`] a
///   gap with evidence.
///
/// A TX restart just before the open whose holes land inside is
/// unexplained (`OURS`). The pump's counts open [`sender::SILENCE_DELAY`]
/// before the window (the silence is counted when sent and played up to
/// that much later), so a pump restart just before the open whose silence
/// plays inside is counted. Three edges err the other way: silence sent
/// within that delay before the open that played before it, and silence
/// sent before the close that plays after it (each at most that delay's
/// silence), and a hole read before the open and written late inside,
/// which lowers the holes never written (at most one restart's).
fn who(heard: &Heard, bridge_clean: bool) -> Who {
    let s = &heard.sender;
    let holes = heard.never_written();
    let lost = heard.off > 0 || holes > 0;
    let pump = s.pump.late == s.pump.restarts;
    let silence = if s.pump.restarts > 0 {
        s.pump.silence
    } else {
        0
    };
    let pump_gaps = if silence > 0 { s.pump.restarts } else { 0 };
    let tx_gaps = if holes > 0 {
        s.tx.lines.len() as u64
    } else {
        0
    };
    let gaps = pump_gaps + tx_gaps;
    let off = heard.off <= silence + holes + GAP_EDGES * gaps;
    let tx = (holes == 0 || s.tx.explain(holes)) && (!s.tx.any() || s.tx.runner());
    if lost && bridge_clean && pump && off && tx {
        Who::Sender
    } else {
        Who::Ours
    }
}

/// The verdict of a judged window ([`who`]) and its evidence.
fn verdict(heard: &Heard, bridge_clean: bool) -> String {
    let s = &heard.sender;
    let who = match who(heard, bridge_clean) {
        Who::Sender => "SENDER (the sender's pump or TX thread was not run)".to_string(),
        Who::Ours => format!("OURS{}", s.tx.ours()),
    };
    format!(
        "verdict {who}: {} samples off, ring holes never written {}, the sender's pump {} \
         restarts ({} late), {} samples sent as silence, the sender's {:?}",
        heard.off,
        heard.never_written(),
        s.pump.restarts,
        s.pump.late,
        s.pump.silence,
        s.tx
    )
}

/// How the event thread wakes: each held event `hold` late, the
/// [`LATE_EVENTS`] events after it `late_after` late, and every event
/// `every` later still (a runner's timer that wakes each sleep late).
#[derive(Debug, Clone, Copy)]
struct Wakes {
    hold: Duration,
    late_after: Duration,
    every: Duration,
}

/// What the card's thread works with: the receiver's pull, its player
/// and their FIFO and counts, the receiving device's media (its ring), the
/// sender's counts, and the flag set once the channel reads Receiving.
struct Card {
    pull: dlink::bridge::RxPull,
    player: Player,
    fifo: Arc<dlink::bridge::Fifo>,
    stats: Arc<Stats>,
    media: Arc<Media>,
    sender: sender::Sender,
    receiving: Arc<AtomicU64>,
}

/// The card and its event thread (module doc), woken as `wakes` says:
/// returns what it heard, and what the sender did meanwhile.
fn output(wakes: Wakes, card: Card) -> Heard {
    let Card {
        mut pull,
        mut player,
        fifo,
        stats,
        media,
        sender,
        receiving,
    } = card;
    let mut out = vec![0f32; CARD_FRAMES as usize];
    let mut heard = Heard::default();
    let began = Instant::now();
    let holes = || media.rx.rings()[0].counters().holes;
    // The engine's ticks read so far, and what the client's buffer holds.
    let mut ticks = 0u64;
    let mut padding = 0u64;
    let mut next = began;
    let mut late_left = 0;
    let mut hole_before = false;
    let mut events = 0u64;
    let mut judging: Option<(Instant, [u64; 6], SenderAt)> = None;
    // The sender's pump at the recent calls: a window's counts open
    // SILENCE_DELAY before it (the silence is counted when sent).
    let mut noted = Noted::default();
    let mut since = None;
    // Callbacks since the last hold (the latency is judged two after it).
    let mut after_hold = u64::MAX;
    loop {
        next += PERIOD;
        events += 1;
        let held = judging.is_some() && events.is_multiple_of(HOLD_EVERY);
        let late = if held {
            late_left = LATE_EVENTS;
            wakes.hold
        } else if late_left > 0 {
            late_left -= 1;
            wakes.late_after
        } else {
            Duration::ZERO
        };
        let wake = next + late + wakes.every;
        if let Some(wait) = wake.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        let now = Instant::now();
        // The events stay on the engine's ticks: the ones missed are gone.
        while next + PERIOD <= now {
            next += PERIOD;
        }
        // Each tick passed reads a period; what it does not find plays as
        // silence.
        let mut silence = 0;
        let now_ticks = (now - began).as_nanos() as u64 / PERIOD.as_nanos() as u64;
        for _ in ticks..now_ticks {
            let take = padding.min(PERIOD_FRAMES);
            silence += PERIOD_FRAMES - take;
            padding -= take;
        }
        ticks = now_ticks;
        let before_padding = padding;
        let room = (CARD_FRAMES - padding) as usize;
        let h0 = holes();
        let pulled = pull.pull();
        let top = fifo.len() as u64;
        player.fill_pulled(pulled, &mut out[..room], 1, |x| x as f32);
        let foot = fifo.len() as u64;
        let hole_now = holes() > h0;
        padding = CARD_FRAMES;
        noted.note(now, &sender);
        if receiving.load(Ordering::Acquire) == 0 {
            if began.elapsed() > Duration::from_secs(15) {
                return heard;
            }
            continue;
        }
        let elapsed = since.get_or_insert_with(Instant::now).elapsed();
        if elapsed < SETTLE {
            continue;
        }
        let counts = || {
            [
                stats.underruns(),
                stats.recentres(),
                stats.output_gaps(),
                stats.output_lost_frames(),
                holes(),
                media.rx.rings()[0].counters().late,
            ]
        };
        let (start, first, opened) =
            *judging.get_or_insert_with(|| (now, counts(), sender.open(noted.delayed())));
        // The window ends outside a gap: a gap's dry call drops before its
        // silence is counted here, and its settling drops the rest a few
        // calls later.
        let settled = !held && after_hold > u64::from(SETTLE_CALLS) + 2;
        if elapsed > SETTLE + JUDGED && settled {
            let last = counts();
            heard.underruns = last[0] - first[0];
            heard.recentres = last[1] - first[1];
            heard.output_gaps = last[2] - first[2];
            heard.output_lost = last[3] - first[3];
            heard.ring_holes = last[4] - first[4];
            heard.ring_late = last[5] - first[5];
            let (tx, pump) = sender.since(&opened);
            heard.sender = Sent { pump, tx };
            return heard;
        }
        if held {
            heard.holds += 1;
            after_hold = 0;
        }
        heard.card_silence += silence;
        let looks = (top + before_padding) + (foot + padding);
        if heard.holds == 0 && now - start < Duration::from_secs(1) {
            heard.before.0 += looks;
            heard.before.1 += 2;
        } else if heard.holds > 0 && after_hold >= 2 {
            heard.after.0 += looks;
            heard.after.1 += 2;
        }
        after_hold = after_hold.saturating_add(1);
        let judged = !hole_now && !hole_before;
        hole_before = hole_now;
        if !judged {
            heard.hole_calls += 1;
            continue;
        }
        for &x in &out[..room] {
            if (f64::from(x) - LEVEL).abs() < 1e-2 {
                heard.on += 1;
            } else {
                heard.off += 1;
            }
        }
    }
}

/// The whole run with the events woken as `wakes` says: what the card
/// heard, the pull's restarts, and the receiver's and sender's counts for
/// a failure's message.
fn run(wakes: Wakes) -> (Heard, u64, String) {
    let _one = ONE_DEVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().unwrap();
    restarts::listen();
    let place = Place::Ip(test_ipv4());
    let tx_config = Config::new("dlink-held-tx", dir.path().join("tx.toml"))
        .tx(["Out 1"])
        .sample_rate(RATE)
        .work_margin_ns(12_000_000);
    let rx_config = Config::new("dlink-held-rx", dir.path().join("rx.toml"))
        .rx(["In 1"])
        .sample_rate(RATE)
        .app_buffer(10_000_000, None)
        .ports(Ports {
            arc: 4441,
            cmc: 8801,
            flows: 4456,
        })
        .process_id(1)
        .subscribe(1, "Out 1", "dlink-held-tx");
    let buffer = rx_config.bridge_buffer().expect("the app buffer");
    let sender = Link::start_on(
        &tx_config,
        &place,
        Map {
            rx: vec![],
            tx: vec![0],
        },
        Constant,
        Clock::System,
    )
    .expect("the sending device started");
    let fifo = Arc::new(fifo_for(1, RATE));
    let (receiver, pull) = Link::start_pulling_on(
        &rx_config,
        &place,
        Map {
            rx: vec![0],
            tx: vec![],
        },
        Idle,
        Arc::clone(&fifo),
        Clock::System,
    )
    .expect("the receiving device started");
    let stats = Arc::new(Stats::default());
    let player = Player::new(
        Arc::clone(&fifo),
        RATE,
        RATE,
        buffer,
        buffer.chunk_ns(),
        Arc::clone(&stats),
    )
    .expect("the player");
    let receiving = Arc::new(AtomicU64::new(0));
    let pulled = pull.stats();
    let card = Card {
        pull,
        player,
        fifo: Arc::clone(&fifo),
        stats: Arc::clone(&stats),
        media: Arc::clone(receiver.media()),
        sender: crate::sender::Sender {
            tx: Arc::clone(&sender.media().tx),
            pump: sender.media().engine_stats().expect("the sender's pump"),
            rate: RATE,
        },
        receiving: Arc::clone(&receiving),
    };
    let card = std::thread::spawn(move || output(wakes, card));
    let end = Instant::now() + Duration::from_secs(15);
    while receiver.rx_channels()[0].status != Some(SubStatus::Receiving) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    receiving.store(1, Ordering::Release);
    let heard = card.join().expect("the output thread");
    let channel = receiver.rx_channels().remove(0);
    let ring = receiver.media().rx.rings()[0].counters();
    let sent = sender.media().engine().expect("the sender's pump");
    let counts = format!("{channel:?}, the ring {ring:?}, the sender's pump {sent:?}");
    assert_eq!((sender.ended(), receiver.ended()), (None, None));
    assert!(matches!(receiver.stop(), Stopped::Done));
    assert!(matches!(sender.stop(), Stopped::Done));
    assert!(
        heard.on + heard.off >= 7 * u64::from(RATE),
        "{heard:?}: {counts}"
    );
    (heard, pulled.restarts(), counts)
}

/// The asserts of a held run (module doc).
fn judge(case: &str, heard: &Heard, restarts: u64, counts: &str) {
    let (before, after) = (Heard::mean_ms(heard.before), Heard::mean_ms(heard.after));
    // The bridge's own counts, each asserted below: the verdict names the
    // sender only when all of them hold (dstack #436).
    let bridge_clean = (heard.underruns, heard.recentres, restarts) == (0, 0, 0)
        && heard.output_lost == heard.card_silence
        && (heard.output_gaps > 0 || heard.card_silence == 0)
        && (after - before).abs() < 2.0;
    let at = format!(
        "{case}: {heard:?}, restarts {restarts}, wait before {before:.2} ms, after {after:.2} ms; {} (callbacks not judged on the level {}); {counts}",
        verdict(heard, bridge_clean),
        heard.hole_calls
    );
    eprintln!("{at}");
    assert!(heard.holds >= 8, "only {} holds: {at}", heard.holds);
    assert_eq!(
        (heard.underruns, heard.recentres, restarts),
        (0, 0, 0),
        "bridge underruns, re-centres and pull restarts: {at}"
    );
    // The level, in every callback whose read met no ring hole (module
    // doc).
    assert_eq!(heard.off, 0, "samples off the level: {at}");
    // The frames due while the card played silence are the output's gap,
    // dropped and counted as its own: the card's silence, to the frame
    // (the settling snaps the estimate to the silence the buffer allows).
    let (lost, silence) = (heard.output_lost, heard.card_silence);
    assert_eq!(
        lost, silence,
        "the output's gaps dropped {lost} frames for the card's {silence} of silence: {at}"
    );
    assert!(
        heard.output_gaps > 0 || silence == 0,
        "no gap counted: {at}"
    );
    // No latency added: the frames due while the card played silence are
    // dropped, not played late (within a converter call and a pump block).
    assert!(
        (after - before).abs() < 2.0,
        "the wait after the holds {after:.2} ms against {before:.2} ms before: {at}"
    );
}

/// Each held event `hold` late, none other late.
fn held(hold: Duration) -> Wakes {
    Wakes {
        hold,
        late_after: Duration::ZERO,
        every: Duration::ZERO,
    }
}

#[test]
fn an_output_callback_held_46_ms_plays_on_from_the_queued_packets() {
    let (heard, restarts, counts) = run(held(Duration::from_millis(46)));
    judge("held 46 ms", &heard, restarts, &counts);
}

#[test]
fn an_output_callback_held_20_ms_plays_on_from_the_queued_packets() {
    let (heard, restarts, counts) = run(held(Duration::from_millis(20)));
    judge("held 20 ms", &heard, restarts, &counts);
}

/// The events after the hold wake 6 ms late too, past the settling (a
/// held PC's burst): that lateness must not be dropped as silence.
#[test]
fn an_output_callback_held_25_ms_then_6_ms_late_plays_on() {
    let (heard, restarts, counts) = run(Wakes {
        late_after: Duration::from_millis(6),
        ..held(Duration::from_millis(25))
    });
    judge("held 25 ms, then 6 ms late", &heard, restarts, &counts);
}

/// dstack #433 (v0.6.187 tag CI on windows-2025: 1 underrun, 1 re-centre,
/// 5088 frames dropped for 5568 of silence): every wake 1 ms late, and the
/// held one to 0.9 ms past the tick at T+30, less late past its tick than
/// the calls before were past theirs. Its dry call drops a period under
/// the card's silence (never over), and the FIFO holds that period until
/// the settling drops it: the controller must not re-centre on it. The
/// depth then crosses the excess bound only in the converter's latest
/// phase and with the hold as late as it can be under the floor (dev1
/// under `chrt -f 10`: 0.9 ms failed 4 of 4 with 3 underruns and 3
/// re-centres, 0.3 ms passed: 1807 frames at its first look against the
/// bound's 1824).
#[test]
fn an_output_callback_held_to_just_past_a_tick_on_a_late_timer_plays_on() {
    let (heard, restarts, counts) = run(Wakes {
        hold: Duration::from_micros(29_900),
        late_after: Duration::ZERO,
        every: Duration::from_millis(1),
    });
    judge(
        "every wake 1 ms late, held to 0.9 ms past a tick",
        &heard,
        restarts,
        &counts,
    );
}

/// The verdict's rule (dstack #436) on the numbers of its proofs (dev1
/// under `chrt -f 10`, 19.11.2026).
mod verdict_tests {
    use super::*;

    /// A window with `off` samples off and these sender counts.
    fn heard(off: u64, holes: u64, sender: Sent) -> Heard {
        Heard {
            off,
            ring_holes: holes + 56,
            ring_late: 56,
            sender,
            ..Heard::default()
        }
    }

    /// `restarts` pump restarts, `late` of them late, `silence` samples
    /// sent as silence, no TX restart.
    fn pump(restarts: u64, late: u64, silence: u64) -> Sent {
        Sent {
            pump: Pump {
                restarts,
                late,
                silence,
            },
            tx: restarts::fixture(0, &[]),
        }
    }

    /// `counted` TX restarts with lines of these oversleeps (µs), each 440
    /// samples late on a 432-sample bound: up to 872 samples unsent each.
    fn tx(counted: u64, oversleeps: &[Option<u64>]) -> Sent {
        Sent {
            tx: restarts::fixture(counted, oversleeps),
            ..Sent::default()
        }
    }

    #[test]
    fn samples_off_within_a_late_pump_restart_s_silence_are_the_sender_s() {
        // A 20 ms pump stall every 300 blocks: 10 late restarts, 10240
        // samples sent as silence, 10340 off.
        let w = heard(10_340, 0, pump(10, 10, 10_240));
        assert_eq!(who(&w, true), Who::Sender);
        assert!(
            verdict(&w, true).starts_with("verdict SENDER (the sender's pump or TX thread"),
            "{}",
            verdict(&w, true)
        );
        // The silence and GAP_EDGES a restart, and one sample past them.
        assert_eq!(
            who(&heard(10_560, 0, pump(10, 10, 10_240)), true),
            Who::Sender
        );
        assert_eq!(
            who(&heard(10_561, 0, pump(10, 10, 10_240)), true),
            Who::Ours
        );
        // A restart that was not late (a clock step back, a re-anchor).
        assert_eq!(who(&heard(10_340, 0, pump(10, 9, 10_240)), true), Who::Ours);
        // Silence with no pump restart (a block that ended past W), or a
        // late restart that sent none.
        assert_eq!(who(&heard(500, 0, pump(0, 0, 10_240)), true), Who::Ours);
        assert_eq!(who(&heard(500, 0, pump(1, 1, 0)), true), Who::Ours);
        assert_eq!(who(&heard(1, 0, pump(1, 1, 0)), true), Who::Ours);
        assert_eq!(who(&heard(1_056, 0, pump(1, 1, 1_024)), true), Who::Sender);
        assert_eq!(who(&heard(1_057, 0, pump(1, 1, 1_024)), true), Who::Ours);
        // The bridge's own counts not clean (an underrun, a dropped pull).
        assert_eq!(who(&w, false), Who::Ours);
        assert!(verdict(&w, false).starts_with("verdict OURS: 10340 samples off"));
    }

    #[test]
    fn holes_never_written_after_a_late_tx_sleep_are_the_sender_s() {
        // A 12 ms TX sleep every 700: two restarts, 1280 holes never
        // written, 56 off where a hole's tail played (here each line of
        // lag 440 on 432 may leave 872 unsent).
        let late = tx(2, &[Some(12_010), Some(12_011)]);
        assert_eq!(who(&heard(56, 1_280, late.clone()), true), Who::Sender);
        assert_eq!(who(&heard(0, 1_280, late.clone()), true), Who::Sender);
        // No hole never written: the lines leave no edge.
        assert_eq!(who(&heard(1, 0, late.clone()), true), Who::Ours);
        // The holes past the restarts' bound, the off past the holes and
        // GAP_EDGES a restart.
        assert_eq!(who(&heard(0, 1_745, late.clone()), true), Who::Ours);
        assert_eq!(who(&heard(1_344, 1_280, late.clone()), true), Who::Sender);
        assert_eq!(who(&heard(1_345, 1_280, late.clone()), true), Who::Ours);
        // A restart whose sleep was on time, a counted one with no line,
        // and holes with no restart at all.
        assert_eq!(who(&heard(0, 512, tx(1, &[Some(500)])), true), Who::Ours);
        assert_eq!(who(&heard(0, 512, tx(2, &[Some(12_010)])), true), Who::Ours);
        assert_eq!(who(&heard(0, 512, tx(0, &[])), true), Who::Ours);
        // A late pump restart beside a TX restart without a late sleep.
        let mut both = pump(1, 1, 1_024);
        both.tx = restarts::fixture(1, &[Some(500)]);
        assert_eq!(who(&heard(1_024, 0, both), true), Who::Ours);
        // dstack #426/#427: a restart bound under its own receiver's
        // latency is the stack's, named so.
        let mut under = late;
        under.tx.receiver_lag = vec![433];
        let w = heard(0, 1_280, under);
        assert_eq!(who(&w, true), Who::Ours);
        assert!(
            verdict(&w, true)
                .starts_with("verdict OURS (a TX restart bound under the receiver's latency): "),
            "{}",
            verdict(&w, true)
        );
    }

    #[test]
    fn a_loss_with_no_sender_evidence_is_ours() {
        // A dropped pull's samples off with a clean sender, and no loss.
        assert_eq!(who(&heard(1_548, 0, Sent::default()), true), Who::Ours);
        assert_eq!(who(&heard(0, 0, pump(3, 3, 3_072)), true), Who::Ours);
        // Late packets (holes written later) are no hole never written.
        let w = Heard {
            off: 0,
            ring_holes: 56,
            ring_late: 56,
            sender: tx(1, &[Some(12_010)]),
            ..Heard::default()
        };
        assert_eq!((w.never_written(), who(&w, true)), (0, Who::Ours));
    }
}
