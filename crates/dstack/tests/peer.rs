// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #305: the CI test peer (`dstack-dev peer`) on the box's routed
//! address. Two peers in this process, each on the TEST system clock with
//! its own skew, send each other a tone: each hears the other's tone whole,
//! and each one's play times show its own clock's skew against the app's
//! clock. The binary's command line parses, runs and stops on SIGINT with
//! its status written. A file of its own: its devices set the `normal`
//! timing profile (one per process).

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::Path;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use dstack::host::Stopped;
use dstack::peer::Peer;
use dstack::peer::args::{self, Parsed, PeerArgs};

/// Held by each test that runs a device: they bind the Dante ports on the
/// routed address.
static ONE_DEVICE: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

fn peer_args(line: &str, dir: &Path, name: &str) -> PeerArgs {
    let mut words: Vec<String> = line.split_whitespace().map(str::to_string).collect();
    words.extend([
        "--ip".into(),
        routed_ipv4().to_string(),
        "--name".into(),
        name.into(),
        "--status".into(),
        dir.join(format!("{name}.json")).display().to_string(),
    ]);
    match args::parse(&words) {
        Ok(Parsed::Run(a)) => *a,
        other => panic!("{line}: {other:?}"),
    }
}

/// A peer's counts the gap check reads.
#[derive(Debug, Clone, Copy)]
struct Counts {
    gaps: u64,
    holes: u64,
    tx_restarts: u64,
    pump_restarts: u64,
    blocks: u64,
}

impl Counts {
    fn of(p: &Peer) -> Counts {
        let v: serde_json::Value = serde_json::from_str(&p.status()).unwrap();
        let num = |x: &serde_json::Value| x.as_u64().unwrap_or_else(|| panic!("a count: {x}"));
        let pump = p.media().engine().expect("the pump");
        Counts {
            gaps: p.shared().rx[0].view().gaps,
            holes: num(&v["device"]["channels"][0]["holes"]),
            tx_restarts: num(&v["device"]["tx"][0]["rebootstraps"]),
            pump_restarts: pump.rebootstraps,
            blocks: pump.blocks,
        }
    }

    fn minus(&self, before: &Counts) -> Counts {
        Counts {
            gaps: self.gaps - before.gaps,
            holes: self.holes - before.holes,
            tx_restarts: self.tx_restarts - before.tx_restarts,
            pump_restarts: self.pump_restarts - before.pump_restarts,
            blocks: self.blocks - before.blocks,
        }
    }
}

