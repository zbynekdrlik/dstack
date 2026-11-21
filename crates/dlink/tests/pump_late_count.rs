// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #477: a receiver whose pump wakes late within its margin W counts
//! no packet late that arrived within the latency. presenter-listen
//! (21.11.2026, P 5 ms, block 1.33 ms) reported 40 late packets in its
//! heartbeat's 0x8004 while their arrivals were 0.26-0.34 ms (0x8003), in
//! the seconds its pump woke 4-6 ms late (`behind_max_us` 5604, 5947,
//! 4291); the same at P 12.35 ms with one wake 14.1 ms late (19.11, x21)
//! and one 22.5 ms (18.11, x57). The pump reads by its schedule: a late
//! wake first reads the block due that long ago, so the packets in its
//! sockets are all in time, though now is past P + B of them.
//!
//! A dlink TX device sends a constant; a dlink RX device at P 5 ms (the
//! field's) has a processor that sleeps [`NAP`] every [`NAP_EVERY`] blocks,
//! within W (set 20 ms). Each second of the judged window reads the counts
//! the heartbeat sends (0x8004 is the slot's `late`, 0x8003 its last
//! second's lateness at arrival): no packet late, none written behind the
//! reader, the tone whole but for packets the sender never sent (its TX
//! thread may skip some on a loaded box). A file of its own (one device
//! pair), on loopback (dstack #366).

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dlink::{Block, Clock, Config, Link, Map, Place, Ports, Process, Stopped, SubStatus};

/// Loopback (dstack #366): nothing this test sends leaves the box.
fn test_ipv4() -> std::net::Ipv4Addr {
    std::net::Ipv4Addr::LOCALHOST
}

/// The app's (and the network's) rate.
const RATE: u32 = 48_000;
/// The level the sender plays.
const LEVEL: f64 = 0.5;
/// P = (a) 1 ms + (b) 4 ms: the field's 5 ms.
const RECEIVE_BUFFER_NS: u64 = 4_000_000;
/// W: a nap stays well inside it (no restart, no block lost).
const MARGIN_NS: u64 = 20_000_000;
/// The receiver's pump sleeps this long in a block now and then: its next
/// wake is about that late, past P (5 ms) and P + B (7.67 ms), where the
/// old rule (now past P + B of a packet) counted the packets that came
/// since the drain before as late.
const NAP: Duration = Duration::from_millis(9);
/// Blocks between naps: about 1.4 naps a second at 128-frame blocks.
const NAP_EVERY: u64 = 256;
/// The counted seconds from the flow's start (`Receiving`), and the second
/// after which the tone is judged (the flow settled).
const JUDGED_SECONDS: u32 = 14;
const SETTLE_SECONDS: u32 = 2;

/// [`LEVEL`] on output 0.
struct Constant;

impl Process for Constant {
    fn process(&mut self, block: &mut Block<'_>) {
        block.output(0).fill(LEVEL);
    }
}

/// Sleeps [`NAP`] every [`NAP_EVERY`] blocks; counts input 0's samples on
/// and off the level once `judge` is set.
struct Napping {
    calls: u64,
    naps: Arc<AtomicU64>,
    judge: Arc<AtomicU64>,
    on: Arc<AtomicU64>,
    off: Arc<AtomicU64>,
}

impl Process for Napping {
    fn process(&mut self, block: &mut Block<'_>) {
        self.calls += 1;
        if self.calls.is_multiple_of(NAP_EVERY) {
            self.naps.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(NAP);
        }
        if self.judge.load(Ordering::Relaxed) != 0 {
            let on = block
                .input(0)
                .iter()
                .filter(|&&x| (x - LEVEL).abs() < 1e-3)
                .count() as u64;
            self.on.fetch_add(on, Ordering::Relaxed);
            self.off
                .fetch_add(block.frames() as u64 - on, Ordering::Relaxed);
        }
    }
}

