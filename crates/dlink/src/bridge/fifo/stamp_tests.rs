// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #303: the write stamps ([`Stamp`]) on the FIFO's clock.

use super::*;

use std::sync::Arc;
use std::sync::atomic::AtomicI64;

/// A FIFO on a clock the test sets, ns.
fn clocked(channels: usize, capacity: usize) -> (Fifo, Arc<AtomicI64>) {
    let now = Arc::new(AtomicI64::new(0));
    let read = Arc::clone(&now);
    let fifo =
        Fifo::new(channels, capacity).with_clock(Arc::new(move || read.load(Ordering::Relaxed)));
    (fifo, now)
}

/// Each write is stamped on the FIFO's clock with the counts it brought;
/// none before the first write, and an empty write stamps nothing.
#[test]
fn each_write_is_stamped_on_the_fifo_s_clock() {
    let (fifo, now) = clocked(1, 4);
    assert_eq!(fifo.last_write(), None);
    now.store(1_000, Ordering::Relaxed);
    assert_eq!(fifo.now_ns(), 1_000);
    assert_eq!(fifo.push_with(0, |_, _| 0.0), 0);
    assert_eq!(fifo.last_write(), None, "an empty write is no write");
    fifo.push_with(3, |_, _| 0.5);
    assert_eq!(
        fifo.last_write(),
        Some(Stamp {
            at_ns: 1_000,
            written: 3,
            arrived: 3,
            writes: 1
        })
    );
    now.store(2_500, Ordering::Relaxed);
    // One of 3 fits: the stored count and the offered one part.
    fifo.push_with(3, |_, _| 0.5);
    let stamp = fifo.last_write().unwrap();
    assert_eq!(
        stamp,
        Stamp {
            at_ns: 2_500,
            written: 4,
            arrived: 6,
            writes: 2
        }
    );
    assert_eq!(stamp.mean_write(), 3.0);
    let mut out = [0.0; 2];
    assert!(fifo.pop_planar(&mut out, 2));
    assert_eq!(fifo.read_frames(), 2);
    assert_eq!(fifo.skip(1), 1);
    assert_eq!(fifo.read_frames(), 3);
}

/// A reader that meets a write in progress (the sequence word odd) gets
/// no stamp rather than a torn one.
#[test]
fn a_stamp_in_progress_is_not_read() {
    let (fifo, _) = clocked(1, 8);
    fifo.push_with(2, |_, _| 0.5);
    assert!(fifo.last_write().is_some());
    fifo.hold_stamp(true);
    assert_eq!(fifo.last_write(), None);
    fifo.hold_stamp(false);
    assert_eq!(fifo.last_write().map(|s| s.written), Some(2));
}

/// A stamp's mean write on no writes is 0, not a division by zero.
#[test]
fn a_stamp_of_no_writes_has_a_mean_of_zero() {
    let stamp = Stamp {
        at_ns: 0,
        written: 0,
        arrived: 0,
        writes: 0,
    };
    assert_eq!(stamp.mean_write(), 0.0);
}

/// Without a clock of its own the FIFO stamps on the app's clock.
#[test]
fn the_default_clock_is_the_app_s() {
    let fifo = Fifo::new(1, 4);
    let before = dstack::clock::app_monotonic_ns();
    fifo.push_with(1, |_, _| 0.5);
    let at = fifo.last_write().unwrap().at_ns;
    let after = dstack::clock::app_monotonic_ns();
    assert!((before..=after).contains(&at), "{before} {at} {after}");
    let now = fifo.now_ns();
    assert!(now >= after, "{now} {after}");
}

/// A writer thread and a reader: every stamp read is whole, never mixed
/// from two writes (the writer's clock is its own count).
#[test]
fn a_stamp_read_across_threads_is_never_torn() {
    let count = Arc::new(AtomicI64::new(0));
    let read = Arc::clone(&count);
    let fifo =
        Arc::new(Fifo::new(1, 1 << 20).with_clock(Arc::new(move || read.load(Ordering::Relaxed))));
    let total = 100_000u64;
    let writer = {
        let fifo = Arc::clone(&fifo);
        let count = Arc::clone(&count);
        std::thread::spawn(move || {
            for k in 1..=total {
                count.store(k as i64 * 3, Ordering::Relaxed);
                fifo.push_with(3, |_, _| 0.5);
            }
        })
    };
    let mut seen = 0;
    while seen < total {
        if let Some(s) = fifo.last_write() {
            assert_eq!(
                (s.at_ns as u64, s.arrived, s.writes * 3),
                (s.written, s.written, s.written),
                "{s:?}"
            );
            seen = s.writes;
        }
        std::hint::spin_loop();
    }
    writer.join().unwrap();
}