/// Waits up to `bound` for `done`, polling.
fn wait_for(bound: Duration, mut done: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + bound;
    while !done() {
        if Instant::now() > end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

/// The design's whole-device test: two devices at +40 and -30 ppm, each
/// receiving the other's tone (a 70 ppm drift between them: 0.7 ms over the
/// test's 10 s, well inside the 10 ms latency). Counted after both lock
/// and settle: no tone gap without a counted cause (a restart of the
/// sender's TX schedule or of a pump, or a ring hole: dev1 loaded by
/// another account's job restarted a TX schedule once and the check saw
/// it), no play-time step; each device's play times run at its own skew
/// against the app's clock.
#[test]
fn two_peers_at_different_skews_hear_each_other_s_tone_whole() {
    let _one = ONE_DEVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().unwrap();
    let a = peer_args(
        "--tone-hz 1000 --rx-from peer-b:TX~1 --test-clock-ppm 40",
        dir.path(),
        "peer-a",
    );
    let b = peer_args(
        "--tone-hz 440 --rx-from peer-a:TX~1 --test-clock-ppm -30 \
         --ports 4441,8801,4456 --process-id 1",
        dir.path(),
        "peer-b",
    );
    // The channel names hold a space: put it back (the line splits on it).
    let fix = |mut p: PeerArgs| {
        if let Some((_, chs)) = p.rx_from.as_mut() {
            for c in chs.iter_mut() {
                *c = c.replace('~', " ");
            }
        }
        p
    };
    let a = Peer::start(fix(a)).expect("peer A started");
    let b = Peer::start(fix(b)).expect("peer B started");
    let locked = |p: &Peer| p.shared().rx[0].view().locked;
    let both = wait_for(Duration::from_secs(20), || locked(&a) && locked(&b));
    assert!(
        both,
        "locked: A {}, B {}\nA {}\nB {}",
        locked(&a),
        locked(&b),
        a.status(),
        b.status()
    );
    // Settle (the play-time filter's first pairs), then count.
    std::thread::sleep(Duration::from_secs(1));
    let before = [Counts::of(&a), Counts::of(&b)];
    std::thread::sleep(Duration::from_secs(8));
    let after = [Counts::of(&a), Counts::of(&b)];
    let status = [a.status(), b.status()];
    let ppm = |p: &Peer| f64::from_bits(p.shared().play_at_ppm.load(Ordering::Relaxed));
    let ppms = [ppm(&a), ppm(&b)];
    let hz = [a.shared().rx[0].view().hz, b.shared().rx[0].view().hz];
    let steps = [
        a.media().engine().expect("A's pump").play_at_steps,
        b.media().engine().expect("B's pump").play_at_steps,
    ];
    assert_eq!((a.ended(), b.ended()), (None, None));
    assert!(matches!(b.stop(), Stopped::Done));
    assert!(matches!(a.stop(), Stopped::Done));
    let rise = |i: usize| after[i].minus(&before[i]);
    for (me, other) in [(0, 1), (1, 0)] {
        let (mine, theirs) = (rise(me), rise(other));
        // Every gap has a counted cause: the sender's TX schedule or pump
        // restarting (a loaded box: another account's job), our pump
        // restarting or our ring's holes. On a quiet box all are 0.
        let causes = theirs.tx_restarts + theirs.pump_restarts + mine.pump_restarts + mine.holes;
        assert!(
            mine.gaps <= causes,
            "peer {me}: {} tone gaps, {causes} counted causes ({mine:?}, sender {theirs:?}): {}",
            mine.gaps,
            status[me]
        );
        assert!(mine.blocks > 0, "{mine:?}");
        assert_eq!(steps[me], 0, "play-time steps on peer {me}: {}", status[me]);
    }
    // Each hears the other's frequency, as the other's clock sends it.
    assert!(
        (hz[0] - 440.0).abs() < 0.1 && (hz[1] - 1000.0).abs() < 0.1,
        "{hz:?}"
    );
    // The play times show each device's own skew (the filter follows it
    // within its slew bound, 100 ppm).
    assert!(
        (ppms[0] - 40.0).abs() < 5.0,
        "A's play_at rate {} ppm",
        ppms[0]
    );
    assert!(
        (ppms[1] + 30.0).abs() < 5.0,
        "B's play_at rate {} ppm",
        ppms[1]
    );
    // The status file's fields, as a CI gate reads them.
    let v: serde_json::Value = serde_json::from_str(&status[0]).unwrap();
    assert_eq!(v["peer"]["test_clock_ppm"], 40.0);
    assert_eq!(v["peer"]["rx"][0]["tone_gaps"], after[0].gaps);
    assert_eq!(v["device"]["engine"]["play_at_steps"], 0);
    assert!(
        v["device"]["engine"]["play_at_max_dev_us"].is_u64(),
        "{}",
        status[0]
    );
}

/// The binary: `peer --help` exits 0, a usage error 2, and a run on the
/// routed address writes its status and ends on SIGINT with exit 0.
#[cfg(unix)]
#[test]
fn the_peer_command_line_parses_runs_and_stops() {
    use std::process::Command;
    let bin = env!("CARGO_BIN_EXE_dstack-dev");
    let help = Command::new(bin).args(["peer", "--help"]).output().unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).starts_with("usage: dstack-dev peer"));
    let bad = Command::new(bin)
        .args(["peer", "--name", "x"])
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("--iface or --ip is missing"));

    let _one = ONE_DEVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().unwrap();
    let status = dir.path().join("peer.json");
    let mut child = Command::new(bin)
        .args([
            "peer",
            "--ip",
            &routed_ipv4().to_string(),
            "--name",
            "peer-cli",
        ])
        .args(["--tone-hz", "1000", "--test-clock-ppm", "6", "--status"])
        .arg(&status)
        .spawn()
        .unwrap();
    let written = wait_for(Duration::from_secs(15), || {
        std::fs::read_to_string(&status)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .is_some_and(|v| v["peer"]["blocks"].as_u64().is_some_and(|n| n > 0))
    });
    let kill = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(kill.success());
    let end = Instant::now() + Duration::from_secs(10);
    let code = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s.code();
        }
        if Instant::now() > end {
            let _ = child.kill();
            panic!("the peer did not end on SIGINT");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(written, "no status with blocks at {}", status.display());
    assert_eq!(code, Some(0));
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&status).unwrap()).unwrap();
    assert_eq!(v["peer"]["name"], "peer-cli");
    assert_eq!(v["peer"]["test_clock_ppm"], 6.0);
    assert_eq!(v["device"]["clock"]["source"], "system");
}
