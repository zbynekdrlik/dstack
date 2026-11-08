// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::Relay;

/// A wait that keeps looking for `d`.
fn for_up_to(d: Duration) -> impl FnMut() -> bool {
    let start = Instant::now();
    move || !d.saturating_sub(start.elapsed()).is_zero()
}

#[test]
fn an_offer_nobody_takes_comes_back_to_the_caller() {
    let relay = Relay::new();
    let mut looks = 0;
    let back = relay.hand_over(7u32, || {
        looks += 1;
        looks < 3
    });
    assert_eq!(back, Err(7));
    assert_eq!(looks, 3, "waited while told to");
    // A later offer starts afresh.
    assert_eq!(relay.hand_over(8u32, || false), Err(8));
}

#[test]
fn a_stopped_relay_takes_nothing() {
    let relay = Relay::new();
    relay.stop();
    let never = || -> bool { panic!("no wait for a stopped relay") };
    assert_eq!(relay.hand_over(1u32, never), Err(1));
    // serve returns at once.
    relay.serve(|_: &mut u32, _| panic!("nothing to cover"));
}

/// The relay thread takes the value, works on it until it is asked back,
/// and hands it back with its work; the owner and the relay never hold it
/// at the same time.
#[test]
fn the_relay_covers_until_asked_back_and_hands_the_value_back() {
    let relay = Arc::new(Relay::new());
    let passes = Arc::new(AtomicU64::new(0));
    let server = {
        let (relay, passes) = (Arc::clone(&relay), Arc::clone(&passes));
        std::thread::spawn(move || {
            relay.serve(|v: &mut Vec<u64>, back| {
                while !back() {
                    v.push(passes.fetch_add(1, Ordering::Relaxed));
                    std::thread::sleep(Duration::from_micros(100));
                }
            });
        })
    };
    let mut v = Vec::new();
    for round in 0..20u64 {
        v.push(1_000_000 + round);
        relay
            .hand_over(v, for_up_to(Duration::from_secs(5)))
            .expect("the relay takes it");
        // The owner's block.
        std::thread::sleep(Duration::from_millis(1));
        v = relay.take_back();
    }
    relay.stop();
    server.join().unwrap();
    // Every pass of the relay is in the value, in order, once.
    let passes_in: Vec<u64> = v.iter().copied().filter(|&x| x < 1_000_000).collect();
    let all = passes.load(Ordering::Relaxed);
    assert_eq!(passes_in, (0..all).collect::<Vec<_>>());
    let rounds: Vec<u64> = v.iter().copied().filter(|&x| x >= 1_000_000).collect();
    assert_eq!(rounds, (1_000_000..1_000_020).collect::<Vec<_>>());
}

/// A relay that wakes late: the owner withdraws its offer, keeps the
/// value, and the late relay does not take it.
#[test]
fn a_withdrawn_offer_is_not_taken_by_a_late_relay() {
    let relay = Arc::new(Relay::new());
    assert_eq!(relay.hand_over(5u32, || false), Err(5));
    let server = {
        let relay = Arc::clone(&relay);
        std::thread::spawn(move || relay.serve(|_: &mut u32, _| panic!("withdrawn")))
    };
    std::thread::sleep(Duration::from_millis(20));
    relay.stop();
    server.join().unwrap();
}
