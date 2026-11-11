// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::{Arc, Mutex};

use super::*;
use crate::traffic::{OTHER_BYTES_PER_S, Traffic};

const S: u64 = 1_000_000_000;
// Documentation addresses (RFC 5737) only.
const IP: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 10);

/// What a [`FakeFinder`] was asked, and what it answers.
#[derive(Debug, Default)]
pub(crate) struct Fake {
    pub running: bool,
    pub devices: Vec<Device>,
    pub starts: Vec<Ipv4Addr>,
    pub stops: usize,
    /// The reason a start fails with; `None`: it starts.
    pub fail: Option<String>,
}

/// A [`Finder`] whose state a test holds.
pub(crate) struct FakeFinder(pub Arc<Mutex<Fake>>);

impl Finder for FakeFinder {
    fn running(&self) -> bool {
        self.0.lock().unwrap().running
    }
    fn start(&mut self, ip: Ipv4Addr) -> Result<(), String> {
        let mut f = self.0.lock().unwrap();
        f.starts.push(ip);
        if let Some(reason) = f.fail.clone() {
            return Err(reason);
        }
        f.running = true;
        Ok(())
    }
    fn stop(&mut self) {
        let mut f = self.0.lock().unwrap();
        f.stops += 1;
        f.running = false;
    }
    fn devices(&self) -> Vec<Device> {
        self.0.lock().unwrap().devices.clone()
    }
}

/// One of our devices at `ip` with its status port `port`.
pub(crate) fn ours_at(name: &str, ip: Ipv4Addr, port: u16) -> Device {
    let mut txt = BTreeMap::new();
    txt.insert(
        crate::announce::STATUS_PORT_KEY.to_string(),
        port.to_string(),
    );
    Device {
        name: name.to_string(),
        arc: SocketAddrV4::new(ip, 4440),
        dbc: None,
        cmc: None,
        device_id: None,
        txt,
    }
}

fn quiet() -> Traffic {
    Traffic {
        other: OTHER_BYTES_PER_S,
        ours: 5_000_000,
        devices: Vec::new(),
    }
}

fn loud() -> Traffic {
    Traffic {
        other: OTHER_BYTES_PER_S + 1,
        ..quiet()
    }
}

#[test]
fn the_rest_past_ours_is_the_other_traffic_and_the_neighbours_audio() {
    let t = Traffic {
        other: 7,
        ours: 1_000,
        devices: vec![("a".to_string(), 30), ("b".to_string(), 200)],
    };
    assert_eq!(rest_past_ours(&t), 237);
    assert_eq!(rest_past_ours(&quiet()), OTHER_BYTES_PER_S);
}

#[test]
fn the_discovery_starts_at_a_loud_look_and_stops_after_a_quiet_hour() {
    let mut lazy = Lazy::default();
    // Quiet looks start nothing.
    assert_eq!(lazy.step(0, &quiet(), false), Step::Keep);
    assert_eq!(lazy.step(60 * S, &quiet(), false), Step::Keep);
    // A loud one starts it; while it runs, it is kept.
    assert_eq!(lazy.step(120 * S, &loud(), false), Step::Start);
    assert_eq!(lazy.step(180 * S, &loud(), true), Step::Keep);
    assert_eq!(lazy.step(240 * S, &quiet(), true), Step::Keep);
    // A start that failed is asked again at the next look.
    assert_eq!(lazy.step(300 * S, &quiet(), false), Step::Start);
    // An hour after the last loud look: stopped, and not started again.
    assert_eq!(
        lazy.step(180 * S + QUIET_NS - 1, &quiet(), true),
        Step::Keep
    );
    assert_eq!(lazy.step(180 * S + QUIET_NS, &quiet(), true), Step::Stop);
    assert_eq!(lazy.step(240 * S + QUIET_NS, &quiet(), false), Step::Keep);
}

