// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #68: a dlink device in the app's process, on the box's routed
//! address and its own system clock, at dlink's defaults (a 15 ms budget,
//! the normal profile: no real-time priority, no pinning). A file of its own,
//! so its process sets the normal profile (one per process). Our device
//! hears itself (the tests' rule: our device only).

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dlink::{Block, Clock, Config, Link, Map, Place, Ports, Process, Stopped, SubState, SubStatus};
use dstack::device::{DeviceState, Subscription};

/// The box's IPv4 address on its default route (connecting a UDP socket
/// picks the route and sends nothing).
fn routed_ipv4() -> std::net::Ipv4Addr {
    let probe = UdpSocket::bind("0.0.0.0:0").unwrap();
    probe.connect("192.0.2.1:9").unwrap();
    match probe.local_addr().expect("the routed address") {
        SocketAddr::V4(a) => *a.ip(),
        other => panic!("no routed IPv4 address on this box: {other}"),
    }
}

/// Held by each test that runs a device: they bind the same Dante ports on
/// the routed address, and the harness runs a file's tests at once.
static ONE_DEVICE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A constant on output 0, and the largest input 0 heard (as `f64` bits).
struct Echo {
    heard: Arc<AtomicU64>,
}

impl Process for Echo {
    fn process(&mut self, block: &mut Block<'_>) {
        let most = block.input(0).iter().fold(0.0f64, |m, &x| m.max(x.abs()));
        if most > f64::from_bits(self.heard.load(Ordering::Relaxed)) {
            self.heard.store(most.to_bits(), Ordering::Relaxed);
        }
        block.output(0).fill(0.5);
    }
}

#[test]
fn a_dlink_device_runs_the_app_s_processor_at_its_defaults() {
    let _one = ONE_DEVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("dlink.toml");
    let config = Config::new("dlink-test", &state)
        .rx(["In 1", "In 2"])
        .tx(["Out 1", "Out 2"]);
    // Our RX 1 subscribed to our own TX 1, as Dante Controller would keep it.
    let cfg = config.device().unwrap();
    let mut saved = DeviceState::from_config(&cfg);
    saved.subscriptions = vec![None; cfg.rx.len()];
    saved.subscriptions[0] = Some(Subscription {
        tx_channel: cfg.tx[0].clone(),
        tx_device: cfg.name.clone(),
    });
    saved.save(&state).unwrap();

    let heard = Arc::new(AtomicU64::new(0f64.to_bits()));
    let link = Link::start_on(
        &config,
        &Place::Ip(routed_ipv4()),
        Map {
            rx: vec![0],
            tx: vec![0],
        },
        Echo {
            heard: Arc::clone(&heard),
        },
        Clock::System,
    )
    .expect("the dlink device started");
    let end = Instant::now() + Duration::from_secs(10);
    while f64::from_bits(heard.load(Ordering::Relaxed)) < 0.49 && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    let pump = link.media().engine().expect("the pump attached");
    assert_eq!(link.ended(), None);
    let stopped = link.stop();
    let got = f64::from_bits(heard.load(Ordering::Relaxed));
    assert!(
        (got - 0.5).abs() < 1e-3,
        "our TX 1 came back on RX 1 as {got}"
    );
    assert!(pump.media && pump.blocks > 0, "{pump:?}");
    // The pump runs at dlink's timing (48 kHz), as the split says: B 128
    // (dstack #90) + P 432 (9 ms, what the 15 ms budget leaves after W;
    // dstack #68, #1) + W 288 (6 ms, 2/5 of the budget).
    let s = config.split().unwrap();
    let p = dstack::clock::latency_samples(s.latency_ns, 48_000);
    assert_eq!((p, s.margin), (432, 288));
    assert_eq!(
        (pump.playout, pump.offset),
        (p, 128 + p + s.margin),
        "{pump:?}"
    );
    assert!(matches!(stopped, Stopped::Done), "{stopped:?}");
    assert_eq!(
        dstack::clock::profile(),
        dstack::device::TimingProfile::Normal
    );
}

