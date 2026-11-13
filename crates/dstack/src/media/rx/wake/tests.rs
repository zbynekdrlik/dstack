use super::*;

const US: u64 = 1_000;

#[test]
fn a_wake_delay_falls_in_its_50_us_bucket_or_the_one_past_them() {
    assert_eq!(bucket(0), 0);
    assert_eq!(bucket(BUCKET_NS - 1), 0);
    assert_eq!(bucket(BUCKET_NS), 1);
    assert_eq!(bucket(BUCKETS as u64 * BUCKET_NS - 1), BUCKETS - 1);
    assert_eq!(bucket(BUCKETS as u64 * BUCKET_NS), BUCKETS);
    assert_eq!(bucket(u64::MAX), BUCKETS);
    const _: () = assert!(BUCKETS as u64 * BUCKET_NS == 25_600_000);
}

fn counts(of: &[(usize, u32)]) -> [u32; BUCKETS + 1] {
    let mut c = [0u32; BUCKETS + 1];
    for &(b, n) in of {
        c[b] += n;
    }
    c
}

#[test]
fn the_figures_are_the_upper_edges_of_the_buckets_at_each_rank() {
    assert_eq!(WakeDelay::of(&counts(&[]), 0), None);
    // 1 000 packets: 500 in bucket 0 (0-50 us), 490 in bucket 10, 9 in
    // bucket 100, 1 at 15.5 ms. Rank 500 is bucket 0, rank 990 bucket 10,
    // rank 999 bucket 100.
    let c = counts(&[(0, 500), (10, 490), (100, 9), (310, 1)]);
    assert_eq!(
        WakeDelay::of(&c, 15_500 * US),
        Some(WakeDelay {
            packets: 1_000,
            p50_us: 50,
            p99_us: 550,
            p999_us: 5_050,
            max_us: 15_500,
        })
    );
    // Ranks round up: of 1 001 packets, 501, 991 and 1 000 (a floor would
    // take 500, 990 and 999: bucket 0, bucket 10 and bucket 100).
    let c = counts(&[(0, 500), (10, 491), (100, 8), (310, 2)]);
    let w = WakeDelay::of(&c, 15_520 * US).unwrap();
    assert_eq!((w.p50_us, w.p99_us, w.p999_us), (550, 550, 15_520));
    // A bucket's edge is never past the largest; past the buckets reads it.
    let w = WakeDelay::of(&counts(&[(4, 1)]), 230 * US).unwrap();
    assert_eq!(
        (w.p50_us, w.p99_us, w.p999_us, w.max_us),
        (230, 230, 230, 230)
    );
    let w = WakeDelay::of(&counts(&[(BUCKETS, 2)]), 40_000 * US).unwrap();
    assert_eq!((w.packets, w.p50_us, w.p999_us), (2, 40_000, 40_000));
}

#[test]
fn a_window_takes_its_figures_keeps_them_and_starts_again_empty() {
    let w = Window::default();
    assert_eq!(w.last(), None, "before the first take");
    for ns in [10 * US, 20 * US, 120 * US, 7_000 * US] {
        w.record(ns);
    }
    let taken = w.take().unwrap();
    assert_eq!(
        taken,
        WakeDelay {
            packets: 4,
            p50_us: 50,
            p99_us: 7_000,
            p999_us: 7_000,
            max_us: 7_000,
        }
    );
    assert_eq!(w.last(), Some([50, 7_000, 7_000]));
    assert_eq!(w.last(), Some([50, 7_000, 7_000]), "a read takes nothing");
    // The next minute holds only its own packets.
    w.record(300 * US);
    let taken = w.take().unwrap();
    assert_eq!((taken.packets, taken.p50_us, taken.max_us), (1, 300, 300));
    assert_eq!(w.last(), Some([300, 300, 300]));
    // A minute without packets: none, and the status shows none.
    assert_eq!(w.take(), None);
    assert_eq!(w.last(), None);
}

/// dstack #234: a window keeps its last largest for the status (the write
/// delay's), none before the first take and after an empty window.
#[test]
fn a_window_keeps_its_last_largest() {
    let w = Window::default();
    assert_eq!(w.last_max_us(), None, "before the first take");
    w.record(120 * US);
    w.record(9_000 * US);
    w.take();
    assert_eq!(w.last_max_us(), Some(9_000));
    w.take();
    assert_eq!(w.last_max_us(), None, "an empty window");
}
