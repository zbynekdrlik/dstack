//! dstack #417: the host frame sender's NIC across an interface re-create.
//! The neighbour rows count only on our NIC's interface index; after the
//! NIC is destroyed and created again under a new index (a USB NIC reset:
//! same address), the TX interface watch must move that index, or every
//! row is filtered out and the flows go through the UDP sockets until a
//! restart.

use super::*;

use std::time::{Duration, Instant};

use crate::membership::fake::Fake;

/// The index the re-created NIC gets.
const NEW_INDEX: u32 = IFINDEX + 2;

/// The receiver at `RESOLVED` in the neighbour table on `ifindex`.
fn rows_on(ifindex: u32) -> [Neighbour; 1] {
    [Neighbour {
        ip: RESOLVED,
        mac: PEER_MAC,
        ifindex,
    }]
}

#[test]
fn after_an_interface_re_create_the_host_path_takes_the_rows_on_the_new_index() {
    let shared = shared();
    let (_a, at_a) = receiver(RESOLVED);
    let slot = flow(&shared, at_a, 32);
    let rec = host(&shared, Take::All);
    assert_eq!(shared.host_mac(slot), Some(PEER_MAC));
    let fake = Fake::at(IFINDEX);
    let mut lister = fake.lister(Ipv4Addr::LOCALHOST, Duration::from_millis(1));
    let mut watch = lister.watch(Ipv4Addr::LOCALHOST);
    let start = Instant::now();
    assert!(
        !shared.keep_send_interface(&mut watch, start, &mut lister.read),
        "the first look only learns"
    );
    // The NIC re-created: the table's next read holds the receiver on the
    // new index only.
    fake.recreate(NEW_INDEX);
    shared.set_neighbours(&rows_on(NEW_INDEX));
    assert_eq!(
        shared.host_mac(slot),
        None,
        "the old index filters the new rows out"
    );
    let later = start + Duration::from_millis(2);
    assert!(shared.keep_send_interface(&mut watch, later, &mut lister.read));
    assert_eq!(
        shared.host_mac(slot),
        Some(PEER_MAC),
        "the watch's look moves the host NIC to the new index"
    );
    // The table's later reads stay on it, the old index's rows do not.
    shared.set_neighbours(&rows_on(NEW_INDEX));
    assert_eq!(shared.host_mac(slot), Some(PEER_MAC));
    shared.set_neighbours(&rows_on(IFINDEX));
    assert_eq!(shared.host_mac(slot), None, "the old index is gone");
    // The frames go to the host sender again, without a restart.
    shared.set_neighbours(&rows_on(NEW_INDEX));
    let mut tx = Transmitter::new(Arc::clone(&shared));
    run(&mut tx, &shared, &rec, slot, 10);
    let lane = shared.lane(slot);
    assert!(lane.packets > 20, "{lane:?}");
    assert_eq!(lane.direct, lane.packets, "{lane:?}");
    assert_eq!(rec.frames().len() as u64, lane.packets);
}

#[test]
fn the_move_is_one_info_line_and_a_held_index_logs_nothing() {
    let shared = shared();
    let _rec = host(&shared, Take::All);
    crate::test_log::undecided();
    let moved = crate::test_log::capture_at(tracing::Level::INFO, || {
        shared.follow_host(Some(NEW_INDEX)).unwrap();
    });
    assert_eq!(moved.matches(HOST_FOLLOWED).count(), 1, "{moved}");
    assert!(moved.contains(&format!("old_index={IFINDEX}")), "{moved}");
    assert!(moved.contains(&format!("new_index={NEW_INDEX}")), "{moved}");
    assert!(moved.contains("nic=test0"), "{moved}");
    let held = crate::test_log::capture_at(tracing::Level::INFO, || {
        shared.follow_host(Some(NEW_INDEX)).unwrap();
    });
    assert!(!held.contains(HOST_FOLLOWED), "{held}");
}

#[test]
fn an_interface_without_an_index_fails_the_act_and_keeps_the_index() {
    let shared = shared();
    let slot = flow(&shared, SocketAddrV4::new(RESOLVED, 4321), 32);
    let _rec = host(&shared, Take::All);
    let text = shared.follow_host(None).unwrap_err().to_string();
    assert!(text.contains(&format!("{HOST_SENDER} on test0")), "{text}");
    assert_eq!(shared.host_mac(slot), Some(PEER_MAC), "the index held");
    shared.set_neighbours(&rows_on(NEW_INDEX));
    assert_eq!(shared.host_mac(slot), None, "still on the old index");
}

#[test]
fn the_watch_asks_again_after_a_list_without_the_index() {
    let shared = shared();
    let slot = flow(&shared, SocketAddrV4::new(RESOLVED, 4321), 32);
    let _rec = host(&shared, Take::All);
    let fake = Fake::at(IFINDEX);
    let mut lister = fake.lister(Ipv4Addr::LOCALHOST, Duration::from_millis(1));
    let mut watch = lister.watch(Ipv4Addr::LOCALHOST);
    let start = Instant::now();
    assert!(!shared.keep_send_interface(&mut watch, start, &mut lister.read));
    fake.without_index();
    shared.set_neighbours(&rows_on(NEW_INDEX));
    let later = start + Duration::from_millis(2);
    crate::test_log::undecided();
    let warned = crate::test_log::capture_warnings(|| {
        assert!(
            !shared.keep_send_interface(&mut watch, later, &mut lister.read),
            "the host path's act failed, whatever the flow sockets did"
        );
    });
    let named = format!("{HOST_SENDER} on test0");
    assert_eq!(warned.matches(&named).count(), 1, "{warned}");
    assert_eq!(shared.host_mac(slot), None, "nothing moved");
    // The index back in the list: the next look moves the NIC to it.
    fake.recreate(NEW_INDEX);
    let again = later + Duration::from_millis(2);
    assert!(shared.keep_send_interface(&mut watch, again, &mut lister.read));
    assert_eq!(shared.host_mac(slot), Some(PEER_MAC));
}

#[test]
fn without_a_sender_there_is_nothing_to_follow() {
    let shared = shared();
    let slot = flow(&shared, SocketAddrV4::new(RESOLVED, 4321), 32);
    shared.follow_host(None).unwrap();
    shared.follow_host(Some(NEW_INDEX)).unwrap();
    shared.set_neighbours(&rows_on(NEW_INDEX));
    assert_eq!(shared.host_mac(slot), None);
    assert_ne!(shared.tx_path(), TxPath::HostSender);
}
