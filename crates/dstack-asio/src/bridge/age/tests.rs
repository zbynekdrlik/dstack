// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use dstack_audio_io::{Block, Process};

use super::*;
use crate::bridge::{Bridge, Spec};
use crate::device::Opened;
use crate::format::Sample;

const M: i64 = MINUTE_NS;
/// 128 frames at 96 kHz, ns.
const CALL_128_96K: u64 = 1_333_333;

/// A clock the test sets, ns.
fn fake() -> (Arc<AtomicI64>, Clock) {
    let t = Arc::new(AtomicI64::new(0));
    let read = Arc::clone(&t);
    (t, Arc::new(move || read.load(Ordering::Relaxed)))
}

#[test]
fn a_window_without_an_age_has_no_max() {
    assert_eq!(Window::new().max_at(0), None);
    assert_eq!(Window::default(), Window::new());
}

/// One age: it is the max for two minutes (its window and the next), then
/// none.
#[test]
fn an_age_is_the_max_for_its_window_and_the_next() {
    let mut w = Window::new();
    w.record(1_000, 100);
    assert_eq!(w.max_at(1_000), Some(100));
    assert_eq!(w.max_at(1_000 + M - 1), Some(100));
    assert_eq!(w.max_at(1_000 + M), Some(100));
    assert_eq!(w.max_at(1_000 + 2 * M - 1), Some(100));
    assert_eq!(w.max_at(1_000 + 2 * M), None);
}

/// Within a window the max keeps the largest; a minute after its start the
/// window closes and its max stays as the previous one, read with the new.
#[test]
fn a_window_keeps_its_max_then_hands_it_to_the_next() {
    let mut w = Window::new();
    w.record(0, 300);
    w.record(10, 50);
    assert_eq!((w.start, w.max), (0, 300));
    w.record(M - 1, 200);
    assert_eq!((w.start, w.max, w.prev), (0, 300, 0), "still the first");
    w.record(M, 40);
    assert_eq!((w.start, w.max, w.prev), (M, 40, 300));
    assert_eq!(w.max_at(M + 1), Some(300), "the previous minute's max");
    // The previous window is dropped once the current is a minute old.
    assert_eq!(w.max_at(2 * M), Some(40));
}

/// A window that closes two minutes after its start or later leaves no
/// previous max (nothing came within the last minute).
#[test]
fn a_window_closed_late_leaves_no_previous_max() {
    let mut w = Window::new();
    w.record(0, 300);
    w.record(2 * M - 1, 40);
    assert_eq!(w.prev, 300, "closed within two minutes");
    let mut w = Window::new();
    w.record(0, 300);
    w.record(2 * M, 40);
    assert_eq!((w.start, w.max, w.prev), (2 * M, 40, 0));
}

fn spec() -> Spec {
    Spec {
        dante_rate: 96_000,
        block_frames: 128,
        to_asio: 1,
        from_asio: 1,
        app_buffer_ns: None,
    }
}

fn opened(rate: f64, buffer_frames: u32, to_asio: usize, from_asio: usize) -> Opened {
    Opened {
        rate,
        buffer_frames,
        driver_inputs: 2,
        driver_outputs: 2,
        inputs: (0..from_asio as u32).collect(),
        outputs: (0..to_asio as u32).collect(),
        input_sample: Some(Sample::Int32),
        output_sample: Sample::Int32,
    }
}

/// One driver callback of `frames` and one pump block (128) through the
/// bridge's own sides.
fn callback(asio: &mut impl Process, frames: usize) {
    let input = vec![0.25; frames];
    let mut output = vec![0.0; frames];
    asio.process(&mut Block::new(frames, &input, &mut output));
}

fn pump(dante: &mut impl Process) {
    let input = vec![0.0; 128];
    let mut output = vec![0.0; 128];
    dante.process(&mut Block::new(128, &input, &mut output));
}

/// dasio#120: a driver calling 128 frames at 96 kHz hands blocks
/// whose first sample is one call (1.333 ms) old; the pump taking them
/// 0.2 ms later reads 1.533 ms, and a receiver needs that + one pump block.
#[test]
fn a_128_frame_call_at_96k_is_at_least_one_call_old_at_the_pump_s_take() {
    let (t, clock) = fake();
    let mut bridge = Bridge::with_clock(spec(), clock).unwrap();
    let mut dante = bridge.dante_side().unwrap();
    let mut asio = bridge.asio_side(&opened(96_000.0, 128, 1, 1)).unwrap();
    let age = bridge.from_asio_age();
    assert_eq!(age.report(), None, "no block yet");
    pump(&mut dante);
    assert_eq!(
        age.report(),
        None,
        "a take with nothing handed records nothing"
    );
    t.store(5_000_000, Ordering::Relaxed);
    callback(&mut asio, 128);
    t.store(5_200_000, Ordering::Relaxed);
    pump(&mut dante);
    let want = CALL_128_96K + 200_000;
    assert_eq!(
        age.report(),
        Some(AgeReport {
            block_age_us: want / 1_000,
            block_age_max_us: want / 1_000,
            min_receiver_latency_us: (want + CALL_128_96K) / 1_000,
        })
    );
    assert!(age.report().unwrap().block_age_us >= 1_333);
    // The next take with no new callback changes nothing.
    t.store(5_300_000, Ordering::Relaxed);
    pump(&mut dante);
    assert_eq!(age.report().unwrap().block_age_us, want / 1_000);
    // A quick take of the next block: the current falls, the max holds.
    t.store(6_000_000, Ordering::Relaxed);
    callback(&mut asio, 128);
    t.store(6_010_000, Ordering::Relaxed);
    pump(&mut dante);
    let r = age.report().unwrap();
    assert_eq!((r.block_age_us, r.block_age_max_us), (1_343, want / 1_000));
    // Two minutes without a take: nothing to show.
    t.store(6_010_000 + 2 * M, Ordering::Relaxed);
    assert_eq!(age.report(), None);
}

