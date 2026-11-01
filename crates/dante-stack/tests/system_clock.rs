//! A whole device on the test-only system clock (dasio's CI E2E: a
//! GitHub-hosted runner has no dantesync and no PTP network). The real
//! host is prepared on the box's IPv4 address on its default route (the
//! production checks stay: no loopback, a real MAC) with
//! `ClockSource::System` and a state
//! file that subscribes RX 1 to our own TX 1 (resolved from our own
//! records, no mDNS); it starts its threads as in production. TX 1 sends
//! the counter; after the start, a second of RX 1 must read clean with no
//! TX schedule restart and every packet within the latency. Only our own
//! device is subscribed.

#![cfg(any(target_os = "linux", windows))]

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use dante_stack::ctl;
use dante_stack::daemon::{Options, Place};
use dante_stack::device::{DeviceConfig, DeviceState, Subscription};
use dante_stack::host::{self, ClockSource, Signals};

const TEST_DEVICE: &str = include_str!("../../../config/test-dante-device.toml");
/// One device at a time: each binds the Dante ports on the same address.
static ONE_DEVICE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// How long the flow may take to be asked for, granted and read clean.
const BOUND: Duration = Duration::from_secs(20);

/// The test device's config with its state file in `dir`, and a state
/// file that subscribes RX 1 to our own TX 1.
fn device_in(dir: &Path) -> PathBuf {
    config_in(dir, true)
}

