// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

const ONE: &str = "Virtual Soundcard (x64)";
const TWO: &str = "Another ASIO";

#[test]
fn a_held_driver_is_refused_until_its_hold_is_dropped() {
    let holds = DriverHolds::new();
    let first = holds.claim(ONE);
    assert!(first.is_some(), "a free driver is held");
    assert!(holds.is_held(ONE));
    assert!(holds.claim(ONE).is_none(), "a held driver is refused");
    drop(first);
    assert!(!holds.is_held(ONE), "a dropped hold gives the driver back");
    assert!(holds.claim(ONE).is_some(), "and it can be held again");
}

#[test]
fn each_driver_is_held_on_its_own() {
    let holds = DriverHolds::default();
    let one = holds.claim(ONE).expect("free");
    let two = holds.claim(TWO).expect("another driver is free");
    drop(one);
    assert!(!holds.is_held(ONE));
    assert!(
        holds.is_held(TWO),
        "dropping one hold gives back only its own driver"
    );
    drop(two);
    assert!(!holds.is_held(TWO));
}

#[test]
fn a_parked_driver_stays_held_and_reads_parked() {
    let holds = DriverHolds::new();
    holds.claim(ONE).expect("free").park();
    assert!(holds.is_held(ONE), "a parked driver is never given back");
    assert!(holds.is_parked(ONE));
    assert!(holds.claim(ONE).is_none());
    let other = holds.claim(TWO).expect("free");
    assert!(!holds.is_parked(TWO), "held is not parked");
    drop(other);
    assert!(!holds.is_parked(TWO));
}
