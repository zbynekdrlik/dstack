// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #442: a process receives each mDNS datagram on ONE 5353 socket
//! per address, whatever its devices there. Up to v0.6.192 every device
//! had a daemon socket of its own, the process an mDNS meter's beside it,
//! and on Windows every device a responder's too, each joined to the group:
//! one datagram was delivered three times on dsg's system CPU, inside the
//! NIC's receive DPC (dasio#190: 0.1-0.8 ms of DPC a datagram, late
//! SoundGrid callbacks).
//!
//! The devices are served on loopback (dstack #366). Linux counts the
//! process's sockets bound to port 5353: its own file descriptors
//! (`/proc/self/fd`) matched by inode against `/proc/net/udp`, judged on
//! a read that lists one (a read can skip rows, #455). The records
//! are read as a consumer reads them, by a direct query of a channel
//! (`subscribe::instance`, from a port of its own).

#![cfg(any(target_os = "linux", windows))]

#[cfg(target_os = "linux")]
use std::collections::{BTreeSet, HashSet};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use dstack::daemon::{Options, Place};
use dstack::host::{self, ClockSource, Signals};
use dstack::subscribe::instance::Instances;
use dstack::subscribe::resolve::Resolver;

const TEST_DEVICE: &str = include_str!("../../../config/test-device.toml");
/// One device set at a time: each binds the Dante ports on the same address.
static ONE_DEVICE: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// How long a record may take to be read.
const BOUND: Duration = Duration::from_secs(20);

/// The test device's address: loopback (dstack #366).
fn test_ipv4() -> Ipv4Addr {
    Ipv4Addr::LOCALHOST
}

/// The test device's config with its state file in `dir`; `second` gives
/// it another name, its own ports and process id (two on one address).
fn config_with(dir: &Path, second: bool) -> PathBuf {
    let state = dir.join("state.toml");
    let mut text: String = TEST_DEVICE
        .lines()
        .filter(|l| !l.starts_with("state_file"))
        .map(|l| format!("{l}\n"))
        .collect();
    text.push_str(&format!("state_file = {:?}\n", state.display().to_string()));
    if second {
        text = text.replace("name = \"test-device\"", "name = \"test-device-b\"")
            + "arc_port = 4441\ncmc_port = 8801\nflows_port = 4456\nprocess_id = 1\n";
    }
    let path = dir.join("device.toml");
    std::fs::write(&path, &text).unwrap();
    path
}

/// A started device on the test address: its host, name and TX 1.
fn start(dir: &Path, second: bool) -> (host::Host, String, String) {
    let opts = Options {
        config: config_with(dir, second),
        place: Place::Ip(test_ipv4()),
        control: dir.join("control.sock"),
    };
    let prepared = host::prepare_with(&opts, &[], ClockSource::System).expect("set up");
    let (name, tx1) = (
        prepared.config().name.clone(),
        prepared.config().tx[0].clone(),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let host = prepared.start(Signals::Report(stop)).expect("started");
    (host, name, tx1)
}

/// The inodes of this process's open sockets, exact: `/proc/self/fd` is
/// listed by fd number, so a socket closed or opened meanwhile never hides
/// another.
#[cfg(target_os = "linux")]
fn socket_inodes() -> HashSet<String> {
    let mut ours = HashSet::new();
    for fd in std::fs::read_dir("/proc/self/fd").unwrap().flatten() {
        let Ok(link) = std::fs::read_link(fd.path()) else {
            continue;
        };
        let link = link.to_string_lossy().to_string();
        if let Some(inode) = link
            .strip_prefix("socket:[")
            .and_then(|s| s.strip_suffix(']'))
        {
            ours.insert(inode.to_string());
        }
    }
    ours
}

/// One read of `/proc/net/udp`'s rows on port 5353 of the sockets `ours`:
/// (`local_address` hex, e.g. `0100007F`, inode), sorted, each once. The
/// read is not exact (#455): the kernel hands the table out a page a
/// `read` and walks it again to its count at each `read`, so every row
/// listed earlier that closes between two `read`s (a device thread's
/// socket, another process's) skips a later one. Never judge from an
/// empty read, and judge a close by [`socket_inodes`].
#[cfg(target_os = "linux")]
fn rows_5353(ours: &HashSet<String>) -> Vec<(String, String)> {
    let table = std::fs::read_to_string("/proc/net/udp").unwrap();
    let rows: BTreeSet<(String, String)> = table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            let (addr, port) = cols.get(1)?.split_once(':')?;
            let inode = cols.get(9)?;
            (port == "14E9" && ours.contains(*inode)).then(|| (addr.to_string(), inode.to_string()))
        })
        .collect();
    rows.into_iter().collect()
}

