use super::*;

use std::io;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::mpsc;

use crate::daemon::Place;

const TEST_DEVICE: &str = include_str!("../../../../config/test-device.toml");
/// Every wait's bound, so a mutant that hangs fails fast.
const BOUND: Duration = Duration::from_secs(5);
/// An address no interface of a build box holds (RFC 5737's TEST-NET-1).
const NOWHERE: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 77);

#[test]
fn exit_codes_are_phase_b_s() {
    assert_eq!(exit_code(&HostError::Setup("the config".into())), 2);
    assert_eq!(exit_code(&HostError::Run("the address".into())), 1);
    // The error's text is its own, as the binary logged it.
    assert_eq!(HostError::Run("why".into()).to_string(), "why");
    assert_eq!(HostError::Setup("why".into()).to_string(), "why");
}

#[test]
fn a_serving_error_is_ended_with_its_reason() {
    let control = Control::default();
    let served = settle(&control, Err(ServeError::Arc(io::Error::other("gone"))));
    let why = "the ARC server stopped: gone";
    assert_eq!(served, Err(HostError::Run(why.into())));
    assert_eq!(control.ended.get().map(String::as_str), Some(why));
    // An end that was asked for is none.
    let asked = Control::default();
    assert_eq!(settle(&asked, Ok(())), Ok(()));
    assert_eq!(asked.ended.get(), None);
}

// ---- the setup ----

/// The synthetic device's config, its state file in `dir`.
fn config_in(dir: &Path) -> (PathBuf, DeviceConfig) {
    let state = dir.join("state.toml");
    let text = format!(
        "{}\nstate_file = {:?}\n",
        TEST_DEVICE
            .lines()
            .filter(|l| !l.starts_with("state_file"))
            .collect::<Vec<_>>()
            .join("\n"),
        state.display().to_string()
    );
    let path = dir.join("device.toml");
    std::fs::write(&path, &text).unwrap();
    let cfg = DeviceConfig::from_toml(&text).unwrap();
    assert_eq!(cfg.state_file, state);
    (path, cfg)
}

fn options(config: PathBuf, place: Place, dir: &Path) -> Options {
    Options {
        config,
        place,
        control: dir.join("control.sock"),
    }
}

fn setup_error(e: HostError) -> String {
    match e {
        HostError::Setup(why) => why,
        HostError::Run(why) => panic!("a run error, not a setup one: {why}"),
    }
}

fn run_error(e: HostError) -> String {
    match e {
        HostError::Run(why) => why,
        HostError::Setup(why) => panic!("a setup error, not a run one: {why}"),
    }
}

#[test]
fn a_config_is_read_or_refused_naming_its_path() {
    let dir = tempfile::tempdir().unwrap();
    let (path, cfg) = config_in(dir.path());
    assert_eq!(read_config(&path), Ok(cfg));
    let missing = dir.path().join("none.toml");
    let why = setup_error(read_config(&missing).unwrap_err());
    assert!(
        why.starts_with(&format!("{}: ", missing.display())),
        "{why}"
    );
    let bad = dir.path().join("bad.toml");
    std::fs::write(&bad, "zyxname = [").unwrap();
    let why = setup_error(read_config(&bad).unwrap_err());
    assert!(why.starts_with(&format!("{}: ", bad.display())), "{why}");
}

#[test]
fn a_device_loads_only_with_a_writable_state_folder() {
    let dir = tempfile::tempdir().unwrap();
    let (_, cfg) = config_in(dir.path());
    let device = load_device(cfg.clone()).unwrap();
    assert_eq!(device.cfg, cfg);
    assert!(!cfg.state_file.exists(), "nothing written but checked");
    // A folder that is not there: found at the start.
    let gone = DeviceConfig {
        state_file: dir.path().join("none/state.toml"),
        ..cfg.clone()
    };
    let why = setup_error(load_device(gone).unwrap_err());
    assert!(why.starts_with("the state file cannot be written"), "{why}");
    // A state file that does not hold.
    std::fs::write(&cfg.state_file, "zyxname = [").unwrap();
    setup_error(load_device(cfg).unwrap_err());
}

#[test]
fn a_device_follows_a_config_whose_channel_counts_changed() {
    // Live 1 (#72): phase C's config gave the device more channels than
    // its state file held, and the start was refused with exit 2.
    let dir = tempfile::tempdir().unwrap();
    let (_, cfg) = config_in(dir.path());
    let fewer = DeviceConfig {
        rx: cfg.rx[..8].to_vec(),
        tx: cfg.tx[..4].to_vec(),
        ..cfg.clone()
    };
    let mut state = DeviceState::from_config(&fewer);
    state.rx[2] = "Return 3".into();
    state.save(&cfg.state_file).unwrap();
    let device = load_device(cfg.clone()).unwrap();
    assert_eq!(device.state.rx.len(), 32);
    assert_eq!(device.state.tx, cfg.tx);
    assert_eq!(device.state.rx[2], "Return 3");
    // Saved: the next start reads the same, with nothing left to fit.
    let saved = std::fs::read(&cfg.state_file).unwrap();
    assert_eq!(load_device(cfg.clone()).unwrap(), device);
    assert_eq!(std::fs::read(&cfg.state_file).unwrap(), saved);
    // A cut that would drop a rename: the setup error (exit 2), by kind
    // and number, the file kept.
    let mut more = DeviceState::from_config(&cfg);
    more.tx.push("zyxq 17".into());
    more.save(&cfg.state_file).unwrap();
    let kept = std::fs::read(&cfg.state_file).unwrap();
    let why = setup_error(load_device(cfg.clone()).unwrap_err());
    assert!(
        why.contains("a renamed TX channel past the config's count (channel 17)"),
        "{why}"
    );
    assert!(!why.contains("zyxq"), "{why} echoes a name");
    assert_eq!(std::fs::read(&cfg.state_file).unwrap(), kept);
}

