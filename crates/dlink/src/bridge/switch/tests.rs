// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

#[test]
fn a_handover_is_taken_once_and_its_predecessor_comes_back() {
    let (taker, mut switch) = switch::<u32>();
    let mut current = 1;
    assert!(!taker.take(&mut current), "nothing handed over");
    assert_eq!(switch.send(2), Ok(()));
    assert_eq!(
        switch.send(3),
        Err(NOT_TAKEN),
        "not taken yet: refused, never queued"
    );
    assert_eq!(switch.collect(), 0, "nothing replaced yet");
    assert!(taker.take(&mut current));
    assert_eq!(current, 2);
    assert!(!taker.take(&mut current));
    assert_eq!(switch.collect(), 1, "the old one came back to be freed");
    assert_eq!(switch.pending(), 0);
}

#[test]
fn every_replaced_item_comes_back() {
    let (taker, mut switch) = switch::<u32>();
    let mut current = 0;
    let mut back = 0;
    for n in 1..=5 {
        assert_eq!(switch.send(n), Ok(()));
        assert!(taker.take(&mut current));
        assert!(
            !taker.take(&mut current),
            "a take with no handover retires nothing"
        );
        back += switch.collect();
    }
    assert_eq!((back, current, switch.pending()), (5, 5, 0));
}

#[test]
fn a_send_is_refused_while_the_room_back_is_taken() {
    let (taker, mut switch) = switch::<u32>();
    let mut current = 0;
    // Items replaced but not yet back (the audio thread's try_send not run).
    switch.pending = RETIRED;
    assert_eq!(switch.send(1), Err(NOT_TAKEN));
    switch.pending = RETIRED - 1;
    assert_eq!(switch.send(1), Ok(()));
    assert_eq!(switch.pending(), RETIRED);
    assert!(taker.take(&mut current));
    // One came back: room again.
    assert_eq!(switch.send(2), Ok(()));
}

#[test]
fn a_switch_whose_taker_is_gone_refuses() {
    let (taker, mut switch) = switch::<u32>();
    drop(taker);
    assert_eq!(switch.send(1), Err(GONE));
}
