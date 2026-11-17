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
//! the card's engine reads the client's buffer of two periods one period
//! at each of its ticks (every 10 ms, on its own clock; a tick that finds
//! the buffer short plays silence for what is missing), and each event,
//! signalled at a tick, runs the callback, which fills the room the buffer
//! has (cpal's `max_frames_in_buffer - padding`). A callback held past
//! what the buffer still holds leaves the card playing silence meanwhile
//! (the card's gap, not the bridge's). Some events are held 46 ms (the
//! field's worst) or 20 ms. The bridge must play every frame it is asked
//! for at the level (no underrun, no re-centre), and the latency after a
//! hold must be the latency before it: the frames due while the card
//! played silence are dropped once, counted as the output's own gap
//! ([`dlink::bridge::Stats::output_gaps`]), never played late.
//!
//! The level is judged only while the sender's audio reached the ring
//! whole: a sender thread not run long enough (a loaded box) leaves ring
//! holes, which play as samples off the level with no underrun; such a run
//! says `SENDER` and is judged on the bridge's own counts only (the
//! `rx_pull.rs` rule: holes with no underrun is the sender). A file of its
//! own (its process sets the normal profile), on loopback (dstack #366)
//! and its own system clock.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dlink::bridge::{Player, Stats, fifo_for};
use dlink::{Block, Clock, Config, Link, Map, Media, Place, Ports, Process, Stopped, SubStatus};

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
/// The client's buffer: two periods (WASAPI shared mode gives about that).
const CARD_FRAMES: u64 = 2 * PERIOD_FRAMES;
/// The settling (priming, the converter's filters), then the window
/// judged.
const SETTLE: Duration = Duration::from_secs(2);
const JUDGED: Duration = Duration::from_secs(8);
/// A held event about every this many events (0.7 s).
const HOLD_EVERY: u64 = 70;

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
    /// Samples the callback wrote at the level and off it.
    on: u64,
    off: u64,
    /// The bridge's underruns and re-centres in the judged window.
    underruns: u64,
    recentres: u64,
    /// The output's gaps the player counted, and the input frames it
    /// dropped for them.
    output_gaps: u64,
    output_lost: u64,
    /// The RX ring's holes in the judged window (the sender's audio that
    /// never came).
    ring_holes: u64,
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
}

/// The card and its event thread (module doc): returns what it heard.
fn output(
    hold: Duration,
    mut pull: dlink::bridge::RxPull,
    mut player: Player,
    fifo: Arc<dlink::bridge::Fifo>,
    stats: Arc<Stats>,
    media: Arc<Media>,
    receiving: Arc<AtomicU64>,
) -> Heard {
    let mut out = vec![0f32; CARD_FRAMES as usize];
    let mut heard = Heard::default();
    let began = Instant::now();
    // The engine's reads up to `now`: a period at each tick passed.
    let card = |now: Instant| (now - began).as_nanos() as u64 / PERIOD.as_nanos() as u64 * PERIOD_FRAMES;
    let holes = || media.rx.rings()[0].counters().holes;
    // Frames written into the client's buffer, on the card's count (a
    // tick that found it short moves it on).
    let mut written = 0u64;
    let mut next = began;
    let mut events = 0u64;
    let mut judging: Option<(Instant, [u64; 5])> = None;
    let mut since = None;
    // Callbacks since the last hold (the latency is judged two after it).
    let mut after_hold = u64::MAX;
    loop {
        next += PERIOD;
        events += 1;
        let held = judging.is_some() && events.is_multiple_of(HOLD_EVERY);
        let wake = if held { next + hold } else { next };
        if let Some(wait) = wake.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        let now = Instant::now();
        // The events stay on the engine's ticks: the ones missed are gone.
        while next + PERIOD <= now {
            next += PERIOD;
        }
        let played = card(now);
        let silence = played.saturating_sub(written);
        written = written.max(played);
        let padding = written - played;
        let room = (CARD_FRAMES - padding) as usize;
        let pulled = pull.pull();
        let top = fifo.len() as u64;
        player.fill_pulled(pulled, &mut out[..room], 1, |x| x as f32);
        let foot = fifo.len() as u64;
        written += room as u64;
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
            ]
        };
        let (start, first) = *judging.get_or_insert_with(|| (now, counts()));
        if elapsed > SETTLE + JUDGED {
            let last = counts();
            heard.underruns = last[0] - first[0];
            heard.recentres = last[1] - first[1];
            heard.output_gaps = last[2] - first[2];
            heard.output_lost = last[3] - first[3];
            heard.ring_holes = last[4] - first[4];
            return heard;
        }
        if held {
            heard.holds += 1;
            after_hold = 0;
        }
        heard.card_silence += silence;
        let looks = (top + padding) + (foot + padding + room as u64);
        if heard.holds == 0 && now - start < Duration::from_secs(1) {
            heard.before.0 += looks;
            heard.before.1 += 2;
        } else if heard.holds > 0 && after_hold >= 2 {
            heard.after.0 += looks;
            heard.after.1 += 2;
        }
        after_hold = after_hold.saturating_add(1);
        for &x in &out[..room] {
            if (f64::from(x) - LEVEL).abs() < 1e-2 {
                heard.on += 1;
            } else {
                heard.off += 1;
            }
        }
    }
}

