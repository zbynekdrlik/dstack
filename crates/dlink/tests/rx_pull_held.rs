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
//! the card plays on its own clock from a buffer of two periods, each
//! event (every 10 ms) the callback fills what the card has room for
//! (cpal's `max_frames_in_buffer - padding`), and a callback held past
//! what the card still holds leaves the card playing silence meanwhile
//! (the card's glitch, not the bridge's). Some events are held 46 ms (the
//! field's worst) or 20 ms. The bridge must play every frame it is asked
//! for at the level (no underrun, no re-centre), and the latency after a
//! hold must be the latency before it: the frames due while the card
//! played silence are dropped once, counted as the output's own gap
//! ([`dlink::bridge::Stats::output_gaps`]), never played late.
//! A file of its own (its process sets the normal profile), on loopback
//! (dstack #366) and its own system clock.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dlink::bridge::{Player, Stats, fifo_for};
use dlink::{Block, Clock, Config, Link, Map, Place, Ports, Process, Stopped, SubStatus};

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
/// The card's period: 10 ms, WASAPI shared mode's default.
const PERIOD: Duration = Duration::from_millis(10);
const PERIOD_FRAMES: u64 = 480;
/// The card's buffer: two periods (WASAPI shared mode gives about that).
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
    /// Held events, and the frames the card played as silence meanwhile
    /// (its own glitch: the callback was not there to fill it).
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
    receiving: Arc<AtomicU64>,
) -> Heard {
    let mut out = vec![0f32; CARD_FRAMES as usize];
    let mut heard = Heard::default();
    let began = Instant::now();
    let card = |now: Instant| (now - began).as_nanos() as u64 * u64::from(RATE) / 1_000_000_000;
    // Frames written into the card, on its clock (a glitch moves it on).
    let mut written = 0u64;
    let mut next = began;
    let mut events = 0u64;
    let mut judging: Option<(Instant, [u64; 4])> = None;
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
        // The event stays on the card's grid: the ones missed are gone.
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
            ]
        };
        let (start, first) = *judging.get_or_insert_with(|| (now, counts()));
        if elapsed > SETTLE + JUDGED {
            let last = counts();
            heard.underruns = last[0] - first[0];
            heard.recentres = last[1] - first[1];
            heard.output_gaps = last[2] - first[2];
            heard.output_lost = last[3] - first[3];
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

/// The whole run with each held event `hold` late; returns what the card
/// heard and the pull's restarts.
fn run(hold: Duration, rx_port: u16) -> (Heard, u64) {
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
            flows: rx_port,
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
        let (fifo, stats, receiving) = (
            Arc::clone(&fifo),
            Arc::clone(&stats),
            Arc::clone(&receiving),
        );
        std::thread::spawn(move || output(hold, pull, player, fifo, stats, receiving))
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
    assert_eq!((sender.ended(), receiver.ended()), (None, None));
    assert!(matches!(receiver.stop(), Stopped::Done));
    assert!(matches!(sender.stop(), Stopped::Done));
    assert!(
        heard.on + heard.off >= 7 * u64::from(RATE),
        "{heard:?}: {channel:?}, the ring {ring:?}, the sender's pump {sent:?}"
    );
    (heard, pulled.restarts())
}

/// The asserts of a held run (module doc).
fn judge(hold: Duration, heard: &Heard, restarts: u64) {
    let (before, after) = (Heard::mean_ms(heard.before), Heard::mean_ms(heard.after));
    eprintln!(
        "held {hold:?}: {heard:?}, restarts {restarts}, wait before {before:.2} ms, after {after:.2} ms"
    );
    assert!(heard.holds >= 8, "only {} holds: {heard:?}", heard.holds);
    assert_eq!(
        (heard.underruns, heard.off, restarts),
        (0, 0, 0),
        "bridge underruns, samples off the level and pull restarts with the callback held {hold:?}: {heard:?}"
    );
    // Nothing the controller must re-centre: the frames due while the card
    // played silence are the output's gap, dropped once and counted as its
    // own, within a converter call of what the card lost.
    assert_eq!(heard.recentres, 0, "re-centres: {heard:?}");
    let call = dlink::bridge::player::BLOCK as u64;
    assert!(
        heard.output_lost.abs_diff(heard.card_silence) <= heard.holds * call,
        "the output's gaps dropped {} frames for the card's {} of silence: {heard:?}",
        heard.output_lost,
        heard.card_silence
    );
    // No latency added: the frames due while the card played silence are
    // dropped, not played late (within a converter call and a pump block).
    assert!(
        (after - before).abs() < 2.0,
        "the wait after the holds {after:.2} ms against {before:.2} ms before: {heard:?}"
    );
}

#[test]
fn an_output_callback_held_46_ms_plays_on_from_the_queued_packets() {
    let hold = Duration::from_millis(46);
    let (heard, restarts) = run(hold, 4456);
    judge(hold, &heard, restarts);
}

#[test]
fn an_output_callback_held_20_ms_plays_on_from_the_queued_packets() {
    let hold = Duration::from_millis(20);
    let (heard, restarts) = run(hold, 4456);
    judge(hold, &heard, restarts);
}
