// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use dstack_audio_io::{Block, Process};

use super::*;
use crate::bridge::{Bridge, Spec};
use crate::device::Opened;
use crate::format::Sample;
use dstack::media::tx::{AppView, ReceiverLateness};

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
        to_asio_app_buffer_ns: None,
        from_asio_app_buffer_ns: None,
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
    // The modelled block age only (a diagnostic): no TX view yet, so no
    // packet age and no receiver figure (dstack #325's field reading).
    assert_eq!(
        age.report(),
        Some(AgeReport {
            block_age_us: Some(want / 1_000),
            block_age_max_us: Some(want / 1_000),
            packet_age_us: None,
            min_receiver_latency_us: None,
        })
    );
    assert!(age.report().unwrap().block_age_us >= Some(1_333));
    // The next take with no new callback changes nothing.
    t.store(5_300_000, Ordering::Relaxed);
    pump(&mut dante);
    assert_eq!(age.report().unwrap().block_age_us, Some(want / 1_000));
    // A quick take of the next block: the current falls, the max holds.
    t.store(6_000_000, Ordering::Relaxed);
    callback(&mut asio, 128);
    t.store(6_010_000, Ordering::Relaxed);
    pump(&mut dante);
    let r = age.report().unwrap();
    assert_eq!(
        (r.block_age_us, r.block_age_max_us),
        (Some(1_343), Some(want / 1_000))
    );
    // Two minutes without a take: nothing to show.
    t.store(6_010_000 + 2 * M, Ordering::Relaxed);
    assert_eq!(age.report(), None);
}