/// The whole run with each held event `hold` late: what the card heard,
/// the pull's restarts, and the receiver's and sender's counts for a
/// failure's message.
fn run(hold: Duration) -> (Heard, u64, String) {
    let _one = ONE_DEVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().unwrap();
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
    let card = {
        let (fifo, stats, media, receiving) = (
            Arc::clone(&fifo),
            Arc::clone(&stats),
            Arc::clone(receiver.media()),
            Arc::clone(&receiving),
        );
        std::thread::spawn(move || output(hold, pull, player, fifo, stats, media, receiving))
    };
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
fn judge(hold: Duration, heard: &Heard, restarts: u64, counts: &str) {
    let (before, after) = (Heard::mean_ms(heard.before), Heard::mean_ms(heard.after));
    let verdict = if heard.ring_holes > 0 { "SENDER" } else { "OURS" };
    let at = format!(
        "held {hold:?}: {heard:?}, restarts {restarts}, wait before {before:.2} ms, after {after:.2} ms; verdict {verdict} (ring holes in the window {}); {counts}",
        heard.ring_holes
    );
    eprintln!("{at}");
    assert!(heard.holds >= 8, "only {} holds: {at}", heard.holds);
    assert_eq!(
        (heard.underruns, heard.recentres, restarts),
        (0, 0, 0),
        "bridge underruns, re-centres and pull restarts: {at}"
    );
    // Samples off the level are judged while the sender's audio reached
    // the ring whole (module doc).
    assert!(
        heard.off == 0 || heard.ring_holes > 0,
        "samples off the level with the ring whole: {at}"
    );
    // The frames due while the card played silence are the output's gap,
    // dropped once and counted as its own: exactly the card's silence, up
    // to one period in all (a hold that ends right at an engine tick, read
    // on both sides of it by the card's clock and the pull's).
    assert!(
        heard.output_lost.abs_diff(heard.card_silence) <= PERIOD_FRAMES,
        "the output's gaps dropped {} frames for the card's {} of silence: {at}",
        heard.output_lost,
        heard.card_silence
    );
    // No latency added: the frames due while the card played silence are
    // dropped, not played late (within a converter call and a pump block).
    assert!(
        (after - before).abs() < 2.0,
        "the wait after the holds {after:.2} ms against {before:.2} ms before: {at}"
    );
}

#[test]
fn an_output_callback_held_46_ms_plays_on_from_the_queued_packets() {
    let hold = Duration::from_millis(46);
    let (heard, restarts, counts) = run(hold);
    judge(hold, &heard, restarts, &counts);
}

#[test]
fn an_output_callback_held_20_ms_plays_on_from_the_queued_packets() {
    let hold = Duration::from_millis(20);
    let (heard, restarts, counts) = run(hold);
    judge(hold, &heard, restarts, &counts);
}
