// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #414: the socket past the qdisc (`media::bypass::Direct`) names
//! its interface's index in every send. After a NIC re-create (a new
//! index, the same name and address) the TX interface watch must move it
//! to the new index, or every direct send fails with ENXIO until a
//! restart. No test has CAP_NET_RAW, so the socket is a stand-in; the
//! index is read back.

use super::*;
use crate::media::bypass::Direct;
use crate::membership::fake::Fake;

/// `lo`'s interface index.
fn lo_index() -> i32 {
    let name = std::ffi::CString::new("lo").unwrap();
    // SAFETY: a NUL-terminated name the call only reads.
    let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
    assert!(index > 0, "the box has lo");
    index as i32
}

#[test]
fn after_an_interface_re_create_the_direct_socket_sends_on_the_new_index() {
    let lo = lo_index();
    // The index the interface had before it was re-created.
    let stale = lo + 1000;
    let shared = TxShared::new(cfg(), Ipv4Addr::LOCALHOST, Vec::new().into(), 1, &[])
        .unwrap()
        .with_direct(Direct::stand_in(stale), "lo");
    let fake = Fake::at(4);
    let mut lister = fake.lister(Ipv4Addr::LOCALHOST, Duration::from_millis(1));
    let mut watch = lister.watch(Ipv4Addr::LOCALHOST);
    let start = std::time::Instant::now();
    assert!(
        !shared.keep_send_interface(&mut watch, start, &mut lister.read),
        "the first look only learns"
    );
    assert_eq!(
        shared.direct_index(),
        Some(stale),
        "nothing before a change"
    );
    fake.recreate(9);
    let later = start + Duration::from_millis(2);
    assert!(shared.keep_send_interface(&mut watch, later, &mut lister.read));
    assert_eq!(
        shared.direct_index(),
        Some(lo),
        "the direct socket sends on the interface's index now"
    );
}

/// A device on `nic`'s stale index `stale`, past the qdisc.
fn on_stale(nic: &str, stale: i32) -> TxShared {
    TxShared::new(cfg(), Ipv4Addr::LOCALHOST, Vec::new().into(), 1, &[])
        .unwrap()
        .with_direct(Direct::stand_in(stale), nic)
}

#[test]
fn the_move_is_one_info_line_and_a_held_index_none() {
    let lo = lo_index();
    let shared = on_stale("lo", lo + 1000);
    crate::test_log::undecided();
    let moved = crate::test_log::capture_at(tracing::Level::INFO, || {
        shared.follow_direct().unwrap();
    });
    assert_eq!(
        moved.matches(super::super::direct::FOLLOWED).count(),
        1,
        "{moved}"
    );
    assert!(moved.contains(&format!("new_index={lo}")), "{moved}");
    let held = crate::test_log::capture_at(tracing::Level::INFO, || {
        shared.follow_direct().unwrap();
    });
    assert!(!held.contains(super::super::direct::FOLLOWED), "{held}");
}

#[test]
fn an_interface_name_not_there_fails_the_act_and_is_asked_again() {
    let stale = lo_index() + 1000;
    let shared = on_stale("dstack-none0", stale);
    let fake = Fake::at(4);
    let mut lister = fake.lister(Ipv4Addr::LOCALHOST, Duration::from_millis(1));
    let mut watch = lister.watch(Ipv4Addr::LOCALHOST);
    let start = std::time::Instant::now();
    assert!(!shared.keep_send_interface(&mut watch, start, &mut lister.read));
    fake.recreate(9);
    let later = start + Duration::from_millis(2);
    let again = later + Duration::from_millis(2);
    crate::test_log::undecided();
    let warned = crate::test_log::capture_warnings(|| {
        assert!(
            !shared.keep_send_interface(&mut watch, later, &mut lister.read),
            "the act failed"
        );
        // The watch asks again at its next look, the index unchanged.
        assert!(!shared.keep_send_interface(&mut watch, again, &mut lister.read));
    });
    assert_eq!(shared.direct_index(), Some(stale), "nothing moved");
    assert_eq!(
        warned.matches("not named again").count(),
        2,
        "warned at each look: {warned}"
    );
}

#[test]
fn without_the_socket_there_is_nothing_to_follow() {
    let shared = TxShared::new(cfg(), Ipv4Addr::LOCALHOST, Vec::new().into(), 1, &[]).unwrap();
    assert_eq!(shared.direct_index(), None, "no CAP_NET_RAW in a test");
    shared.follow_direct().unwrap();
}
