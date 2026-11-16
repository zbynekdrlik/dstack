// SPDX-License-Identifier: MIT OR Apache-2.0

//! The device's measured network latency for an app's status (dstack
//! #325): the largest p99 of any RX flow's last second.

use super::*;

/// Slot `slot`'s last second measured one packet of `ns`.
fn measured(shared: &RxShared, slot: usize, ns: u64) {
    shared.lanes[slot].network.record(ns);
    shared.lanes[slot].network.take();
}

#[test]
fn the_network_p99_is_the_largest_of_the_flows_last_seconds() {
    let shared = RxShared::new(RATE, LOOPBACK, Vec::new().into()).unwrap();
    assert_eq!(shared.network_p99_ns(), None, "nothing measured");
    measured(&shared, 0, 150_000);
    assert_eq!(shared.network_p99_ns(), Some(150_000));
    measured(&shared, 3, 420_000);
    measured(&shared, MAX_FLOWS - 1, 90_000);
    assert_eq!(shared.network_p99_ns(), Some(420_000));
    // A slot's second without a packet leaves the others.
    shared.lanes[3].network.take();
    assert_eq!(shared.network_p99_ns(), Some(150_000));
}