/// An app at 48 kHz on a 96 kHz network (dstack #20, #90): the device runs at
/// 96 kHz, the app gets blocks of half its frames, and our TX 1 comes back
/// on RX 1 through both conversions at the level the app sent (once the
/// filters settle: the step from silence rings, so the last sample counts,
/// not the peak).
#[test]
fn a_48_khz_app_runs_a_96_khz_device_through_the_conversion() {
    let _one = ONE_DEVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("dlink.toml");
    let config = Config::new("dlink-test", &state)
        .rx(["In 1"])
        .tx(["Out 1"])
        .sample_rate(48_000)
        .network_rate(96_000)
        .subscribe(1, "Out 1", "dlink-test");
    let heard = Arc::new(AtomicU64::new(0f64.to_bits()));
    let frames = Arc::new(AtomicU64::new(0));
    struct Sized {
        last: Arc<AtomicU64>,
        frames: Arc<AtomicU64>,
    }
    impl Process for Sized {
        fn process(&mut self, block: &mut Block<'_>) {
            self.frames.store(block.frames() as u64, Ordering::Relaxed);
            let last = block.input(0).last().copied().unwrap_or(0.0);
            self.last.store(last.to_bits(), Ordering::Relaxed);
            block.output(0).fill(0.5);
        }
    }
    let link = Link::start_on(
        &config,
        &Place::Ip(routed_ipv4()),
        Map {
            rx: vec![0],
            tx: vec![0],
        },
        Sized {
            last: Arc::clone(&heard),
            frames: Arc::clone(&frames),
        },
        Clock::System,
    )
    .expect("the dlink device started");
    let end = Instant::now() + Duration::from_secs(10);
    let settled = || (f64::from_bits(heard.load(Ordering::Relaxed)) - 0.5).abs() < 1e-3;
    while !settled() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    let pump = link.media().engine().expect("the pump attached");
    let status = link.status();
    assert!(status.starts_with("{\"clock\":{\"ready\":1,"), "{status}");
    // The app's own part (dstack #104): carried last, as one object.
    assert!(!status.contains("\"app\""), "{status}");
    link.set_app_status(|| r#"{"line": "48 kHz app", "state": "ok"}"#.to_string());
    // The hook runs on its own thread: the part comes after a read or two.
    let end = Instant::now() + Duration::from_secs(10);
    let mut with_app = link.status();
    while !with_app.contains("\"app\"") && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
        with_app = link.status();
    }
    assert!(
        with_app.ends_with(r#","app":{"line":"48 kHz app","state":"ok"}}"#),
        "{with_app}"
    );
    assert_eq!(link.ended(), None);
    let stopped = link.stop();
    let got = f64::from_bits(heard.load(Ordering::Relaxed));
    assert!(
        (got - 0.5).abs() < 1e-3,
        "our TX 1 came back on RX 1 as {got}"
    );
    // The device's block is 128 frames at 96 kHz (dstack #90): the app gets 64
    // at 48 kHz.
    assert_eq!(frames.load(Ordering::Relaxed), 64);
    // The RX flow runs at 96 kHz: the split's 8.36 ms latency (15 ms less
    // W's 6 ms share and the half-band's 0.64 ms TX delay, over a TX
    // packet's own age of 4.58 ms; dstack #68, #1) is 803 samples.
    let s = config.split().unwrap();
    let p = dstack::clock::latency_samples(s.latency_ns, 96_000);
    assert_eq!((p, s.margin), (803, 576));
    assert!(status.contains(&format!("\"latency\":{p}")), "{status}");
    // The pump runs at the device's 96 kHz: B 128 + P 803 + W 576 (6 ms,
    // W's whole share).
    assert_eq!(
        (pump.playout, pump.offset),
        (p, 128 + p + s.margin),
        "{pump:?}"
    );
    assert!(matches!(stopped, Stopped::Done), "{stopped:?}");
}

/// A subscription the app declares (intercom#5) holds from the first start,
/// with no state file: our RX 1 hears our own TX 1.
#[test]
fn a_declared_subscription_subscribes_a_fresh_device() {
    let _one = ONE_DEVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("dlink.toml");
    let config = Config::new("dlink-declared", &state)
        .rx(["In 1"])
        .tx(["Out 1"])
        .subscribe(1, "Out 1", "dlink-declared");
    let heard = Arc::new(AtomicU64::new(0f64.to_bits()));
    let link = Link::start_on(
        &config,
        &Place::Ip(routed_ipv4()),
        Map {
            rx: vec![0],
            tx: vec![0],
        },
        Echo {
            heard: Arc::clone(&heard),
        },
        Clock::System,
    )
    .expect("the dlink device started");
    let end = Instant::now() + Duration::from_secs(10);
    while f64::from_bits(heard.load(Ordering::Relaxed)) < 0.49 && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    // The app reads its channel's state (intercom#5): subscribed, receiving,
    // on flow 1, its counts growing.
    // The manager marks a granted flow receiving on its next round, once
    // it has heard packets.
    let end = Instant::now() + Duration::from_secs(5);
    while link.rx_channels()[0].status != Some(SubStatus::Receiving) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    let first = link.rx_channels();
    std::thread::sleep(Duration::from_millis(200));
    let later = link.rx_channels();
    assert_eq!(first.len(), 1);
    let ch = &later[0];
    assert_eq!(ch.rx, 1);
    assert_eq!(
        ch.subscription
            .as_ref()
            .map(|s| (s.tx_channel.as_str(), s.tx_device.as_str())),
        Some(("Out 1", "dlink-declared"))
    );
    assert_eq!(ch.status, Some(SubStatus::Receiving), "{ch:?}");
    assert_eq!(ch.status.map(SubStatus::state), Some(SubState::Connected));
    assert_eq!(ch.flow, Some(1));
    assert!(
        ch.packets > first[0].packets && ch.written > first[0].written,
        "{first:?} {later:?}"
    );
    assert_eq!(link.ended(), None);
    assert!(matches!(link.stop(), Stopped::Done));
    let got = f64::from_bits(heard.load(Ordering::Relaxed));
    assert!(
        (got - 0.5).abs() < 1e-3,
        "our TX 1 came back on RX 1 as {got}"
    );
}

/// dlink's default clock (#68): dsync's follower, else the service. On a
/// runner the follower cannot bind port 319 (no capability) and no dsync
/// service runs: the device still starts, serves and stops; it just has no
/// media time until a source is ready.
#[test]
fn a_dlink_device_on_the_linked_clock_starts_and_stops_without_a_source() {
    let _one = ONE_DEVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().unwrap();
    let config = Config::new("dlink-linked", dir.path().join("dlink.toml")).tx(["Out 1"]);
    let link = Link::start_on(
        &config,
        &Place::Ip(routed_ipv4()),
        Map {
            rx: vec![],
            tx: vec![0],
        },
        Echo {
            heard: Arc::new(AtomicU64::new(0)),
        },
        Clock::Linked,
    )
    .expect("the dlink device started on the linked clock");
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(link.ended(), None);
    assert!(matches!(link.stop(), Stopped::Done));
}

/// The app's step out, and where it comes back in: once `go` is set, from
/// its next block on (app frame `sent`) it sends 0.5 on output 0; the first
/// input 0 frame at 0.25 or more after that is `back` (through a converter,
/// the step's half way is its delay). Silence before.
struct Loop {
    go: Arc<AtomicBool>,
    sent: Arc<AtomicU64>,
    back: Arc<AtomicU64>,
}

/// Not yet (`sent`, `back`).
const NONE: u64 = u64::MAX;

impl Process for Loop {
    fn process(&mut self, block: &mut Block<'_>) {
        let Some(start) = block.start() else {
            block.zero_outputs();
            return;
        };
        let sent = self.sent.load(Ordering::Relaxed);
        if sent != NONE
            && self.back.load(Ordering::Relaxed) == NONE
            && let Some(i) = block.input(0).iter().position(|&x| x >= 0.25)
        {
            self.back.store(start + i as u64, Ordering::Relaxed);
        }
        if sent == NONE && self.go.load(Ordering::Relaxed) {
            self.sent.store(start, Ordering::Relaxed);
        }
        let level = if self.sent.load(Ordering::Relaxed) == NONE {
            0.0
        } else {
            0.5
        };
        block.output(0).fill(level);
    }
}

/// Runs `config` (RX 1 subscribed to its own TX 1); once the flow carries
/// our silence, the app sends a step. The loop in ns at the app's rate.
fn measured_loop(config: &Config, app_rate: u32) -> u64 {
    let go = Arc::new(AtomicBool::new(false));
    let sent = Arc::new(AtomicU64::new(NONE));
    let back = Arc::new(AtomicU64::new(NONE));
    let link = Link::start_on(
        config,
        &Place::Ip(routed_ipv4()),
        Map {
            rx: vec![0],
            tx: vec![0],
        },
        Loop {
            go: Arc::clone(&go),
            sent: Arc::clone(&sent),
            back: Arc::clone(&back),
        },
        Clock::System,
    )
    .expect("the dlink device started");
    let end = Instant::now() + Duration::from_secs(10);
    while link.rx_channels()[0].status != Some(SubStatus::Receiving) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    // The flow writes our silence into RX 1 (and the filters settle).
    std::thread::sleep(Duration::from_millis(300));
    go.store(true, Ordering::Relaxed);
    let end = Instant::now() + Duration::from_secs(5);
    while back.load(Ordering::Relaxed) == NONE && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(link.ended(), None);
    let channel = link.rx_channels().remove(0);
    assert!(matches!(link.stop(), Stopped::Done));
    let (s, b) = (sent.load(Ordering::Relaxed), back.load(Ordering::Relaxed));
    assert!(
        s != NONE && b != NONE,
        "the step never came back: {channel:?}"
    );
    (b - s) * 1_000_000_000 / u64::from(app_rate)
}

/// dstack #68: the split's numbers are the real path. Our TX 1 into our own
/// RX 1 is the TX path (W + P) and the RX path (P + B) less the one P they
/// share (the receiver's playout is our RX's), plus the converter's delays
/// when it runs. The step the app sends comes back within a few frames of
/// that (the design asks one block), measured on the device's own stamps
/// through the real flow.
#[test]
fn the_loop_through_our_own_flow_takes_what_the_split_says() {
    let _one = ONE_DEVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().unwrap();
    for (name, app, network) in [
        ("dlink-loop", 48_000u32, 48_000u32),
        ("dlink-loop-96", 48_000, 96_000),
    ] {
        let config = Config::new(name, dir.path().join(format!("{name}.toml")))
            .rx(["In 1"])
            .tx(["Out 1"])
            .sample_rate(app)
            .network_rate(network)
            .subscribe(1, "Out 1", name);
        let split = config.split().unwrap();
        let want = split.tx_path_ns + split.rx_path_ns - split.latency_ns;
        // Each path is within the 15 ms budget.
        assert!(split.tx_path_ns <= dlink::BUDGET_NS && split.rx_path_ns <= dlink::BUDGET_NS);
        let got = measured_loop(&config, app);
        // The design allows one block; the stamps are exact, so a few app
        // frames (the converter's half-way crossing) is the bound, and a
        // margin or a delay off by a block fails.
        let frames_ns = 8 * 1_000_000_000 / u64::from(app);
        assert!(
            got.abs_diff(want) <= frames_ns,
            "{name}: the loop took {got} ns, the split says {want} ns ({split:?})"
        );
    }
}

/// How long [`Napping`] sleeps, and how often (every `NAP_EVERY`th block):
/// a pump that late, on a loaded PC (intercom's OBS box, a busy core).
const NAP: Duration = Duration::from_millis(4);
const NAP_EVERY: u64 = 40;

/// Writes 0.5 on output 0, and counts input 0's samples while `count` is
/// set: those at 0.5 (`good`) and those off it (`bad`, a gap ringing
/// through the converter). Sleeps [`NAP`] every [`NAP_EVERY`]th block, so
/// the pump runs that late (less one block) now and then.
struct Napping {
    calls: u64,
    count: Arc<AtomicBool>,
    good: Arc<AtomicU64>,
    bad: Arc<AtomicU64>,
}

impl Napping {
    fn new(count: &Arc<AtomicBool>, good: &Arc<AtomicU64>, bad: &Arc<AtomicU64>) -> Self {
        Napping {
            calls: 0,
            count: Arc::clone(count),
            good: Arc::clone(good),
            bad: Arc::clone(bad),
        }
    }
}

impl Process for Napping {
    fn process(&mut self, block: &mut Block<'_>) {
        self.calls += 1;
        if self.calls.is_multiple_of(NAP_EVERY) {
            std::thread::sleep(NAP);
        }
        if block.inputs() > 0 && self.count.load(Ordering::Relaxed) {
            let (mut good, mut bad) = (0, 0);
            for &x in block.input(0) {
                if (x - 0.5).abs() < 1e-3 {
                    good += 1;
                } else {
                    bad += 1;
                }
            }
            self.good.fetch_add(good, Ordering::Relaxed);
            self.bad.fetch_add(bad, Ordering::Relaxed);
        }
        if block.outputs() > 0 {
            block.output(0).fill(0.5);
        }
    }
}

/// intercom's report against v0.6.22 (dstack #1): a native 96 kHz dlink TX
/// heard by a dlink RX on the 96 kHz network with a 48 kHz app had gaps
/// on a loaded PC, where v0.6.21 had none. The pump margin W, the
/// lateness a pump may have before it starts again, had dropped to 2 ms
/// (half the latency, 5 ms, before), and stayed there at intercom's 20 ms
/// budget. Both devices' pumps run up to [`NAP`] less a block late now and
/// then: within their share of the budget, the tone arrives whole (no pump
/// starts again, no sample off the level), on both the sending and the
/// receiving side. The test runs intercom's 20 ms budget (W 8 ms; 2 ms
/// before the fix): at 10 ms W's 4 ms leaves a nap 1.3 ms of slack, which
/// wake jitter on a loaded shared box can take (tests.md: real-time
/// lateness is the hardware gate's).
#[test]
fn a_pump_late_within_its_share_of_the_budget_keeps_the_tone_whole() {
    let _one = ONE_DEVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().unwrap();
    let place = Place::Ip(routed_ipv4());
    let count = Arc::new(AtomicBool::new(false));
    let good = Arc::new(AtomicU64::new(0));
    let bad = Arc::new(AtomicU64::new(0));
    let budget = 20_000_000;
    let tx_config = Config::new("dlink-late-tx", dir.path().join("tx.toml"))
        .tx(["Out 1"])
        .sample_rate(96_000)
        .budget_ns(budget);
    let rx_config = Config::new("dlink-late-rx", dir.path().join("rx.toml"))
        .rx(["In 1"])
        .sample_rate(48_000)
        .network_rate(96_000)
        .budget_ns(budget)
        .ports(Ports {
            arc: 4441,
            cmc: 8801,
            flows: 4456,
        })
        .process_id(1)
        .subscribe(1, "Out 1", "dlink-late-tx");
    let sender = Link::start_on(
        &tx_config,
        &place,
        Map {
            rx: vec![],
            tx: vec![0],
        },
        Napping::new(&count, &good, &bad),
        Clock::System,
    )
    .expect("the sending device started");
    let receiver = Link::start_on(
        &rx_config,
        &place,
        Map {
            rx: vec![0],
            tx: vec![],
        },
        Napping::new(&count, &good, &bad),
        Clock::System,
    )
    .expect("the receiving device started");
    let end = Instant::now() + Duration::from_secs(15);
    while receiver.rx_channels()[0].status != Some(SubStatus::Receiving) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    // The flow carries the tone and the converter's filters settle.
    std::thread::sleep(Duration::from_millis(500));
    let before = (
        sender
            .media()
            .engine()
            .expect("the sender's pump")
            .rebootstraps,
        receiver
            .media()
            .engine()
            .expect("the receiver's pump")
            .rebootstraps,
    );
    count.store(true, Ordering::Relaxed);
    // Three seconds of the 48 kHz app's samples: about 50 naps a side.
    let end = Instant::now() + Duration::from_secs(10);
    while good.load(Ordering::Relaxed) + bad.load(Ordering::Relaxed) < 3 * 48_000
        && Instant::now() < end
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    count.store(false, Ordering::Relaxed);
    let after = (
        sender
            .media()
            .engine()
            .expect("the sender's pump")
            .rebootstraps,
        receiver
            .media()
            .engine()
            .expect("the receiver's pump")
            .rebootstraps,
    );
    let pumps = (
        sender.media().engine().expect("the sender's pump"),
        receiver.media().engine().expect("the receiver's pump"),
    );
    let channel = receiver.rx_channels().remove(0);
    assert_eq!((sender.ended(), receiver.ended()), (None, None));
    assert!(matches!(receiver.stop(), Stopped::Done));
    assert!(matches!(sender.stop(), Stopped::Done));
    let (good, bad) = (good.load(Ordering::Relaxed), bad.load(Ordering::Relaxed));
    assert!(
        good + bad >= 3 * 48_000,
        "{good} on, {bad} off: {channel:?}"
    );
    assert_eq!(
        (after.0 - before.0, after.1 - before.1),
        (0, 0),
        "pump restarts (sender, receiver) while late within the margin"
    );
    assert_eq!(bad, 0, "{bad} samples off the tone, {good} on it");
    // The split each device runs (dstack #1), as its pump's playout point
    // and margin (offset = B + P + W): the TX-only sender at 12 ms (1152
    // samples at 96 kHz: the budget less W's 8 ms share, 768), the RX-only
    // receiver at 18.04 ms (1732: the budget less its block and the
    // half-band's 0.63 ms RX delay), with the whole 8 ms share as its W.
    let (tx, rx) = pumps;
    assert_eq!((tx.playout, tx.offset), (1152, 128 + 1152 + 768), "{tx:?}");
    assert_eq!((rx.playout, rx.offset), (1732, 128 + 1732 + 768), "{rx:?}");
}
