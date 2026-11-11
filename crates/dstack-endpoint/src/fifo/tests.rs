// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

use std::sync::Arc;

/// Frame `f` channel `c` of a test signal: distinct per frame and channel,
/// exact in f32.
fn value(f: usize, c: usize) -> f64 {
    (f * 10 + c) as f64 / 1024.0
}

#[test]
fn frames_come_out_in_order_channel_major_across_the_wrap() {
    let fifo = Fifo::new(2, 5);
    assert!(fifo.is_empty());
    let mut next = 0usize;
    let mut got = 0usize;
    // Push 3, pop 3, ten times: the positions wrap the 5 slots.
    for _ in 0..10 {
        let base = next;
        assert_eq!(fifo.push_with(3, |f, c| value(base + f, c)), 3);
        next += 3;
        assert_eq!(fifo.len(), 3);
        assert!(!fifo.is_empty());
        let mut out = [0.0; 6];
        assert!(fifo.pop_planar(&mut out, 3));
        for f in 0..3 {
            assert_eq!(out[f], value(got + f, 0));
            assert_eq!(out[3 + f], value(got + f, 1));
        }
        got += 3;
    }
    assert!(fifo.is_empty());
    assert_eq!((fifo.arrived(), fifo.overflows()), (30, 0));
    assert_eq!((fifo.channels(), fifo.capacity()), (2, 5));
}

#[test]
fn a_full_fifo_drops_the_newest_frames_and_counts_them() {
    let fifo = Fifo::new(1, 4);
    assert_eq!(fifo.push_with(3, value), 3);
    assert_eq!(fifo.push_with(3, |f, c| value(3 + f, c)), 1);
    assert_eq!(fifo.len(), 4);
    assert_eq!((fifo.arrived(), fifo.overflows()), (6, 2));
    let mut out = [0.0; 4];
    assert!(fifo.pop_planar(&mut out, 4));
    assert_eq!(out, [value(0, 0), value(1, 0), value(2, 0), value(3, 0)]);
    // Room again: the next push is whole.
    assert_eq!(fifo.push_with(2, |_, _| 0.5), 2);
    assert_eq!(fifo.overflows(), 2);
}

#[test]
fn a_short_fifo_or_a_short_buffer_takes_nothing() {
    let fifo = Fifo::new(2, 8);
    fifo.push_with(3, value);
    let mut out = [9.0; 8];
    assert!(!fifo.pop_planar(&mut out, 4), "3 held, 4 asked");
    assert_eq!(out, [9.0; 8], "nothing written");
    let mut small = [9.0; 5];
    assert!(!fifo.pop_planar(&mut small, 3), "room for 2.5 frames");
    assert_eq!(fifo.len(), 3, "nothing taken");
    let mut exact = [0.0; 6];
    assert!(fifo.pop_planar(&mut exact, 3), "exactly 3 held, room exact");
    assert!(fifo.pop_planar(&mut [], 0), "no frames is always there");
}

#[test]
fn skip_drops_the_oldest_up_to_what_it_holds() {
    let fifo = Fifo::new(1, 8);
    fifo.push_with(5, value);
    assert_eq!(fifo.skip(2), 2);
    let mut out = [0.0; 1];
    assert!(fifo.pop_planar(&mut out, 1));
    assert_eq!(out[0], value(2, 0));
    assert_eq!(fifo.skip(10), 2);
    assert!(fifo.is_empty());
}

#[test]
fn zero_channels_or_capacity_are_taken_as_one() {
    let fifo = Fifo::new(0, 0);
    assert_eq!((fifo.channels(), fifo.capacity()), (1, 1));
    assert_eq!(fifo.push_with(2, |_, _| 0.25), 1);
}

#[test]
fn a_writer_and_a_reader_on_two_threads_keep_every_frame_in_order() {
    let fifo = Arc::new(Fifo::new(2, 64));
    let total = 200_000usize;
    let writer = {
        let fifo = Arc::clone(&fifo);
        std::thread::spawn(move || {
            let mut next = 0;
            while next < total {
                let n = (total - next).min(7);
                // Only what fits: a test writer waits for room instead of
                // dropping, so every frame must come out.
                let room = fifo.capacity() - fifo.len();
                let n = n.min(room);
                let base = next;
                assert_eq!(fifo.push_with(n, |f, c| ((base + f) % 4096 + c) as f64), n);
                next += n;
                std::thread::yield_now();
            }
        })
    };
    let mut got = 0;
    let mut out = [0.0; 10];
    while got < total {
        let n = (total - got).min(5);
        if fifo.pop_planar(&mut out, n) {
            for f in 0..n {
                assert_eq!(out[f], ((got + f) % 4096) as f64);
                assert_eq!(out[n + f], ((got + f) % 4096 + 1) as f64);
            }
            got += n;
        } else {
            std::thread::yield_now();
        }
    }
    writer.join().unwrap();
    assert_eq!(fifo.overflows(), 0);
}

/// dstack #184: the peak covers the stored samples since the last take,
/// either sign; dropped frames and NaN are left out.
#[test]
fn the_peak_is_the_largest_magnitude_stored_since_the_last_take() {
    let fifo = Fifo::new(2, 4);
    assert_eq!(fifo.take_peak(), 0.0);
    fifo.push_with(2, |f, c| [[0.25, -0.75], [0.5, f64::NAN]][f][c]);
    fifo.push_with(1, |_, _| 0.125);
    assert_eq!(fifo.take_peak(), 0.75);
    assert_eq!(fifo.take_peak(), 0.0, "taken");
    // One frame fits (3 of 4 held): the dropped ones are not counted.
    assert_eq!(
        fifo.push_with(3, |f, _| if f == 0 { -0.25 } else { 1.0 }),
        1
    );
    assert_eq!(fifo.take_peak(), 0.25);
}