#[test]
fn a_pump_late_within_its_margin_counts_no_packet_late_that_came_in_time() {
    let dir = tempfile::tempdir().unwrap();
    let place = Place::Ip(test_ipv4());
    let tx_config = Config::new("dlink-latecount-tx", dir.path().join("tx.toml"))
        .tx(["Out 1"])
        .sample_rate(RATE)
        // The flow's latency is the larger of the transmitter's advertised
        // P and ours (dstack #352): the sender advertises 5 ms too.
        .receive_buffer_ns(RECEIVE_BUFFER_NS);
    let rx_config = Config::new("dlink-latecount-rx", dir.path().join("rx.toml"))
        .rx(["In 1"])
        .sample_rate(RATE)
        .receive_buffer_ns(RECEIVE_BUFFER_NS)
        .work_margin_ns(MARGIN_NS)
        .ports(Ports {
            arc: 4441,
            cmc: 8801,
            flows: 4456,
        })
        .process_id(1)
        .subscribe(1, "Out 1", "dlink-latecount-tx");
    let split = rx_config.split().expect("the receiver's split");
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
    let naps = Arc::new(AtomicU64::new(0));
    let judge = Arc::new(AtomicU64::new(0));
    let on = Arc::new(AtomicU64::new(0));
    let off = Arc::new(AtomicU64::new(0));
    let receiver = Link::start_on(
        &rx_config,
        &place,
        Map {
            rx: vec![0],
            tx: vec![],
        },
        Napping {
            calls: 0,
            naps: Arc::clone(&naps),
            judge: Arc::clone(&judge),
            on: Arc::clone(&on),
            off: Arc::clone(&off),
        },
        Clock::System,
    )
    .expect("the receiving device started");
    let end = Instant::now() + Duration::from_secs(15);
    while receiver.rx_channels()[0].status != Some(SubStatus::Receiving) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    // From the flow's first second (the field's late came in a flow's
    // first half minute): each second, the slot's counts as the heartbeat
    // sends them (0x8004 the lifetime `late`, 0x8003 the last second's
    // largest lateness at arrival, samples) and the pump's wakes. The tone
    // is judged once the flow settled.
    let rx = Arc::clone(&receiver.media().rx);
    let pump = || receiver.media().engine().expect("the receiver's pump");
    // The ring's holes never written (`holes - late`): the sender's, whose
    // TX thread may skip packets on a loaded box (tests.md); never ours.
    let unwritten = || {
        let ring = rx.rings()[0].counters();
        ring.holes - ring.late
    };
    let first = (rx.lane(0), pump(), naps.load(Ordering::Relaxed));
    let mut seconds = String::new();
    let mut last = first.0;
    let mut unwritten_at_judge = 0;
    for s in 1..=JUDGED_SECONDS {
        std::thread::sleep(Duration::from_secs(1));
        if s == SETTLE_SECONDS {
            unwritten_at_judge = unwritten();
            judge.store(1, Ordering::Relaxed);
        }
        let c = rx.lane(0);
        let p = pump();
        writeln!(
            seconds,
            "{s:>2} s: packets +{} late +{} late_at_read +{} 0x8004 {} 0x8003 {} samples, naps {}, pump behind max {} us, rebootstraps {}",
            c.packets - last.packets,
            c.late - last.late,
            c.late_at_read - last.late_at_read,
            c.late,
            rx.last_lateness(0),
            naps.load(Ordering::Relaxed),
            p.wake_behind_max_us,
            p.rebootstraps,
        )
        .unwrap();
        last = c;
    }
    judge.store(0, Ordering::Relaxed);
    let sender_holes = unwritten() - unwritten_at_judge;
    let (c, p) = (rx.lane(0), pump());
    let channel = receiver.rx_channels().remove(0);
    assert_eq!((sender.ended(), receiver.ended()), (None, None));
    assert!(matches!(receiver.stop(), Stopped::Done));
    assert!(matches!(sender.stop(), Stopped::Done));
    let (_, first_p, first_naps) = first;
    let naps = naps.load(Ordering::Relaxed) - first_naps;
    let (on, off) = (on.load(Ordering::Relaxed), off.load(Ordering::Relaxed));
    let window = format!(
        "P {} ns, W {} samples; per second:\n{seconds}the slot {c:?}, the pump {p:?}, {channel:?}",
        split.latency_ns, split.margin
    );
    // The window held the late wakes the field had, and nothing worse: no
    // pump restart (a wake past W is a runner stall, not this case).
    assert!(naps >= 10, "only {naps} naps: {window}");
    assert_eq!(
        p.rebootstraps - first_p.rebootstraps,
        0,
        "the receiver's pump restarted (a wake past W): {window}"
    );
    let p_us = split.latency_ns / 1000;
    assert!(
        p.wake_behind_max_us > p_us,
        "no wake later than P ({p_us} us): {window}"
    );
    assert!(
        on + off >= u64::from(JUDGED_SECONDS - SETTLE_SECONDS - 1) * u64::from(RATE),
        "{on} on, {off} off: {window}"
    );
    // Every packet came in time and was read, from the device's start (the
    // heartbeat's 0x8004 counts since then): none late, none behind the
    // reader.
    assert_eq!(
        (c.late, c.late_at_read),
        (0, 0),
        "late packets, packets written behind the reader: {window}"
    );
    // The tone whole but for what the sender never sent (its holes, a
    // block's slack each side of the judged window's edges).
    let slack = 2 * 128;
    assert!(
        off <= sender_holes + slack,
        "{off} samples off the level, the sender's holes {sender_holes}: {window}"
    );
}
