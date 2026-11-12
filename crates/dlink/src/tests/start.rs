//! A start whose place is not there (dstack #225, #230): `Link::start`
//! fails at once, `Link::start_until` waits until the app's stop, and the
//! waits an app runs before its start (`wait_for_place`,
//! `default_route_place`).
//!
//! The bounds here are loose on purpose: a start that waits never answers
//! (the place never comes), so any bound of seconds catches it, while the
//! start's own work before the place (the state file, the interface list)
//! may take a while on a loaded runner. That a start that fails at once
//! looks once and never sleeps is the daemon's unit tests' (panicking
//! sleeps, `daemon/tests/place.rs`).

use super::*;

use std::time::Duration;

#[test]
fn a_place_the_dante_network_cannot_reach_is_refused_without_a_wait() {
    // dstack #225: the wait an app runs before its own use of the address
    // (dsend's and dlisten's free ports) refuses as `Link::start_until`
    // does.
    match wait_for_place(&Place::Ip(std::net::Ipv4Addr::LOCALHOST)) {
        Err(HostError::Run(why)) => assert!(why.contains("loopback"), "{why}"),
        other => panic!("{other:?}"),
    }
}

/// dstack #230: the place of an app given no address or interface (dsend,
/// dlisten): the default route's address, as the tests' own routed IPv4
/// (every box that runs the tests has a default route, so no wait here).
#[test]
fn the_default_route_s_place_is_the_box_s_routed_ipv4() {
    let probe = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    probe.connect("192.0.2.1:9").unwrap();
    let std::net::SocketAddr::V4(routed) = probe.local_addr().unwrap() else {
        panic!("no IPv4 route");
    };
    assert_eq!(default_route_place(), Ok(Place::Ip(*routed.ip())));
}

/// An app's processor that writes nothing (the starts below never run it).
struct Quiet;
impl Process for Quiet {
    fn process(&mut self, _block: &mut Block<'_>) {}
}

/// A config whose state file is in a folder of its own: the start writes
/// it (a probe of the folder), and starts sharing a path race each other.
fn own_state() -> (tempfile::TempDir, Config) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config::new("app-link", dir.path().join("dlink.toml"))
        .rx(["In 1", "In 2"])
        .tx(["Out 1", "Out 2"]);
    (dir, cfg)
}

/// An address no interface of any box holds (TEST-NET-1, RFC 5737).
const ABSENT: std::net::Ipv4Addr = std::net::Ipv4Addr::new(192, 0, 2, 77);

/// How long a start that does not wait may take to answer: a start that
/// waits never does.
const ANSWERS: Duration = Duration::from_secs(5);

/// One RX channel, no TX.
fn rx_only() -> Map {
    Map {
        rx: vec![0],
        tx: vec![],
    }
}

/// `start` run on a thread of its own, its answer handed back within
/// `bound` (`None` when it has not answered by then: a start that waits
/// stays blocked on its thread, and the test fails instead of hanging).
fn answer_within(
    bound: Duration,
    start: impl FnOnce() -> Result<Link, HostError> + Send + 'static,
) -> Option<Result<Link, HostError>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(start());
    });
    rx.recv_timeout(bound).ok()
}

/// dstack #230: `Link::start` fails at once on an address not there, as up
/// to v0.6.97 (v0.6.98 waited with no limit and no way out, which hung the
/// apps' own retry and shutdown: songplayer#313, the intercom).
#[test]
fn link_start_on_an_address_not_there_fails_at_once() {
    let (dir, cfg) = own_state();
    let got = answer_within(ANSWERS, move || {
        let _dir = dir;
        Link::start(&cfg, &Place::Ip(ABSENT), rx_only(), Quiet)
    });
    match got {
        Some(Err(HostError::Run(why))) => {
            assert_eq!(why, "no interface of this box holds the address");
        }
        Some(other) => panic!("{:?}", other.map(drop)),
        None => panic!("Link::start still waiting after {ANSWERS:?}"),
    }
}

/// dstack #230: `Link::start_until` waits for an address not there, and
/// its stop ends the wait within one retry interval, with an error the app
/// can tell (it set the stop itself).
#[test]
fn link_start_until_waits_and_ends_within_a_retry_of_its_stop() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let stop = Arc::new(AtomicBool::new(false));
    let theirs = Arc::clone(&stop);
    let (tx, rx) = std::sync::mpsc::channel();
    let (dir, cfg) = own_state();
    std::thread::spawn(move || {
        let _dir = dir;
        let got = Link::start_until(&cfg, &Place::Ip(ABSENT), rx_only(), Quiet, &theirs);
        let _ = tx.send(got);
    });
    // Still waiting a while in (`Link::start` answers in milliseconds).
    let waited = rx.recv_timeout(Duration::from_millis(300));
    assert!(waited.is_err(), "answered before its stop");
    stop.store(true, Ordering::Relaxed);
    // One retry interval, and slack for a loaded runner.
    let bound = dstack::daemon::PLACE_RETRY + Duration::from_millis(500);
    match rx.recv_timeout(bound) {
        Ok(Err(HostError::Run(why))) => {
            assert_eq!(why, "stopped while waiting for the device's address");
        }
        Ok(other) => panic!("{:?}", other.map(drop)),
        Err(_) => panic!("Link::start_until not stopped {bound:?} after its stop"),
    }
}

/// A stop set before the start ends it after one look, without a wait.
#[test]
fn link_start_until_with_its_stop_set_ends_at_once() {
    let (dir, cfg) = own_state();
    let got = answer_within(ANSWERS, move || {
        let _dir = dir;
        let stop = std::sync::atomic::AtomicBool::new(true);
        Link::start_until(&cfg, &Place::Ip(ABSENT), rx_only(), Quiet, &stop)
    });
    match got {
        Some(Err(e)) => assert_eq!(
            e,
            HostError::Run("stopped while waiting for the device's address".into())
        ),
        Some(Ok(_)) => panic!("started"),
        None => panic!("Link::start_until with its stop set still waiting"),
    }
}
