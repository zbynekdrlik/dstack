// SPDX-License-Identifier: GPL-3.0-or-later

//! dstack #20: a device inside an app's own process ([`Embedded`]), on the
//! box's routed address and its own system clock. A file of its own: its
//! process must never have had a signal handler, so that it can check
//! that [`Embedded`] leaves none (the whole-device tests use
//! `Signals::Report`, and tokio keeps a handler for the process's life).
//! Cargo runs test files one after another, so it never meets their ports.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dante_audio_io::{Block, Process};
use dante_stack::daemon::Place;
use dante_stack::device::{DeviceConfig, DeviceState, Subscription};
use dante_stack::embed::Embedded;
use dante_stack::host::{ClockSource, Stopped};
use dante_stack::stream::Map;

const TEST_DEVICE: &str = include_str!("../../../config/test-dante-device.toml");

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
    let dir = tempfile::tempdir().unwrap();
    let ip = routed_ipv4();
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
        Err(dante_stack::host::HostError::Setup(why)) => {
            assert!(why.contains(&format!("RX channel {}", n + 1)), "{why}")
        }
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("started with an RX channel past the device's"),
    }
}