/// Two callbacks before the pump's take: the older one's age counts (the
/// oldest sample the take sends).
#[test]
fn the_oldest_untaken_hand_over_is_the_one_aged() {
    let (t, clock) = fake();
    let age = BlockAge::new(CALL_128_96K, clock);
    age.handed(128, 96_000);
    t.store(1_333_333, Ordering::Relaxed);
    age.handed(128, 96_000);
    t.store(1_500_000, Ordering::Relaxed);
    age.taken();
    assert_eq!(age.report().unwrap().block_age_us, 2_833);
    // A rate of 0 reads as 1 Hz (never a division by zero).
    age.handed(1, 0);
    t.store(1_600_000, Ordering::Relaxed);
    age.taken();
    assert_eq!(age.report().unwrap().block_age_us, 1_000_100);
}

/// A bridge with nothing from ASIO measures nothing: its driver's side
/// marks no hand-over.
#[test]
fn a_bridge_with_nothing_from_asio_has_no_age() {
    let (t, clock) = fake();
    let mut s = spec();
    s.from_asio = 0;
    let mut bridge = Bridge::with_clock(s, clock).unwrap();
    let mut dante = bridge.dante_side().unwrap();
    let mut asio = bridge.asio_side(&opened(96_000.0, 128, 1, 0)).unwrap();
    callback(&mut asio, 128);
    assert_eq!(
        bridge.from_asio_age().pending.load(Ordering::Relaxed),
        NONE,
        "no hand-over marked"
    );
    t.store(1_000_000, Ordering::Relaxed);
    pump(&mut dante);
    assert_eq!(bridge.from_asio_age().report(), None);
    assert_eq!(Bridge::new(spec()).unwrap().from_asio_age().report(), None);
}

fn receivers() -> Vec<(String, u64)> {
    vec![
        ("unknown".to_string(), 0),
        ("low".to_string(), 1_000_000),
        ("exact".to_string(), 2_866_000),
        ("roomy".to_string(), 5_000_000),
    ]
}

/// The warning names each known receiver below the smallest serving
/// latency (2866 us here), never an unknown one or one at it.
#[test]
fn the_warning_names_the_known_receivers_below_the_age() {
    let report = AgeReport {
        block_age_us: 1_533,
        block_age_max_us: 1_533,
        min_receiver_latency_us: 2_866,
    };
    let text = low_receivers_text(report, &receivers()).expect("one low");
    assert!(text.contains("low at 1000 us"), "{text}");
    assert!(text.contains("at least 2866 us"), "{text}");
    assert!(text.contains("up to 1533 us"), "{text}");
    for name in ["unknown", "exact", "roomy"] {
        assert!(!text.contains(name), "{name}: {text}");
    }
    let mut two = receivers();
    two.push(("low2".to_string(), 2_865_999));
    let text = low_receivers_text(report, &two).unwrap();
    assert!(text.contains("low at 1000 us, low2 at 2865 us"), "{text}");
    assert_eq!(low_receivers_text(report, &receivers()[2..]), None);
}

/// Once an hour at most, and only while there is an age to check against.
#[test]
fn the_low_receiver_warning_comes_once_an_hour() {
    let (t, clock) = fake();
    let age = BlockAge::new(CALL_128_96K, clock);
    assert_eq!(age.warn_low_receivers(&receivers()), None, "no age yet");
    age.handed(128, 96_000);
    t.store(200_000, Ordering::Relaxed);
    age.taken();
    assert!(age.warn_low_receivers(&receivers()).is_some());
    for _ in 0..3 {
        assert_eq!(age.warn_low_receivers(&receivers()), None);
    }
    // Keep the age current past the hour.
    t.store(200_000 + HOUR_NS - 1, Ordering::Relaxed);
    age.handed(128, 96_000);
    age.taken();
    assert_eq!(age.warn_low_receivers(&receivers()), None, "a ns short");
    t.store(200_000 + HOUR_NS, Ordering::Relaxed);
    assert!(age.warn_low_receivers(&receivers()).is_some());
    assert_eq!(age.warn_low_receivers(&receivers()[2..]), None, "none low");
}
