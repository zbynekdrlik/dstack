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
//! (`/proc/self/fd`) matched by inode against `/proc/net/udp`. The records
//! are read as a consumer reads them, by a direct query of a channel
//! (`subscribe::instance`, from a port of its own).

#![cfg(any(target_os = "linux", windows))]

use std::net::Ipv4Addr;
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
fn config_in(dir: &Path, second: bool) -> PathBuf {
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
        config: config_in(dir, second),
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

/// The sockets of this process bound to UDP port 5353, as `<address>`
/// (`/proc/net/udp`'s hex `local_address`, e.g. `0100007F`).
#[cfg(target_os = "linux")]
fn mdns_sockets() -> Vec<String> {
    let mut ours = std::collections::HashSet::new();
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
    let table = std::fs::read_to_string("/proc/net/udp").unwrap();
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            let (addr, port) = cols.get(1)?.split_once(':')?;
            let inode = cols.get(9)?;
            (port == "14E9" && ours.contains(*inode)).then(|| addr.to_string())
        })
        .collect()
}

/// Waits until a direct query reads `device`'s channel `tx` (at most
/// [`BOUND`]): the daemon answers on the shared socket.
fn read_record(device: &str, tx: &str) {
    let mut instances = Instances::new();
    instances.start(test_ipv4()).expect("the query thread");
    let deadline = Instant::now() + BOUND;
    while instances.lookup(device, tx).is_none() {
        assert!(
            Instant::now() < deadline,
            "{device}'s channel record was never read"
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
        assert_eq!(mdns_sockets(), Vec::<String>::new(), "none after the stop");
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
    // reader reads it.
    read_record(&name_a, &tx_a);
    #[cfg(target_os = "linux")]
    let after_b = mdns_sockets();
    let stopped_a = host_a.stop(Duration::from_secs(5));
    assert!(matches!(stopped_b, host::Stopped::Done), "{stopped_b:?}");
    assert!(matches!(stopped_a, host::Stopped::Done), "{stopped_a:?}");
    #[cfg(target_os = "linux")]
    {
        assert_eq!(both.len(), 1, "one 5353 socket for both: {both:?}");
        assert_eq!(after_b, both, "A keeps the socket");
        assert_eq!(mdns_sockets(), Vec::<String>::new(), "none after the stops");
    }
}
