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