#[test]
fn the_box_s_interfaces_include_the_loopback_address() {
    let all = interfaces().unwrap();
    assert!(
        all.iter()
            .any(|i| i.ip == Ipv4Addr::LOCALHOST && i.netmask == Ipv4Addr::new(255, 0, 0, 0)),
        "{all:?}"
    );
}

// The interface facts come from Linux files (/sys); Windows reads the adapter.
#[cfg(target_os = "linux")]
#[test]
fn an_interface_without_a_usable_mac_is_setup_and_one_not_there_a_run_error() {
    let lo = IfAddr {
        name: "lo".into(),
        ip: Ipv4Addr::LOCALHOST,
        netmask: Ipv4Addr::new(255, 0, 0, 0),
    };
    // The loopback's MAC is all zeros: no hardware address.
    let why = setup_error(interface_info(&lo).unwrap_err());
    assert_eq!(why, "the interface has no usable MAC address");
    let none = IfAddr {
        name: "zyxnone0".into(),
        ..lo
    };
    let why = run_error(interface_info(&none).unwrap_err());
    assert!(why.starts_with("the interface's MAC address: "), "{why}");
}

#[test]
fn prepare_refuses_as_the_binary_did_before_anything_is_bound() {
    let dir = tempfile::tempdir().unwrap();
    let (path, _) = config_in(dir.path());
    let opts = |config: PathBuf, place: Place| options(config, place, dir.path());
    // The config: exit 2.
    let missing = dir.path().join("none.toml");
    let e = prepare(&opts(missing, Place::Ip(NOWHERE)), &[])
        .err()
        .unwrap();
    assert_eq!(exit_code(&e), 2, "{e}");
    // An engine channel the device lacks (16 TX channels): exit 2, before
    // the interface is looked at.
    for engine in [&[16][..], &[3, 20]] {
        let e = prepare(&opts(path.clone(), Place::Ip(NOWHERE)), engine)
            .err()
            .unwrap();
        let why = setup_error(e);
        assert_eq!(
            why,
            format!(
                "the engine's TX channel {} is past the device's 16 TX channels",
                engine.last().unwrap() + 1
            )
        );
    }
    // An address the Dante network cannot reach (the loopback): exit 1 at
    // once; the engine's channels were the device's. An address or an
    // interface not there yet is waited for (dstack #225,
    // `a_place_that_appears_after_the_start_is_the_device_s`).
    let e = prepare(&opts(path.clone(), Place::Ip(Ipv4Addr::LOCALHOST)), &[15])
        .err()
        .unwrap();
    let why = run_error(e);
    assert!(why.contains("loopback"), "{why}");
    assert!(!why.contains("127.0.0"), "no address in {why}");
    assert!(!dir.path().join("control.sock").exists(), "nothing bound");
}

// The interface facts come from Linux files (/sys); Windows reads the adapter.
#[cfg(target_os = "linux")]
#[test]
fn a_place_that_appears_after_the_start_is_the_device_s() {
    // dstack #225: dsend started at logon in the same second the PC got
    // its Dante address, and failed at once. The address appears here on
    // the third lookup (on the loopback interface, so the start goes on to
    // its facts and stops there: no usable MAC, nothing bound).
    let dir = tempfile::tempdir().unwrap();
    let (_, cfg) = config_in(dir.path());
    let lo = |ip| IfAddr {
        name: "lo".into(),
        ip,
        netmask: Ipv4Addr::new(255, 0, 0, 0),
    };
    let mut lists = [
        vec![lo(Ipv4Addr::LOCALHOST)],
        vec![lo(Ipv4Addr::LOCALHOST)],
        vec![lo(Ipv4Addr::LOCALHOST), lo(NOWHERE)],
    ]
    .into_iter();
    let (looked, slept) = (std::cell::Cell::new(0), std::cell::Cell::new(0));
    let control = dir.path().join("control.sock");
    let e = prepare_on(
        cfg,
        &Place::Ip(NOWHERE),
        Some(&control),
        &[],
        ClockSource::System,
        |place| {
            daemon::wait_for_interface(
                place,
                || {
                    looked.set(looked.get() + 1);
                    Ok(lists.next().expect("looked up again after it appeared"))
                },
                |_| slept.set(slept.get() + 1),
            )
        },
    )
    .err()
    .unwrap();
    assert_eq!(setup_error(e), "the interface has no usable MAC address");
    assert_eq!((looked.get(), slept.get()), (3, 2));
    assert!(!control.exists(), "nothing bound");
}

