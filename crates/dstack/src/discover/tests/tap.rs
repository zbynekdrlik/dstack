//! A discovery that reads the mDNS group from the address's daemon socket
//! (dstack #442, `traffic::neighbours`' in a device process): what the
//! daemon's tap hands on is read as the group's own socket was, and no
//! socket of its own is joined again after an interface change.

use std::net::SocketAddr;

use super::*;

use crate::announce::mdns::{Heard, Taps};
use crate::membership::fake::Fake;

/// The daemon's taps, as `announce::mdns::Mdns` holds them.
fn taps() -> Arc<Taps> {
    Arc::new(Taps::default())
}

/// A source the daemon's socket heard.
const FROM: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 9), 5353));

#[test]
fn a_tapped_discovery_lists_a_device_and_drops_it_at_a_goodbye_its_tap_hands_on() {
    let (mdns, _) = responder(vec![card()]);
    let taps = taps();
    let group = Some(Group::Tap(taps.listen(Heard::All)));
    let (discovery, changes) =
        Discovery::start_group(LOOPBACK, mdns, group, Lister::default()).unwrap();
    let Change::Added(dev) = changes.recv_timeout(BOUND).unwrap() else {
        panic!("an addition first");
    };
    assert_eq!(dev.name, "Card");
    assert_eq!(taps.listeners(), 1, "the discovery's listener");
    let bye = response(vec![ptr(
        "_netaudio-arc._udp.local",
        "Card._netaudio-arc._udp.local",
        0,
    )]);
    taps.feed(&bye, FROM);
    loop {
        match changes.recv_timeout(BOUND).unwrap() {
            Change::Removed(name) => break assert_eq!(name, "Card"),
            Change::Changed(_) => {}
            other => panic!("{other:?}"),
        }
    }
    discovery.stop().unwrap();
    // The thread's end dropped its listener.
    assert_eq!(taps.listeners(), 0);
}

/// dstack #206's busy group, through the tap: a goodbye behind 150 other
/// datagrams is read within a second.
#[test]
fn a_goodbye_behind_a_busy_tap_is_read_within_a_second() {
    let (mdns, _) = responder(vec![card()]);
    let taps = taps();
    let group = Some(Group::Tap(taps.listen(Heard::All)));
    let (discovery, changes) =
        Discovery::start_group(LOOPBACK, mdns, group, Lister::default()).unwrap();
    let Change::Added(_) = changes.recv_timeout(BOUND).unwrap() else {
        panic!("an addition first");
    };
    let other = response(vec![ptr("_other._udp.local", "Box._other._udp.local", 120)]);
    let bye = response(vec![ptr(
        "_netaudio-arc._udp.local",
        "Card._netaudio-arc._udp.local",
        0,
    )]);
    for _ in 0..150 {
        taps.feed(&other, FROM);
    }
    taps.feed(&bye, FROM);
    let sent = Instant::now();
    loop {
        let left = Duration::from_secs(1).saturating_sub(sent.elapsed());
        match changes.recv_timeout(left) {
            Ok(Change::Removed(name)) => break assert_eq!(name, "Card"),
            Ok(Change::Changed(_)) => {}
            other => panic!("the goodbye not read within a second: {other:?}"),
        }
    }
    assert_eq!(taps.dropped(), 0, "the tap's queue held them");
    discovery.stop().unwrap();
}

/// The tap with `queued` datagrams waiting (`[1, 1]`, `[2, 2]`, ...).
fn queued(queued: u8) -> (Arc<Taps>, crate::announce::mdns::Listener) {
    let taps = taps();
    let heard = taps.listen(Heard::All);
    for i in 1..=queued {
        taps.feed(&[i, i], FROM);
    }
    (taps, heard)
}

#[test]
fn draining_a_tap_reads_every_queued_datagram_in_order_then_ends_at_the_empty_one() {
    let (_taps, heard) = queued(5);
    let mut seen = Vec::new();
    drain_tap(&heard, Instant::now() + BOUND, |b| seen.push(b.to_vec()));
    let want: Vec<Vec<u8>> = (1..=5).map(|i| vec![i, i]).collect();
    assert_eq!(seen, want);
    // Nothing queued: one wait of at most `READ_WAIT`, nothing taken.
    let started = Instant::now();
    drain_tap(&heard, Instant::now() + BOUND, |_| panic!());
    let waited = started.elapsed();
    assert!(waited >= READ_WAIT, "{waited:?}");
    assert!(waited < 4 * READ_WAIT, "{waited:?}");
}

#[test]
fn draining_a_tap_reads_nothing_once_its_time_is_up_and_never_waits_past_it() {
    let (_taps, heard) = queued(3);
    drain_tap(&heard, Instant::now(), |_| panic!());
    // The datagrams are still queued for the next round.
    let mut read = 0;
    drain_tap(&heard, Instant::now() + BOUND, |_| read += 1);
    assert_eq!(read, 3);
    // An empty tap is waited for until the round's end only.
    let started = Instant::now();
    drain_tap(&heard, Instant::now() + READ_WAIT / 5, |_| panic!());
    assert!(started.elapsed() < READ_WAIT, "{:?}", started.elapsed());
}

#[test]
fn a_tap_is_no_socket_of_the_discovery_s_own() {
    let sock = UdpSocket::bind((LOOPBACK, 0)).unwrap();
    assert!(Group::Socket(sock).socket().is_some());
    assert!(Group::Tap(taps().listen(Heard::All)).socket().is_none());
}

#[test]
fn a_tapped_discovery_sweeping_a_unicast_responder_reads_no_interface_list() {
    let (mdns, asked) = responder(Vec::new());
    let fake = Fake::at(4);
    let lister = fake.lister(LOOPBACK, Duration::from_millis(1));
    let group = Some(Group::Tap(taps().listen(Heard::All)));
    let (discovery, _changes) = Discovery::start_group(LOOPBACK, mdns, group, lister).unwrap();
    eventually("a sweep", || !asked.lock().unwrap().is_empty());
    // The daemon keeps the group: nothing of the discovery's to follow.
    std::thread::sleep(ROUND);
    assert_eq!(fake.reads(), 0, "nothing to keep");
    discovery.stop().unwrap();
}
