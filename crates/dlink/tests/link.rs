// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #68: a dlink device in the app's process, on the box's routed
//! address and its own system clock, at dlink's defaults (10 ms, the
//! normal profile: no real-time priority, no pinning). A file of its own,
//! so its process sets the normal profile (one per process). Our device
//! hears itself (the tests' rule: our device only).

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dlink::{Block, Clock, Config, Link, Map, Place, Process, Stopped, SubState, SubStatus};
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
    // The pump runs at dlink's timing (48 kHz): B 32 + P 480 (10 ms) + W 240
    // (5 ms).
    assert_eq!((pump.playout, pump.offset), (480, 752), "{pump:?}");
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
    // The device's block is 32 frames at 96 kHz: the app gets 16 at 48 kHz.
    assert_eq!(frames.load(Ordering::Relaxed), 16);
    // The RX flow runs at 96 kHz: its 10 ms latency is 960 samples.
    assert!(status.contains("\"latency\":960"), "{status}");
    // The pump runs at the device's 96 kHz: P 960 (10 ms), W 480 (5 ms).
    assert_eq!((pump.playout, pump.offset), (960, 1_472), "{pump:?}");
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