// ---- the running device ----

/// A host whose device thread runs `body` (the real thread runs serving
/// and the goodbyes).
fn hosting(body: impl FnOnce(&Control) -> Result<(), HostError> + Send + 'static) -> Host {
    let control = Arc::new(Control::default());
    let theirs = Arc::clone(&control);
    let thread = std::thread::spawn(move || body(&theirs));
    Host { control, thread }
}

/// Waits (bounded) until the host asks serving to end, then takes a little
/// while to end, as the stops and the goodbyes do.
fn asked_then_ends(control: &Control) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    rt.block_on(async { tokio::time::timeout(BOUND, control.stop.notified()).await })
        .expect("the stop is asked within the bound");
    std::thread::sleep(Duration::from_millis(20));
}

#[test]
fn a_stop_asks_serving_to_end_and_waits_for_the_device() {
    let host = hosting(|c| {
        asked_then_ends(c);
        Ok(())
    });
    assert_eq!(host.ended(), None, "serving");
    assert_eq!(host.stop(BOUND), Stopped::Done);
}

#[test]
fn a_device_that_failed_on_its_way_out_is_failed_with_its_error() {
    let host = hosting(|c| {
        asked_then_ends(c);
        Err(HostError::Run("the mDNS shutdown was not confirmed".into()))
    });
    assert_eq!(
        host.stop(BOUND),
        Stopped::Failed("the mDNS shutdown was not confirmed".into())
    );
    let host = hosting(|c| {
        asked_then_ends(c);
        panic!("zyxboom");
    });
    assert_eq!(host.stop(BOUND), Stopped::Failed(PANICKED.into()));
}

#[test]
fn a_device_still_running_at_the_end_of_the_wait_is_left_to_end() {
    let (release, released) = mpsc::channel::<()>();
    let host = hosting(move |_| {
        // Deaf to the stop: ends when the test releases it (or at the
        // bound).
        let _ = released.recv_timeout(BOUND);
        Ok(())
    });
    assert_eq!(host.ended(), None);
    assert_eq!(host.stop(Duration::from_millis(50)), Stopped::StillRunning);
    release.send(()).unwrap();
}

#[test]
fn serving_that_ended_by_itself_is_seen_before_the_stop() {
    let (ended, seen) = mpsc::channel::<()>();
    let host = hosting(move |c| {
        let served = settle(c, Err(ServeError::Control(io::Error::other("zyxgone"))));
        ended.send(()).unwrap();
        // The RX stops and the goodbyes still run before the thread ends.
        asked_then_ends(c);
        served
    });
    seen.recv_timeout(BOUND).expect("serving ends");
    let why = "the control socket stopped: zyxgone";
    assert_eq!(host.ended().as_deref(), Some(why));
    assert_eq!(host.stop(BOUND), Stopped::Failed(why.into()));
}

#[test]
fn wait_is_the_device_thread_s_own_end() {
    assert_eq!(hosting(|_| Ok(())).wait(), Ok(()));
    assert_eq!(
        hosting(|_| Err(HostError::Setup("zyxwhy".into()))).wait(),
        Err(HostError::Setup("zyxwhy".into()))
    );
    assert_eq!(
        hosting(|_| panic!("zyxboom")).wait(),
        Err(HostError::Run(PANICKED.into()))
    );
}

#[test]
fn only_a_held_info_port_makes_a_member() {
    use std::io::{Error, ErrorKind};
    assert!(info_port_held(&Error::from(ErrorKind::AddrInUse)));
    assert!(!info_port_held(&Error::from(ErrorKind::PermissionDenied)));
    assert!(!info_port_held(&Error::from(ErrorKind::AddrNotAvailable)));
}

#[test]
fn a_held_info_port_makes_a_member_any_other_bind_error_ends_the_start() {
    use std::io::{Error, ErrorKind};
    assert!(member_or(Error::from(ErrorKind::AddrInUse)).is_ok());
    let other = member_or(Error::from(ErrorKind::PermissionDenied)).unwrap_err();
    assert_eq!(other.kind(), ErrorKind::PermissionDenied);
}

/// dstack #104: the local status endpoint listens on 127.0.0.1 and its
/// port is the one the device announces.
#[tokio::test]
async fn the_local_endpoint_s_port_is_the_device_s() {
    let dir = tempfile::tempdir().unwrap();
    let mut device = crate::daemon::media::tests::device_at(dir.path());
    assert_eq!(device.status_port, None);
    let listener = open_local(&mut device).expect("127.0.0.1 binds");
    let at = listener.local_addr().unwrap();
    assert!(at.ip().is_loopback());
    assert_eq!(device.status_port, Some(at.port()));
}

/// Owner 4.11.2026: every device of ours follows Dante PTP in its own
/// process by default (dsync's follower), the dsync service only its
/// fallback; `prepare`, `Embedded::start` and the daemon take the default.
#[test]
fn the_default_clock_is_the_in_process_follower_linked_to_the_service() {
    assert_eq!(ClockSource::default(), ClockSource::Linked);
}
