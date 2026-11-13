// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

use super::*;

#[test]
fn a_packet_s_network_latency_is_its_arrival_less_its_stamp() {
    // The pass at 10 ms read a packet the kernel received 0.3 ms earlier,
    // stamped at 8.5 ms: it took 1.2 ms on the network.
    assert_eq!(
        network_latency_ns(10_000_000, 300_000, 8_500_000),
        1_200_000
    );
    assert_eq!(network_latency_ns(10_000_000, 0, 8_500_000), 1_500_000);
    // Stamped after it arrived (an early sender): 0, never a wrap.
    assert_eq!(network_latency_ns(10_000_000, 300_000, 9_800_000), 0);
    assert_eq!(network_latency_ns(1_000, 5_000, 0), 0);
}

#[test]
fn a_latency_falls_in_its_20_us_bucket_or_the_one_past_them() {
    assert_eq!(BUCKET_NS, 20_000);
    assert_eq!(BUCKETS, 256);
    assert_eq!(bucket(0), 0);
    assert_eq!(bucket(19_999), 0);
    assert_eq!(bucket(20_000), 1);
    assert_eq!(bucket(1_000_000), 50);
    assert_eq!(bucket(5_119_999), 255);
    assert_eq!(bucket(5_120_000), BUCKETS);
    assert_eq!(bucket(u64::MAX), BUCKETS);
}

fn counts(of: &[(usize, u32)]) -> [u32; BUCKETS + 1] {
    let mut c = [0; BUCKETS + 1];
    for &(b, n) in of {
        c[b] = n;
    }
    c
}

#[test]
fn p99_is_the_upper_edge_of_the_bucket_at_99_percent_within_min_and_max() {
    assert_eq!(p99(&counts(&[]), 0, 0), None);
    // 99 packets at 1.0x ms and one at 3 ms: rank 99 is in bucket 50.
    let c = counts(&[(50, 99), (150, 1)]);
    assert_eq!(p99(&c, 1_000_000, 3_000_000), Some(1_020_000));
    // 98 and 2: rank 99 is the slow ones' bucket, its edge 3.02 ms held
    // to the exact max.
    let c = counts(&[(50, 98), (150, 2)]);
    assert_eq!(p99(&c, 1_000_000, 3_010_000), Some(3_010_000));
    assert_eq!(p99(&c, 1_000_000, 3_019_999), Some(3_019_999));
    // Rounded up: 101 packets, rank 100.
    let c = counts(&[(10, 100), (20, 1)]);
    assert_eq!(p99(&c, 200_000, 400_000), Some(220_000));
    let c = counts(&[(10, 99), (20, 2)]);
    assert_eq!(p99(&c, 200_000, 410_000), Some(410_000));
    // One packet: its bucket's edge, clamped to the exact max.
    assert_eq!(
        p99(&counts(&[(50, 1)]), 1_005_000, 1_005_000),
        Some(1_005_000)
    );
    // Within min: a min above the edge (a racing take) is kept.
    assert_eq!(p99(&counts(&[(0, 1)]), 30_000, 40_000), Some(30_000));
    // Past the buckets: the exact max.
    let c = counts(&[(0, 1), (BUCKETS, 1)]);
    assert_eq!(p99(&c, 1_000, 9_000_000), Some(9_000_000));
    // A max under min (a racing take) never panics.
    assert_eq!(p99(&counts(&[(0, 1)]), 30_000, 10_000), Some(30_000));
}

#[test]
fn a_window_takes_min_p99_and_max_and_starts_again_empty() {
    let w = Window::default();
    assert_eq!(w.last(), None);
    w.take();
    assert_eq!(w.last(), None);
    for _ in 0..99 {
        w.record(400_000);
    }
    w.record(2_500_000);
    w.record(350_000);
    w.take();
    assert_eq!(
        w.last(),
        Some(NetLatency {
            min_us: 350,
            p99_us: 420,
            max_us: 2_500,
        })
    );
    // The next window is its own: one packet at 5.2 ms (past the buckets).
    w.record(5_200_000);
    w.take();
    assert_eq!(
        w.last(),
        Some(NetLatency {
            min_us: 5_200,
            p99_us: 5_200,
            max_us: 5_200,
        })
    );
    // A second without a packet has none.
    w.take();
    assert_eq!(w.last(), None);
}

/// dstack #244 (review): a window's three figures go in one word, so a
/// status read never mixes two windows; each is µs, at most 2.1 s.
#[test]
fn the_three_figures_pack_into_one_word_and_back() {
    let n = NetLatency::unpack(NetLatency::pack(350_999, 420_000, 2_500_001));
    assert_eq!(
        n,
        NetLatency {
            min_us: 350,
            p99_us: 420,
            max_us: 2_500,
        }
    );
    assert_eq!(NetLatency::pack(1_000, 0, 0), 1);
    assert_eq!(NetLatency::pack(0, 1_000, 0), 1 << 21);
    assert_eq!(NetLatency::pack(0, 0, 1_000), 1 << 42);
    // Past 2.1 s each is held at MAX_US, never into its neighbour.
    assert_eq!(MAX_US, 2_097_151);
    let big = NetLatency::unpack(NetLatency::pack(u64::MAX, u64::MAX, u64::MAX));
    assert_eq!(
        (big.min_us, big.p99_us, big.max_us),
        (MAX_US, MAX_US, MAX_US)
    );
    assert_ne!(NetLatency::pack(u64::MAX, u64::MAX, u64::MAX), NONE);
    let lone = NetLatency::unpack(NetLatency::pack(0, u64::MAX, 0));
    assert_eq!((lone.min_us, lone.p99_us, lone.max_us), (0, MAX_US, 0));
}