/// A NIC loud only with our neighbours' audio keeps the discovery: without
/// it the next look would count them as other traffic again.
#[test]
fn neighbours_audio_alone_keeps_the_discovery() {
    let mut lazy = Lazy::default();
    let ours_only = Traffic {
        other: 0,
        ours: 5_000_000,
        devices: vec![("a".to_string(), OTHER_BYTES_PER_S + 1)],
    };
    assert_eq!(lazy.step(0, &ours_only, false), Step::Start);
    assert_eq!(lazy.step(QUIET_NS, &ours_only, true), Step::Keep);
    assert_eq!(lazy.step(2 * QUIET_NS, &quiet(), true), Step::Stop);
}

fn with_fake() -> (Neighbours, Arc<Mutex<Fake>>) {
    let fake = Arc::new(Mutex::new(Fake::default()));
    (
        Neighbours::new(Box::new(FakeFinder(Arc::clone(&fake)))),
        fake,
    )
}

#[test]
fn the_list_is_empty_until_the_discovery_runs_and_never_holds_us() {
    let (mut n, fake) = with_fake();
    n.set_own(Some(5000));
    fake.lock().unwrap().devices = vec![
        ours_at("me", IP, 5000),
        ours_at("a", IP, 5001),
        ours_at("elsewhere", Ipv4Addr::new(192, 0, 2, 99), 5002),
    ];
    assert!(n.list(IP).is_empty(), "not running");
    fake.lock().unwrap().running = true;
    assert_eq!(n.list(IP), vec![("a".to_string(), 5001)]);
}

#[test]
fn a_loud_look_starts_the_discovery_on_our_address_and_a_quiet_hour_stops_it() {
    crate::test_log::undecided();
    let (mut n, fake) = with_fake();
    // No window closed: nothing happens, however loud the last one was.
    n.after(0, None, IP);
    n.after(0, Some(&quiet()), IP);
    assert!(fake.lock().unwrap().starts.is_empty());
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        n.after(60 * S, Some(&loud()), IP);
        n.after(120 * S, Some(&loud()), IP);
    });
    assert_eq!(fake.lock().unwrap().starts, vec![IP]);
    assert_eq!(log.matches("discovery started").count(), 1, "{log}");
    n.after(120 * S + QUIET_NS - 1, Some(&quiet()), IP);
    assert_eq!(fake.lock().unwrap().stops, 0);
    n.after(120 * S + QUIET_NS, None, IP);
    assert_eq!(fake.lock().unwrap().stops, 0, "only at a look");
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        n.after(120 * S + QUIET_NS, Some(&quiet()), IP);
    });
    let f = fake.lock().unwrap();
    assert_eq!(f.stops, 1);
    assert!(!f.running);
    assert!(log.contains("quiet for an hour"), "{log}");
}

#[test]
fn a_discovery_that_does_not_start_is_tried_at_each_look_and_warned_about_once() {
    crate::test_log::undecided();
    let (mut n, fake) = with_fake();
    fake.lock().unwrap().fail = Some("no socket".to_string());
    let log = crate::test_log::capture_warnings(|| {
        n.after(0, Some(&loud()), IP);
        n.after(60 * S, Some(&quiet()), IP);
        n.after(120 * S, Some(&quiet()), IP);
    });
    assert_eq!(fake.lock().unwrap().starts.len(), 3);
    assert_eq!(log.matches("did not start").count(), 1, "{log}");
    assert!(log.contains("no socket"), "{log}");
    // It starts; stopped after a quiet hour; failing again later is told
    // again.
    fake.lock().unwrap().fail = None;
    n.after(180 * S, Some(&loud()), IP);
    assert!(fake.lock().unwrap().running);
    n.after(180 * S + QUIET_NS, Some(&quiet()), IP);
    assert!(!fake.lock().unwrap().running);
    fake.lock().unwrap().fail = Some("no socket".to_string());
    let log = crate::test_log::capture_warnings(|| {
        n.after(2 * QUIET_NS, Some(&loud()), IP);
    });
    assert_eq!(log.matches("did not start").count(), 1, "{log}");
}

/// The real finder lists nothing while it does not run, and a stop of
/// nothing does nothing.
#[test]
fn the_discovery_finder_lists_nothing_until_it_starts() {
    let mut f = DiscoveryFinder::default();
    assert!(!f.running());
    assert!(f.devices().is_empty());
    f.stop();
    assert!(!f.running());
}
