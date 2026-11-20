// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #20: a device inside an app's own process ([`Embedded`]), on the
//! loopback (dstack #366) and its own system clock. A file of its own: its
//! process must never have had a signal handler, so that it can check
//! that [`Embedded`] leaves none (the whole-device tests use
//! `Signals::Report`, and tokio keeps a handler for the process's life).
//! Cargo runs test files one after another, so it never meets their ports.

use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dstack::daemon::Place;
use dstack::device::{DeviceConfig, DeviceState, SavedMulticast, Subscription};
use dstack::embed::Embedded;
use dstack::host::{ClockSource, Stopped};
use dstack::media::rx::RxDestinations;
use dstack::stream::Map;
use dstack_audio_io::{Block, Process};

const TEST_DEVICE: &str = include_str!("../../../config/test-device.toml");

/// Held by each test that runs the device: they bind the same Dante ports
/// on loopback, and the test harness runs a file's tests at once.
static ONE_DEVICE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The test device's config, 10 ms latency (a test proves the path, not
/// real-time timing), its state in `dir` with RX 1 subscribed to our own
/// TX 1.
fn config_in(dir: &std::path::Path) -> DeviceConfig {
    let state = dir.join("state.toml");
    let text: String = TEST_DEVICE
        .lines()
        .filter(|l| !l.starts_with("state_file"))
        .map(|l| {
            if l.starts_with("latency_ns") {
                "latency_ns = 10000000"
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let cfg =
        DeviceConfig::from_toml(&format!("{text}\nstate_file = {:?}\n", state.display())).unwrap();
    let mut saved = DeviceState::from_config(&cfg);
    saved.subscriptions = vec![None; cfg.rx.len()];
    saved.subscriptions[0] = Some(Subscription {
        tx_channel: cfg.tx[0].clone(),
        tx_device: cfg.name.clone(),
    });
    saved.save(&state).unwrap();
    cfg
}

/// The test device's address: loopback, so none of its mDNS, info or
/// heartbeat multicasts leaves the box (dstack #366: this workspace is a
/// test run, which refuses a device on a site address).
fn test_ipv4() -> Ipv4Addr {
    Ipv4Addr::LOCALHOST
}

/// The app's processor: a constant on output 0, and the largest input 0
/// it saw (as `f64` bits).
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

/// SIGTERM's disposition in this process: the default action unless a
/// handler was installed.
#[cfg(target_os = "linux")]
fn sigterm_is_default() -> bool {
    // SAFETY: a query only (no new action), into a zeroed sigaction.
    unsafe {
        let mut old: libc::sigaction = std::mem::zeroed();
        libc::sigaction(libc::SIGTERM, std::ptr::null(), &mut old) == 0
            && old.sa_sigaction == libc::SIG_DFL
    }
}

/// The app's processor sends on our TX 1 and hears it on our RX 1 (the
/// tests' rule: our device only); stopped, started again in the same
/// process, and no signal handler left behind.
#[test]
fn an_embedded_device_runs_the_app_s_processor_and_starts_again() {
    #[cfg(target_os = "linux")]
    assert!(
        sigterm_is_default(),
        "the test process starts with a handler"
    );
    let _one = ONE_DEVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().unwrap();
    let ip = test_ipv4();
    let cfg = config_in(dir.path());
    for round in 0..2 {
        let heard = Arc::new(AtomicU64::new(0f64.to_bits()));
        let device = Embedded::start_with(
            cfg.clone(),
            &Place::Ip(ip),
            Map {
                rx: vec![0],
                tx: vec![0],
            },
            Echo {
                heard: Arc::clone(&heard),
            },
            ClockSource::System,
        )
        .expect("the embedded device started");
        let end = Instant::now() + Duration::from_secs(10);
        while f64::from_bits(heard.load(Ordering::Relaxed)) < 0.49 && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
        }
        let pump = device.media().engine().expect("the pump attached");
        assert_eq!(device.ended(), None, "round {round}");
        let stopped = device.stop(Duration::from_secs(5));
        let got = f64::from_bits(heard.load(Ordering::Relaxed));
        assert!(
            (got - 0.5).abs() < 1e-3,
            "round {round}: our TX 1 came back on RX 1 as {got}"
        );
        assert!(pump.media && pump.blocks > 0, "round {round}: {pump:?}");
        assert!(
            matches!(stopped, Stopped::Done),
            "round {round}: {stopped:?}"
        );
        #[cfg(target_os = "linux")]
        assert!(
            sigterm_is_default(),
            "round {round}: a SIGTERM handler is left"
        );
    }
}

/// A map naming an RX channel the device does not have is refused before
/// anything is bound.
#[test]
fn an_app_map_past_the_device_s_channels_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    let n = cfg.rx.len();
    let got = Embedded::start_with(
        cfg,
        &Place::Ip(Ipv4Addr::LOCALHOST),
        Map {
            rx: vec![n],
            tx: vec![],
        },
        Echo {
            heard: Arc::new(AtomicU64::new(0)),
        },
        ClockSource::System,
    );
    match got {
        Err(dstack::host::HostError::Setup(why)) => {
            assert!(why.contains(&format!("RX channel {}", n + 1)), "{why}")
        }
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("started with an RX channel past the device's"),
    }
}

/// A latency Dante Controller set (kept in the state file) holds after a
/// restart for the app's pump too: it plays at the device's latency, not
/// the config's, or it reads the RX rings off their playout point.
#[test]
fn an_embedded_pump_plays_at_the_latency_dante_controller_set() {
    let _one = ONE_DEVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().unwrap();
    let ip = test_ipv4();
    let cfg = config_in(dir.path());
    let state = dir.path().join("state.toml");
    let mut saved = DeviceState::load_or_default(&cfg).unwrap();
    saved.set_latency(20_000_000, cfg.latency_ns);
    saved.save(&state).unwrap();
    let device = Embedded::start_with(
        cfg.clone(),
        &Place::Ip(ip),
        Map {
            rx: vec![0],
            tx: vec![0],
        },
        Echo {
            heard: Arc::new(AtomicU64::new(0)),
        },
        ClockSource::System,
    )
    .expect("the embedded device started");
    let pump = device.media().engine().expect("the pump attached");
    let stopped = device.stop(Duration::from_secs(5));
    // 20 ms at the test device's rate.
    let want = 20_000_000 * u64::from(cfg.sample_rate) / 1_000_000_000;
    assert_eq!(pump.playout, want, "{pump:?}");
    assert!(matches!(stopped, Stopped::Done), "{stopped:?}");
}

/// The manager's flows of a running device: each one's port and group.
type Flows = Vec<(u16, Option<SocketAddrV4>)>;

/// Starts the device of `cfg` (RX 1 and TX 1 to the app).
fn start(cfg: &DeviceConfig) -> Embedded {
    Embedded::start_with(
        cfg.clone(),
        &Place::Ip(test_ipv4()),
        Map {
            rx: vec![0],
            tx: vec![0],
        },
        Echo {
            heard: Arc::new(AtomicU64::new(0)),
        },
        ClockSource::System,
    )
    .expect("the embedded device started")
}

/// Watches `device` until its RX flows' destinations are `want` of its
/// flows, bounded (a transmitter's multicast flow is known once the
/// directory read its flow list), and returns every set it saw (each
/// generation once, in order) and the manager's flows then.
fn destinations_of(
    device: &Embedded,
    want: impl Fn(&Flows) -> Vec<SocketAddrV4>,
) -> (Vec<RxDestinations>, Flows) {
    let flows = || -> Flows {
        let m = device.media().manager.lock().unwrap();
        (0..dstack::media::rx::MAX_FLOWS)
            .filter_map(|s| m.flow(s).map(|f| (f.port, f.group)))
            .collect()
    };
    let mut seen: Vec<RxDestinations> = Vec::new();
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        let now = device.media().rx_destinations();
        if seen.last().is_none_or(|s| s.generation != now.generation) {
            seen.push(now.clone());
        }
        let f = flows();
        if (!f.is_empty() && now.pairs == want(&f)) || Instant::now() > end {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    (seen, flows())
}

/// dstack #464: a running device gives the (address, port) pairs its RX
/// flows arrive on. RX 1 on our own TX 1 by unicast: our address and the
/// flow slot's port. RX 1 of a second device on the first's TX 1, which a
/// (saved) multicast flow of the first carries: that flow's group and
/// port, never a slot's; each set seen with a later generation.
#[test]
fn an_embedded_device_gives_its_rx_flows_destinations() {
    let _one = ONE_DEVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let ip = test_ipv4();
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());

    let device = start(&cfg);
    let unicast = |f: &Flows| {
        f.iter()
            .map(|&(port, _)| SocketAddrV4::new(ip, port))
            .collect()
    };
    let (seen, flows) = destinations_of(&device, unicast);
    let stopped = device.stop(Duration::from_secs(5));
    let [(port, None)] = flows[..] else {
        panic!("one unicast flow: {flows:?} {seen:?}");
    };
    let last = seen.last().unwrap();
    assert_eq!(last.pairs, [SocketAddrV4::new(ip, port)], "{seen:?}");
    // A request retried into another slot is a set (and a generation) more.
    assert!(last.generation >= 1, "{seen:?}");
    assert!(matches!(stopped, Stopped::Done), "{stopped:?}");

    // The transmitter: our multicast flow of TX 1, no subscription.
    let mut saved = DeviceState::load_or_default(&cfg).unwrap();
    saved.subscriptions = vec![None; cfg.rx.len()];
    saved.multicast = vec![SavedMulticast {
        slot: 0,
        channels: vec![1],
    }];
    saved.save(&dir.path().join("state.toml")).unwrap();
    // The receiver: another name, its own ports and process id, RX 1 on
    // the transmitter's TX 1.
    let dir_b = tempfile::tempdir().unwrap();
    let mut cfg_b = config_in(dir_b.path());
    cfg_b.name = String::from("test-device-b");
    (cfg_b.arc_port, cfg_b.cmc_port, cfg_b.flows_port) = (4441, 8801, 4456);
    cfg_b.process_id = cfg.process_id.wrapping_add(1);
    let mut saved_b = DeviceState::from_config(&cfg_b);
    saved_b.subscriptions = vec![None; cfg_b.rx.len()];
    saved_b.subscriptions[0] = Some(Subscription {
        tx_channel: cfg.tx[0].clone(),
        tx_device: cfg.name.clone(),
    });
    saved_b.save(&dir_b.path().join("state.toml")).unwrap();
    let group = dstack::media::tx::multicast_group(ip, cfg.process_id, 0);
    let transmitter = start(&cfg);
    let receiver = start(&cfg_b);
    let (seen, flows) = destinations_of(&receiver, |_| vec![group]);
    // The pair is where the audio comes: RX 1 counts packets of its flow.
    let end = Instant::now() + Duration::from_secs(5);
    while receiver.media().rx_channels()[0].packets == 0 && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    let heard = receiver.media().rx_channels().remove(0);
    let stopped_b = receiver.stop(Duration::from_secs(5));
    let stopped_a = transmitter.stop(Duration::from_secs(5));
    assert!(
        flows.iter().any(|&(_, g)| g == Some(group)),
        "RX 1 on the multicast flow: {flows:?} {seen:?}"
    );
    assert_eq!(seen.last().unwrap().pairs, [group], "{seen:?}");
    assert!(heard.packets > 0, "{heard:?} {seen:?}");
    // Each set seen has a later generation (a poll may miss a short one).
    assert!(
        seen.windows(2).all(|w| w[0].generation < w[1].generation),
        "{seen:?}"
    );
    assert!(matches!(stopped_b, Stopped::Done), "{stopped_b:?}");
    assert!(matches!(stopped_a, Stopped::Done), "{stopped_a:?}");
}
