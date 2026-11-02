// SPDX-License-Identifier: GPL-3.0-or-later

//! dstack #68: a dlink device in the app's process, on the box's routed
//! address and its own system clock, at dlink's defaults (10 ms, the
//! normal profile: no real-time priority, no pinning). A file of its own,
//! so its process sets the normal profile (one per process). Our device
//! hears itself (the tests' rule: our device only).

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dlink::{Block, Clock, Config, Link, Map, Place, Process, Stopped};
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