/// Two callbacks before the pump's take: the older one's age counts (the
/// oldest sample the take sends).
#[test]
fn the_oldest_untaken_hand_over_is_the_one_aged() {
    let (t, clock) = fake();
    let age = BlockAge::new(clock);
    age.handed(128, 96_000);
    t.store(1_333_333, Ordering::Relaxed);
    age.handed(128, 96_000);
    t.store(1_500_000, Ordering::Relaxed);
    age.taken();
    assert_eq!(age.report().unwrap().block_age_us, Some(2_833));
    // A rate of 0 reads as 1 Hz (never a division by zero).
    age.handed(1, 0);
    t.store(1_600_000, Ordering::Relaxed);
    age.taken();
    assert_eq!(age.report().unwrap().block_age_us, Some(1_000_100));
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

/// The TX view's receivers: a 2 ms one and a 5 ms one, with their late
/// counts so far.
fn view(peak_ns: u64, two: u64, five: u64) -> AppView {
    let rx = |slot, name: &str, latency_ns, over_latency| ReceiverLateness {
        slot,
        receiver: name.to_string(),
        latency_ns,
        over_latency,
    };
    AppView {
        age_peak_ns: peak_ns,
        receivers: vec![
            rx(1, "dvs-two", 2_000_000, two),
            rx(4, "card-five", 5_000_000, five),
        ],
    }
}

/// dstack #325, dasio's field reading (16.11.2026): a 128-frame driver at
/// 96 kHz modelled 1.9 ms of block age, while the bridge's TX packets
/// peaked at 1.4 ms past their stamps and the 2 ms receivers counted
/// nothing late. The receiver figure is the packets' age + the network
/// (200 us while none is measured), not the model, and a 2 ms receiver
/// that counts nothing late is never warned.
#[test]
fn the_receiver_figure_is_the_tx_packet_age_not_the_modelled_block_age() {
    let (t, clock) = fake();
    let age = BlockAge::new(clock);
    age.handed(128, 96_000);
    t.store(566_667, Ordering::Relaxed);
    age.taken();
    assert_eq!(age.look(&view(1_400_000, 0, 0), None), None, "first look");
    assert_eq!(
        age.report(),
        Some(AgeReport {
            block_age_us: Some(1_900),
            block_age_max_us: Some(1_900),
            packet_age_us: Some(1_400),
            min_receiver_latency_us: Some(1_600),
        })
    );
    // A measured network replaces the 200 us allowance.
    t.store(1_000_000_000, Ordering::Relaxed);
    assert_eq!(age.look(&view(1_003_000, 0, 0), Some(350_000)), None);
    let r = age.report().unwrap();
    assert_eq!(r.packet_age_us, Some(1_400), "the minute's max");
    assert_eq!(r.min_receiver_latency_us, Some(1_750));
    // The 2 ms receiver stays clean past the hour: never warned.
    t.store(HOUR_NS + 2, Ordering::Relaxed);
    assert_eq!(age.look(&view(1_397_000, 0, 0), None), None);
    assert_eq!(age.report().unwrap().min_receiver_latency_us, Some(1_597));
}

/// The warning comes only from a receiver's own late count rising (the
/// TX side's `over_latency`), naming it and its latency, at most once an
/// hour; the first look only learns the counts, and a rise inside the hour
/// is held for the next warning, never lost.
#[test]
fn a_receiver_that_counts_packets_late_is_warned_at_most_once_an_hour() {
    let (t, clock) = fake();
    let age = BlockAge::new(clock);
    assert_eq!(age.look(&view(900_000, 5, 0), None), None, "learns 5");
    assert_eq!(age.look(&view(900_000, 5, 0), None), None, "no rise");
    let text = age.look(&view(900_000, 7, 0), None).expect("a rise");
    assert!(text.contains("dvs-two at 2000 us (2 late)"), "{text}");
    assert!(text.contains("up to 900 us old"), "{text}");
    assert!(text.contains("at least 1100 us"), "{text}");
    assert!(!text.contains("card-five"), "{text}");
    for over in 8..11 {
        assert_eq!(age.look(&view(900_000, over, 0), None), None, "{over}");
    }
    t.store(HOUR_NS - 1, Ordering::Relaxed);
    assert_eq!(age.look(&view(0, 11, 1), None), None, "a ns short");
    // The hour's rises were kept, summed: warned without a new one.
    t.store(HOUR_NS, Ordering::Relaxed);
    let text = age.look(&view(0, 11, 1), None).expect("the held rises");
    assert!(
        text.contains("dvs-two at 2000 us (4 late), card-five at 5000 us (1 late)"),
        "{text}"
    );
    // No packet age known (the window empty): the text says only who.
    assert!(!text.contains("old"), "{text}");
    assert_eq!(age.look(&view(0, 12, 3), None), None, "held again");
    t.store(2 * HOUR_NS, Ordering::Relaxed);
    let text = age.look(&view(0, 12, 3), None).expect("an hour on");
    assert!(
        text.contains("dvs-two at 2000 us (1 late), card-five at 5000 us (2 late)"),
        "{text}"
    );
    t.store(3 * HOUR_NS, Ordering::Relaxed);
    assert_eq!(age.look(&view(0, 12, 3), None), None, "nothing new");
}

/// A view without a packet records no age; the packet age lasts one to
/// two minutes like the block age, and a report stands on either.
#[test]
fn the_packet_age_needs_a_packet_and_lasts_two_minutes() {
    let (t, clock) = fake();
    let age = BlockAge::new(clock);
    age.look(&AppView::default(), Some(100_000));
    assert_eq!(age.report(), None, "no packet, no block");
    age.look(&view(1_200_000, 0, 0), Some(100_000));
    assert_eq!(
        age.report(),
        Some(AgeReport {
            block_age_us: None,
            block_age_max_us: None,
            packet_age_us: Some(1_200),
            min_receiver_latency_us: Some(1_300),
        })
    );
    t.store(2 * M - 1, Ordering::Relaxed);
    assert_eq!(age.report().unwrap().packet_age_us, Some(1_200));
    t.store(2 * M, Ordering::Relaxed);
    assert_eq!(age.report(), None);
}