/// The sockets of this process bound to UDP port 5353 (address hex and
/// inode, [`rows_5353`]), from the first read within 5 s that lists one
/// of the sockets open at the call. A row a read skips comes back in the
/// next; a socket the read does not list at all is not counted (a read
/// can only miss a second socket, never add one).
#[cfg(target_os = "linux")]
fn mdns_sockets() -> Vec<(String, String)> {
    let ours = socket_inodes();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let rows = rows_5353(&ours);
        if !rows.is_empty() || deadline.saturating_duration_since(Instant::now()).is_zero() {
            return rows;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The addresses of [`mdns_sockets`]' sockets.
#[cfg(target_os = "linux")]
fn addresses(held: &[(String, String)]) -> Vec<&str> {
    held.iter().map(|(addr, _)| addr.as_str()).collect()
}

/// The 5353 sockets once the last device stopped, as `<address> <inode>`:
/// the daemon's thread confirms its shutdown just before it drops its
/// socket, so the open fds are read again for up to 1 s while one of
/// `held` is open (exact: [`socket_inodes`]). Then one read of the table
/// for any other 5353 socket of the process.
#[cfg(target_os = "linux")]
fn sockets_after_stop(held: &[(String, String)]) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let ours = socket_inodes();
        let open: Vec<String> = held
            .iter()
            .filter(|(_, inode)| ours.contains(inode))
            .map(|(addr, inode)| format!("{addr} {inode}"))
            .collect();
        if open.is_empty() {
            return rows_5353(&ours)
                .into_iter()
                .map(|(addr, inode)| format!("{addr} {inode}"))
                .collect();
        }
        if deadline.saturating_duration_since(Instant::now()).is_zero() {
            return open;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Waits until a direct query reads `device`'s channel `tx` (at most
/// [`BOUND`]): the daemon answers on the shared socket.
fn read_record(device: &str, tx: &str) {
    read_record_at(device, tx, dstack::subscribe::instance::MDNS);
}

/// [`read_record`] with the queries sent to `to` (the group, or a unicast
/// address and port).
fn read_record_at(device: &str, tx: &str, to: SocketAddrV4) {
    let mut instances = Instances::new();
    instances
        .start_to(test_ipv4(), to)
        .expect("the query thread");
    let deadline = Instant::now() + BOUND;
    while instances.lookup(device, tx).is_none() {
        assert!(
            Instant::now() < deadline,
            "{device}'s channel record was never read"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A direct query never reads `device`'s channel `tx` for `within`: its
/// record was withdrawn (a record still registered is read in ms).
fn never_read(device: &str, tx: &str, within: Duration) {
    let mut instances = Instances::new();
    instances.start(test_ipv4()).expect("the query thread");
    let deadline = Instant::now() + within;
    while !deadline.saturating_duration_since(Instant::now()).is_zero() {
        assert!(
            instances.lookup(device, tx).is_none(),
            "{device}'s withdrawn record was read"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_device_s_process_receives_mdns_on_one_socket_and_still_answers() {
    let _one = ONE_DEVICE.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (host, name, tx1) = start(dir.path(), false);
    // The daemon opens its socket on its own thread: counted once it
    // answered.
    read_record(&name, &tx1);
    #[cfg(target_os = "linux")]
    let held = mdns_sockets();
    let stopped = host.stop(Duration::from_secs(5));
    assert!(matches!(stopped, host::Stopped::Done), "{stopped:?}");
    #[cfg(target_os = "linux")]
    {
        assert_eq!(held.len(), 1, "one 5353 socket: {held:?}");
        // The #444 trial switch is off by default: the wildcard's socket.
        assert_eq!(addresses(&held), ["00000000"], "bound to the wildcard");
        assert_eq!(
            sockets_after_stop(&held),
            Vec::<String>::new(),
            "none after the stop"
        );
    }
}

/// dstack #444's last step, #451: a device's one 5353 socket is bound to
/// its address exactly where the OS hands such a socket the group
/// (`announce::mdns::GROUP_TO_BOUND_SOCKET`: Windows, where the wildcard
/// daemon went deaf after an address cycle until a restart), else the
/// wildcard's (Linux). Either way it answers a unicast query to
/// `<address>:5353` and a direct query to the group.
#[test]
fn a_device_s_one_socket_is_bound_to_its_address_where_the_os_hands_it_the_group() {
    use dstack::announce::mdns::{GROUP_TO_BOUND_SOCKET, Mdns};
    let _one = ONE_DEVICE.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (host, name, tx1) = start(dir.path(), false);
    read_record_at(&name, &tx1, SocketAddrV4::new(test_ipv4(), 5353));
    read_record(&name, &tx1);
    let bound = Mdns::shared(test_ipv4())
        .expect("the device's daemon")
        .bound();
    #[cfg(target_os = "linux")]
    let held = mdns_sockets();
    let stopped = host.stop(Duration::from_secs(5));
    assert!(matches!(stopped, host::Stopped::Done), "{stopped:?}");
    assert_eq!(bound, GROUP_TO_BOUND_SOCKET, "the device's daemon");
    #[cfg(target_os = "linux")]
    {
        assert_eq!(
            addresses(&held),
            ["00000000"],
            "one 5353 socket, the wildcard's"
        );
        assert_eq!(
            sockets_after_stop(&held),
            Vec::<String>::new(),
            "none after the stop"
        );
    }
}

#[test]
fn two_devices_on_one_address_share_the_socket_and_one_s_stop_keeps_the_other() {
    let _one = ONE_DEVICE.lock().unwrap_or_else(|e| e.into_inner());
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (host_a, name_a, tx_a) = start(dir_a.path(), false);
    let (host_b, name_b, tx_b) = start(dir_b.path(), true);
    read_record(&name_a, &tx_a);
    read_record(&name_b, &tx_b);
    #[cfg(target_os = "linux")]
    let both = mdns_sockets();
    let stopped_b = host_b.stop(Duration::from_secs(5));
    // B's stop withdrew B's records only: A still answers, a fresh
    // reader reads it, and never B's.
    read_record(&name_a, &tx_a);
    never_read(&name_b, &tx_b, Duration::from_secs(2));
    #[cfg(target_os = "linux")]
    let after_b = mdns_sockets();
    let stopped_a = host_a.stop(Duration::from_secs(5));
    assert!(matches!(stopped_b, host::Stopped::Done), "{stopped_b:?}");
    assert!(matches!(stopped_a, host::Stopped::Done), "{stopped_a:?}");
    #[cfg(target_os = "linux")]
    {
        assert_eq!(both.len(), 1, "one 5353 socket for both: {both:?}");
        assert_eq!(after_b, both, "A keeps the socket");
        assert_eq!(
            sockets_after_stop(&both),
            Vec::<String>::new(),
            "none after the stops"
        );
    }
}

#[test]
fn the_address_s_meter_counts_what_the_shared_socket_hears() {
    use dstack::announce::mdns::Mdns;
    let _one = ONE_DEVICE.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (host, name, tx1) = start(dir.path(), false);
    // The direct query goes to the group: the daemon's socket hears it.
    read_record(&name, &tx1);
    let mdns = Mdns::shared(test_ipv4()).expect("the device's daemon");
    let meter = mdns.meter().expect("the address's meter");
    let heard = meter.heard();
    #[cfg(target_os = "linux")]
    let held = mdns_sockets();
    drop((meter, mdns));
    let stopped = host.stop(Duration::from_secs(5));
    assert!(matches!(stopped, host::Stopped::Done), "{stopped:?}");
    assert!(heard > 0, "the meter heard nothing");
    #[cfg(target_os = "linux")]
    assert_eq!(held.len(), 1, "the device's own daemon: {held:?}");
}

/// The traffic watch's discovery of our other devices (`traffic::neighbours`,
/// dstack #203) as a device runs it while its NIC is loud past our audio:
/// it reads the group from the daemon's socket, so the process still holds
/// one 5353 socket, and it lists the devices on the address.
#[test]
fn a_loud_nic_s_neighbours_discovery_keeps_the_one_socket() {
    use dstack::traffic::neighbours::{DiscoveryFinder, Finder};
    let _one = ONE_DEVICE.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (host, name, tx1) = start(dir.path(), false);
    read_record(&name, &tx1);
    let mdns = dstack::announce::mdns::Mdns::shared(test_ipv4()).expect("the device's daemon");
    let before = mdns.listeners();
    let mut finder = DiscoveryFinder::default();
    finder
        .start(test_ipv4())
        .expect("the neighbours' discovery");
    // It listens on the daemon's socket.
    let listening = mdns.listeners();
    let deadline = Instant::now() + BOUND;
    while !finder.devices().iter().any(|d| d.name == name) {
        assert!(Instant::now() < deadline, "{name} was never listed");
        std::thread::sleep(Duration::from_millis(50));
    }
    #[cfg(target_os = "linux")]
    let held = mdns_sockets();
    // Its stop ends its thread in the background: the listener goes with it.
    finder.stop();
    let deadline = Instant::now() + BOUND;
    while mdns.listeners() > before && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let after = mdns.listeners();
    drop(mdns);
    let stopped = host.stop(Duration::from_secs(5));
    assert!(matches!(stopped, host::Stopped::Done), "{stopped:?}");
    assert_eq!(listening, before + 1, "the discovery's listener");
    assert_eq!(after, before, "gone with the discovery");
    #[cfg(target_os = "linux")]
    {
        assert_eq!(
            held.len(),
            1,
            "one 5353 socket with the discovery: {held:?}"
        );
        assert_eq!(
            sockets_after_stop(&held),
            Vec::<String>::new(),
            "none after the stops"
        );
    }
}
