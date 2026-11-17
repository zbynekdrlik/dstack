// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #366: a test run serves its devices on loopback only. dev1's
//! whole-device tests announced mDNS on the LAN leg (the routed address) at
//! about 35 datagrams a second per binary, the top senders on the FOH PC's
//! Dante leg in the minutes its NIC stalled sends (dasio#127).

use super::*;

/// The workspace's `.cargo/config.toml` makes every test process a test
/// run: without it the guard below never fires.
#[test]
fn the_workspace_s_test_processes_are_a_test_run() {
    assert_eq!(
        std::env::var("DSTACK_TEST_LOOPBACK").as_deref(),
        Ok("1"),
        "the [env] key of .cargo/config.toml is lost"
    );
}

/// A device on the box's routed address in a test run is refused before
/// anything is bound, naming how to run a live test; the error names no
/// address.
#[test]
fn a_test_run_refuses_a_device_on_a_site_address() {
    let routed = default_route()
        .expect("the route probe")
        .expect("a default route on this box");
    let dir = tempfile::tempdir().unwrap();
    let (path, _) = config_in(dir.path());
    let e = prepare(&options(path, Place::Ip(routed), dir.path()), &[])
        .err()
        .expect("a device on a site address refused in a test run");
    let why = run_error(e);
    assert!(why.contains("DSTACK_LIVE_TEST=1"), "{why}");
    assert!(why.contains("loopback"), "{why}");
    assert!(!why.contains(&routed.to_string()), "no address in {why}");
    assert!(!dir.path().join("control.sock").exists(), "nothing bound");
}
