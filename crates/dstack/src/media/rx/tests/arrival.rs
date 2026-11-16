// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The age the heartbeat, the judge and the mean report is the packet's
//! age at its ARRIVAL, not at the read (dstack #350): a consumer that
//! pulls every 10 ms (dlisten's output, an ASIO switch), the pump and the
//! RX thread all read a packet some time after it came, and that wait is
//! no network latency.

use super::*;

/// Ten years of a grandmaster's uptime, as a position: a whole number of
/// ms at 96 kHz, so every age below is exact in ns.
const P0: u64 = 10 * 365 * 86_400 * RATE as u64;
/// The previous read, 10 ms before [`NOW`].
const T1: u64 = P0;
/// The read under test.
const NOW: u64 = T1 + 960;
/// Half a ms at 96 kHz: how long after its stamp each packet arrived.
const HALF_MS: u64 = 48;
/// One ms of frames: each packet's length.
const FRAMES: usize = 96;

/// Slot 0 with a flow from loopback feeding RX channel 0, its playout
/// 20 ms (none of these packets is late under any writer).
fn flow_in_slot_0(s: &RxShared) {
    let mut p = plan(LOOPBACK, 1, true, 1);
    p.latency = 1920;
    publish(s, &[(0, p)], &[(0, RxRoute { flow: 0, slot: 0 })]);
}

/// A read at `T1` of one packet that came 0.5 ms after its stamp, then a
/// read at `NOW` of the ten 1 ms packets that came between the two reads,
/// each 0.5 ms after its stamp (the last one 0.5 ms before `NOW`). Every
/// packet's network age is 0.5 ms; at the read the oldest of the ten has
/// waited 9 ms more. `read(now, n)` reads what waits at PTP time `now`,
/// `n` the datagrams slot 0 has read once it is done.
fn check_arrival(s: &RxShared, read: &mut dyn FnMut(u64, u64)) {
    // As on Windows: no kernel receive stamp; the bracket of two reads.
    s.set_stamps(false);
    let t = transmitter(LOOPBACK);
    t.send_to(&packet_at(T1 - HALF_MS, 1, FRAMES), slot_addr(s, 0))
        .unwrap();
    settle();
    read(ns_of(T1, RATE), 1);
    // The first read has no earlier one to bracket with: its age at the
    // read, which is its arrival age here.
    assert_eq!(s.take_max_lateness(0), HALF_MS);
    for k in 1..=10 {
        let pos = T1 - HALF_MS + k * FRAMES as u64;
        t.send_to(&packet_at(pos, 1, FRAMES), slot_addr(s, 0))
            .unwrap();
    }
    settle();
    read(ns_of(NOW, RATE), 11);
    let c = s.lane(0);
    assert_eq!((c.packets, c.late), (11, 0), "{c:?}");
    // 0x8003's second: the arrival's lateness, not + the wait.
    assert_eq!(s.take_max_lateness(0), HALF_MS, "{c:?}");
    assert_eq!(s.last_lateness_mean_ns(0), Some(MS / 2), "{c:?}");
    assert_eq!(s.last_min_lateness_ns(0), Some(MS / 2), "{c:?}");
    assert_eq!(s.lane(0).lateness_peak, HALF_MS);
}

#[test]
fn an_app_s_pull_10_ms_apart_reports_each_packet_s_arrival_age() {
    let s = shared(1);
    let mut intake = s.app_intake().unwrap();
    flow_in_slot_0(&s);
    intake.drain_into_rings(None);
    check_arrival(&s, &mut |now, _| {
        intake.drain_into_rings(Some(now));
    });
}

#[test]
fn the_pump_s_drain_reports_each_packet_s_arrival_age() {
    let s = shared(1);
    let mut intake = s.pump_intake().unwrap();
    flow_in_slot_0(&s);
    intake.drain_into_rings(None);
    check_arrival(&s, &mut |now, _| {
        intake.drain_into_rings(Some(now));
    });
}

#[test]
fn the_rx_thread_s_pass_reports_each_packet_s_arrival_age() {
    let s = shared(1);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    flow_in_slot_0(&s);
    check_arrival(&s, &mut |now, n| pump(&mut rx, 0, n, Some(now), 0));
}

/// With the kernel's receive stamps (Linux) the arrival is the stamp's: a
/// packet 0.5 ms old when sent and read 10 ms later reports about 0.5 ms
/// (real time: the bound leaves room for a late send), never the 10.5 ms
/// it had at the read.
#[cfg(target_os = "linux")]
#[test]
fn with_kernel_stamps_a_pulled_packet_reports_its_age_at_the_stamp() {
    let s = shared(1);
    assert!(s.stamps());
    let mut intake = s.app_intake().unwrap();
    flow_in_slot_0(&s);
    let start = Instant::now();
    let base = ns_of(P0, RATE);
    let now = || base + u64::try_from(start.elapsed().as_nanos()).unwrap();
    intake.drain_into_rings(Some(now()));
    let t = transmitter(LOOPBACK);
    let pos = clock::to_samples(now(), RATE) - HALF_MS;
    t.send_to(&packet_at(pos, 1, FRAMES), slot_addr(&s, 0))
        .unwrap();
    std::thread::sleep(Duration::from_millis(10));
    intake.drain_into_rings(Some(now()));
    assert_eq!(s.lane(0).packets, 1);
    s.take_max_lateness(0);
    let mean = s.last_lateness_mean_ns(0).unwrap();
    assert!((MS / 2 - 20_000..3 * MS).contains(&mean), "mean {mean} ns");
}