/// The test device's config (10 ms latency; no RX channels unless `rx`)
/// with its state file in `dir`; with `rx`, RX 1 subscribed to our TX 1.
fn config_in(dir: &Path, rx: bool) -> PathBuf {
    let state = dir.join("state.toml");
    let text = format!(
        "{}\nstate_file = {:?}\n",
        TEST_DEVICE
            .lines()
            .filter(|l| !l.starts_with("state_file"))
            .scan(false, |in_rx, l| {
                // TX only: the RX list goes, `rx = []` stands in for it.
                if !rx && l.starts_with("rx = [") {
                    *in_rx = true;
                    return Some(Some("rx = []"));
                }
                if *in_rx {
                    *in_rx = l.trim() != "]";
                    return Some(None);
                }
                Some(Some(l))
            })
            .flatten()
            .map(|l| {
                // 10 ms, not the test device's 1 ms: a CI runner (and a dev
                // box) schedules a normal-priority thread over 1 ms late now
                // and then, which restarts the TX schedule (dev1, 30.10:
                // one restart and a 192-frame dropout in a second). This
                // test proves the path; real-time timing is the hardware
                // gate's.
                if l.starts_with("latency_ns") {
                    "latency_ns = 10000000"
                } else {
                    l
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        state.display().to_string()
    );
    let path = dir.join("device.toml");
    std::fs::write(&path, &text).unwrap();
    let cfg = DeviceConfig::from_toml(&text).unwrap();
    if !rx {
        assert!(cfg.rx.is_empty());
        return path;
    }
    let mut saved = DeviceState::from_config(&cfg);
    saved.subscriptions = vec![None; cfg.rx.len()];
    saved.subscriptions[0] = Some(Subscription {
        tx_channel: cfg.tx[0].clone(),
        tx_device: cfg.name.clone(),
    });
    saved.save(&state).unwrap();
    path
}

/// The box's IPv4 address on its default route: the adapter a runner
/// really uses (not a virtual or down one). Connecting a UDP socket picks
/// the route and sends nothing.
fn routed_ipv4() -> Ipv4Addr {
    let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("a UDP socket");
    probe
        .connect((Ipv4Addr::new(192, 0, 2, 1), 9))
        .expect("a default route on this box");
    match probe.local_addr().expect("the routed address") {
        SocketAddr::V4(a) if !a.ip().is_loopback() && !a.ip().is_unspecified() => *a.ip(),
        other => panic!("no routed IPv4 address on this box: {other}"),
    }
}

/// The number after `"key":` in the first object of `json` that starts
/// with `head`, if any.
fn field(json: &str, head: &str, key: &str) -> Option<u64> {
    let at = json.find(head)?;
    let rest = &json[at..];
    let end = rest.find('}')?;
    let obj = &rest[..end];
    let k = format!("\"{key}\":");
    let v = &obj[obj.find(&k)? + k.len()..];
    v.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// What the test compares across the measured second.
struct Counts {
    checked: u64,
    mismatch: u64,
    dropout: u64,
    slips: u64,
    rebootstraps: u64,
    late_bins: Vec<u64>,
}

impl Counts {
    fn of(status: &str) -> Counts {
        let check = |key| field(status, "\"checks\":[{", key).unwrap_or(u64::MAX);
        let at = status
            .find("\"late_bins\":[")
            .map(|i| i + "\"late_bins\":[".len());
        let late_bins = at
            .and_then(|at| status[at..].find(']').map(|end| &status[at..at + end]))
            .map(|list| list.split(',').filter_map(|n| n.parse().ok()).collect())
            .unwrap_or_default();
        Counts {
            checked: field(status, "\"checks\":[{", "checked").unwrap_or(0),
            mismatch: check("mismatch"),
            dropout: check("dropout"),
            slips: check("slips"),
            rebootstraps: field(status, "\"tx\":[{", "rebootstraps").unwrap_or(u64::MAX),
            late_bins,
        }
    }
}

/// The status once RX 1's check has read `frames` frames, or the last one
/// at the bound (or when the device ended).
fn wait_checked(
    media: &dante_stack::daemon::media::Media,
    host: &host::Host,
    frames: u64,
) -> String {
    let start = Instant::now();
    loop {
        let status = ctl::answer("status", media);
        let done = field(&status, "\"checks\":[{", "checked").unwrap_or(0) >= frames;
        if done || start.elapsed() > BOUND || host.ended().is_some() {
            return status;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn our_tx_reaches_our_rx_clean_on_the_system_clock() {
    let _one = ONE_DEVICE.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let ip = routed_ipv4();
    let opts = Options {
        config: device_in(dir.path()),
        place: Place::Ip(ip),
        control: dir.path().join("control.sock"),
    };
    let prepared = host::prepare_with(&opts, &[], ClockSource::System).expect("the device set up");
    let media = prepared.media();
    // Ready at once: no dantesync.
    let status = ctl::answer("status", &media);
    assert_eq!(field(&status, "\"clock\":", "ready"), Some(1), "{status}");
    assert!(status.contains("\"source\":\"system\"}"), "{status}");
    assert_eq!(ctl::answer("source 1 counter", &media), "ok");
    assert_eq!(ctl::answer("check 1 counter", &media), "ok");
    let stop = Arc::new(AtomicBool::new(false));
    let host = prepared
        .start(Signals::Report(Arc::clone(&stop)))
        .expect("the device started");
    // The start (threads scheduled for the first time, the flow asked for
    // and granted) is left out: the counts are read once RX 1 has been read
    // for 0.1 s, and the second after that must add nothing bad. A slow
    // runner (the coverage build) restarted the schedule at the start.
    let warm = wait_checked(&media, &host, 9_600);
    let base = Counts::of(&warm);
    let last = wait_checked(&media, &host, base.checked + 96_000);
    // dstack #47: the Latency tab's data, read from our running device as
    // from any receiver: its RX flows over ARC and its heartbeats.
    let flows = {
        use dante_stack::arc::client::{REPLY_TIMEOUT, Udp};
        let seq = std::sync::atomic::AtomicU16::new(1);
        let requests = dante_stack::ctl::Requests::default();
        let halt = AtomicBool::new(false);
        let to = std::net::SocketAddrV4::new(ip, dante_stack::arc::PORT);
        let mut udp = Udp::open(ip, to, &seq, REPLY_TIMEOUT, &requests, &halt).expect("a socket");
        dante_stack::arc::control::rx_flows(&mut udp)
    };
    let heard = dante_stack::latency::listen(ip, ip, Duration::from_millis(2_500));
    let stopped = host.stop(Duration::from_secs(5));
    let flows = flows.expect("our RX flows");
    assert_eq!(flows.len(), 1, "{flows:?}");
    assert_eq!(
        (flows[0].rate, flows[0].channels.as_slice()),
        (96_000, &[1u16][..])
    );
    assert!(flows[0].latency_ns > 0, "{flows:?}");
    let heard = heard.expect("the heartbeat group joined");
    assert_eq!(heard.len(), 1, "one device at our address: {heard:?}");
    let heard = &heard[0];
    assert!(heard.beats >= 2, "{heard:?}");
    assert_eq!(heard.rate, 96_000, "{heard:?}");
    assert_eq!(heard.slots.len(), 32, "{heard:?}");
    let slot = &heard.slots[usize::from(flows[0].slot) - 1];
    assert_eq!(slot.peaks_us.len(), heard.beats as usize, "{heard:?}");
    let end = Counts::of(&last);
    assert!(
        end.checked >= base.checked + 96_000,
        "no second of RX 1 read within {BOUND:?}: {last}"
    );
    assert_eq!(
        (end.mismatch, end.dropout, end.slips),
        (base.mismatch, base.dropout, base.slips),
        "RX 1 after the start: {last}"
    );
    assert_eq!(
        end.rebootstraps, base.rebootstraps,
        "the TX schedule restarted: {last}"
    );
    // Every packet left within the latency (else the schedule would have
    // restarted, checked above, and RX 1 would have dropped out). How late
    // inside it is the machine's real-time behaviour: shown, and judged by
    // the hardware gate, not by a shared runner.
    assert_eq!(end.late_bins.len(), 6, "{last}");
    assert!(
        end.late_bins.iter().sum::<u64>() > base.late_bins.iter().sum::<u64>(),
        "{last}"
    );
    let max_us = field(&last, "\"tx\":[{", "late_max_us").unwrap_or(u64::MAX);
    assert!(
        max_us < 10_000,
        "a TX packet past the 10 ms latency: {last}"
    );
    println!(
        "TX late bins over the measured second (< 0.25, 0.5, 1, 2, 5, >= 5 ms): {:?}",
        end.late_bins
            .iter()
            .zip(&base.late_bins)
            .map(|(e, b)| e - b)
            .collect::<Vec<_>>()
    );
    assert!(matches!(stopped, host::Stopped::Done), "{stopped:?}");
}

/// songplayer's shape: a TX-only device (no RX channel), two channels
/// asked for by a receiver. It starts, grants the flow and sends the
/// counter on TX 1 and silence on TX 2, stamped in order.
#[test]
fn a_tx_only_device_serves_a_receiver() {
    let _one = ONE_DEVICE.lock().unwrap_or_else(|e| e.into_inner());
    use dante_stack::flows::client::Client;
    use dante_stack::flows::control::{FlowRequest, PORT};
    use dante_stack::media::packet;
    use std::net::SocketAddrV4;

    let dir = tempfile::tempdir().unwrap();
    let ip = routed_ipv4();
    let opts = Options {
        config: config_in(dir.path(), false),
        place: Place::Ip(ip),
        control: dir.path().join("control.sock"),
    };
    let prepared =
        host::prepare_with(&opts, &[], ClockSource::System).expect("the TX-only device set up");
    let media = prepared.media();
    assert_eq!(ctl::answer("source 1 counter", &media), "ok");
    let stop = Arc::new(AtomicBool::new(false));
    let host = prepared
        .start(Signals::Report(Arc::clone(&stop)))
        .expect("the device started");
    // A receiver of ours on the same address: its audio socket, then the
    // request to the device's flow control.
    let audio = UdpSocket::bind((ip, 0)).unwrap();
    audio
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let SocketAddr::V4(at) = audio.local_addr().unwrap() else {
        panic!("IPv4")
    };
    let req = FlowRequest {
        rx_host: "test-receiver".to_string(),
        rx_flow_name: "program".to_string(),
        sample_rate: 96_000,
        bits: 24,
        channels: vec![Some(1), Some(2)],
        fpp: 32,
        rx: at,
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let granted = rt.block_on(Client::new(ip).request_flow(SocketAddrV4::new(ip, PORT), &req));
    assert!(granted.is_ok(), "the flow: {granted:?}");
    let mut buf = [0u8; 2048];
    let mut stamps = Vec::new();
    let start = Instant::now();
    while stamps.len() < 300 && start.elapsed() < BOUND {
        let n = audio.recv(&mut buf).expect("audio from the TX-only device");
        let (secs, sub) = packet::read_header(&buf[..n]).expect("a media packet");
        stamps.push(u64::from(secs) * 96_000 + u64::from(sub));
        let mut slot2_silent = true;
        packet::read_frames(&buf[..n], 2, 3, |_, slot, x| {
            if slot == 1 && x != 0 {
                slot2_silent = false;
            }
        })
        .expect("two 24-bit slots");
        assert!(slot2_silent, "TX 2 is silent");
    }
    let stopped = host.stop(Duration::from_secs(5));
    assert!(
        stamps.len() >= 300,
        "{} packets within {BOUND:?}",
        stamps.len()
    );
    assert!(
        stamps.windows(2).all(|w| w[1] == w[0] + 32),
        "every stamp follows the last by 32 frames"
    );
    assert!(matches!(stopped, host::Stopped::Done), "{stopped:?}");
}

/// `card` over the network (the ARC client's UDP exchange) against our own
/// running device: read, write two labels, read them back.
#[test]
fn card_reads_and_writes_our_running_device() {
    use dante_stack::arc::client::{REPLY_TIMEOUT, Udp};
    use dante_stack::daemon::card;
    use std::net::SocketAddrV4;
    use std::sync::atomic::AtomicU16;

    let _one = ONE_DEVICE.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let ip = routed_ipv4();
    let opts = Options {
        config: config_in(dir.path(), true),
        place: Place::Ip(ip),
        control: dir.path().join("control.sock"),
    };
    let prepared = host::prepare_with(&opts, &[], ClockSource::System).expect("the device set up");
    let name = prepared.config().name.clone();
    let rx2 = prepared.config().rx[1].clone();
    let stop = Arc::new(AtomicBool::new(false));
    let host = prepared
        .start(Signals::Report(Arc::clone(&stop)))
        .expect("the device started");
    let seq = AtomicU16::new(1);
    let requests = dante_stack::ctl::Requests::default();
    let halt = AtomicBool::new(false);
    let to = SocketAddrV4::new(ip, dante_stack::arc::PORT);
    let mut udp = Udp::open(ip, to, &seq, REPLY_TIMEOUT, &requests, &halt).expect("a socket");
    let read = card::read(&mut udp, &name);
    let changes = card::parse_changes(
        r#"[{"type":"rx","channel":2,"label":"Vox L"},{"type":"tx","channel":1,"label":"Click"}]"#,
    )
    .unwrap();
    let flag = dir.path().join("EVENT-NOW");
    let written = card::write(&mut udp, &name, &flag, &changes);
    let stopped = host.stop(Duration::from_secs(5));
    let read = read.expect("card read");
    assert_eq!(read.rx[&2], rx2, "{read:?}");
    let written = written.expect("card write");
    assert!(written.error.is_none(), "{written:?}");
    assert!(written.mismatches.is_empty(), "{written:?}");
    assert_eq!(written.rx.as_ref().unwrap()[&2], "Vox L");
    assert_eq!(written.tx.as_ref().unwrap()[&1], "Click");
    // The device saved the labels in its state file.
    let saved = std::fs::read_to_string(dir.path().join("state.toml")).unwrap();
    assert!(
        saved.contains("Vox L") && saved.contains("Click"),
        "{saved}"
    );
    assert!(matches!(stopped, host::Stopped::Done), "{stopped:?}");
}

#[test]
fn discovery_lists_our_running_device_and_its_withdrawal() {
    use dante_stack::discover::{Change, Discovery};
    use std::net::SocketAddrV4;

    let _one = ONE_DEVICE.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let ip = routed_ipv4();
    let opts = Options {
        config: config_in(dir.path(), false),
        place: Place::Ip(ip),
        control: dir.path().join("control.sock"),
    };
    let prepared = host::prepare_with(&opts, &[], ClockSource::System).expect("the device set up");
    let name = prepared.config().name.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let host = prepared
        .start(Signals::Report(Arc::clone(&stop)))
        .expect("the device started");
    let (discovery, changes) = Discovery::start(ip).expect("discovery started");
    // Other Dante devices may share the network: wait for ours by name.
    let deadline = Instant::now() + BOUND;
    let ours = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match changes.recv_timeout(left) {
            Ok(Change::Added(d)) if d.name.eq_ignore_ascii_case(&name) => break d,
            Ok(_) => {}
            Err(e) => panic!("our device was not discovered: {e}"),
        }
    };
    let listed = discovery.devices();
    let stopped = host.stop(Duration::from_secs(5));
    assert!(matches!(stopped, host::Stopped::Done), "{stopped:?}");
    assert_eq!(ours.arc, SocketAddrV4::new(ip, dante_stack::arc::PORT));
    assert_eq!(
        ours.txt.get("arcp_vers").map(String::as_str),
        Some("2.7.41")
    );
    assert!(
        ours.txt.contains_key("mf") && ours.txt.contains_key("model"),
        "{ours:?}"
    );
    assert!(listed.iter().any(|d| d.name == ours.name), "{listed:?}");
    // The stop sends the goodbyes: our device leaves the list.
    let removed = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match changes.recv_timeout(left) {
            Ok(Change::Removed(n)) if n.eq_ignore_ascii_case(&name) => break n,
            Ok(_) => {}
            Err(e) => panic!("our device's withdrawal was not seen: {e}"),
        }
    };
    assert_eq!(removed, ours.name);
    assert!(!discovery.devices().iter().any(|d| d.name == ours.name));
    discovery.stop().expect("discovery stopped");
}

#[test]
fn control_subscribes_our_running_device_and_ends_it() {
    use dante_stack::arc::client::{REPLY_TIMEOUT, Udp};
    use dante_stack::arc::control;
    use std::net::SocketAddrV4;
    use std::sync::atomic::AtomicU16;

    let _one = ONE_DEVICE.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let ip = routed_ipv4();
    let opts = Options {
        config: config_in(dir.path(), true),
        place: Place::Ip(ip),
        control: dir.path().join("control.sock"),
    };
    let prepared = host::prepare_with(&opts, &[], ClockSource::System).expect("the device set up");
    let name = prepared.config().name.clone();
    let tx1 = prepared.config().tx[0].clone();
    let stop = Arc::new(AtomicBool::new(false));
    let host = prepared
        .start(Signals::Report(Arc::clone(&stop)))
        .expect("the device started");
    let seq = AtomicU16::new(1);
    let requests = dante_stack::ctl::Requests::default();
    let halt = AtomicBool::new(false);
    let to = SocketAddrV4::new(ip, dante_stack::arc::PORT);
    let mut udp = Udp::open(ip, to, &seq, REPLY_TIMEOUT, &requests, &halt).expect("a socket");
    // Our own RX 2 (RX 1 the config subscribes already) to our own TX 1
    // (the tests' rule: our device only).
    let made = control::subscribe(&mut udp, 2, &tx1, &name);
    let after_made = control::rx_channels(&mut udp);
    let ended = control::unsubscribe(&mut udp, 2);
    let after_end = control::rx_channels(&mut udp);
    let stopped = host.stop(Duration::from_secs(5));
    made.expect("the subscription answered OK");
    let after_made = after_made.expect("the RX channels read");
    assert_eq!(after_made[1].tx_channel.as_deref(), Some(tx1.as_str()));
    assert_eq!(after_made[1].tx_device.as_deref(), Some(name.as_str()));
    assert!(after_made[1].sub_status().is_some(), "{:?}", after_made[1]);
    ended.expect("the end answered OK");
    let after_end = after_end.expect("the RX channels read again");
    assert_eq!(
        (after_end[1].tx_channel.as_deref(), after_end[1].status),
        (None, 0)
    );
    assert!(matches!(stopped, host::Stopped::Done), "{stopped:?}");
}

#[test]
fn a_direct_instance_query_reads_our_running_device_s_channel_record() {
    use dante_stack::subscribe::instance::Instances;
    use dante_stack::subscribe::resolve::Resolver;

    let _one = ONE_DEVICE.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let ip = routed_ipv4();
    let opts = Options {
        config: config_in(dir.path(), false),
        place: Place::Ip(ip),
        control: dir.path().join("control.sock"),
    };
    let prepared = host::prepare_with(&opts, &[], ClockSource::System).expect("the device set up");
    let (name, tx1) = (
        prepared.config().name.clone(),
        prepared.config().tx[0].clone(),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let host = prepared
        .start(Signals::Report(Arc::clone(&stop)))
        .expect("the device started");
    let mut instances = Instances::new();
    assert!(
        instances.lookup(&name, &tx1).is_none(),
        "nothing before the query"
    );
    instances.start(ip).expect("the query thread");
    let deadline = Instant::now() + BOUND;
    let found = loop {
        if let Some(a) = instances.lookup(&name, &tx1) {
            break a;
        }
        assert!(
            Instant::now() < deadline,
            "our channel record was never read"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let stopped = host.stop(Duration::from_secs(5));
    assert!(matches!(stopped, host::Stopped::Done), "{stopped:?}");
    assert_eq!(found.control.ip(), &ip);
    assert_eq!(
        (found.tx_channel_id, found.rate, found.bits),
        (1, 96_000, 24)
    );
    assert_eq!(found.latency_ns, 10_000_000, "the test config's latency");
}

#[test]
fn two_devices_share_one_address_each_on_its_own_ports() {
    use dante_stack::arc::client::{REPLY_TIMEOUT, Udp};
    use dante_stack::arc::control;
    use dante_stack::discover::{Change, Discovery};
    use std::net::SocketAddrV4;
    use std::sync::atomic::AtomicU16;

    let _one = ONE_DEVICE.lock().unwrap_or_else(|e| e.into_inner());
    let ip = routed_ipv4();
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let config_a = config_in(dir_a.path(), false);
    // B: another name, its own ports and process id.
    let config_b = config_in(dir_b.path(), false);
    let text = std::fs::read_to_string(&config_b)
        .unwrap()
        .replace("name = \"test-device\"", "name = \"test-device-b\"")
        + "\narc_port = 4441\ncmc_port = 8801\nflows_port = 4456\nprocess_id = 1\n";
    std::fs::write(&config_b, text).unwrap();
    let start = |config: PathBuf, dir: &Path| {
        let opts = Options {
            config,
            place: Place::Ip(ip),
            control: dir.join("control.sock"),
        };
        let prepared = host::prepare_with(&opts, &[], ClockSource::System).expect("set up");
        let name = prepared.config().name.clone();
        let stop = Arc::new(AtomicBool::new(false));
        (
            prepared.start(Signals::Report(stop)).expect("started"),
            name,
        )
    };
    let (host_a, name_a) = start(config_a, dir_a.path());
    let (host_b, name_b) = start(config_b, dir_b.path());
    let seq = AtomicU16::new(1);
    let requests = dante_stack::ctl::Requests::default();
    let halt = AtomicBool::new(false);
    let name_on = |port| {
        let to = SocketAddrV4::new(ip, port);
        let mut udp = Udp::open(ip, to, &seq, REPLY_TIMEOUT, &requests, &halt).unwrap();
        control::device_name(&mut udp)
    };
    let (on_a, on_b) = (name_on(4440), name_on(4441));
    // Both are discovered, each with its own ARC port.
    let (discovery, changes) = Discovery::start(ip).expect("discovery");
    let deadline = Instant::now() + BOUND;
    let mut seen = std::collections::BTreeMap::new();
    while seen.len() < 2 && Instant::now() < deadline {
        if let Ok(Change::Added(d)) = changes.recv_timeout(Duration::from_millis(200))
            && (d.name == name_a || d.name == name_b)
        {
            seen.insert(d.name.clone(), d.arc.port());
        }
    }
    let _ = discovery.stop();
    let stopped_b = host_b.stop(Duration::from_secs(5));
    let stopped_a = host_a.stop(Duration::from_secs(5));
    assert_eq!(on_a.expect("A's ARC"), name_a);
    assert_eq!(on_b.expect("B's ARC"), name_b);
    assert_eq!(seen.get(&name_a), Some(&4440), "{seen:?}");
    assert_eq!(seen.get(&name_b), Some(&4441), "{seen:?}");
    assert!(matches!(stopped_a, host::Stopped::Done), "{stopped_a:?}");
    assert!(matches!(stopped_b, host::Stopped::Done), "{stopped_b:?}");
}
