// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

const S: u64 = 1_000_000_000;

fn cpu(process: u64, busy: u64, total: u64) -> Option<CpuTimes> {
    Some(CpuTimes {
        process,
        busy,
        total,
    })
}

/// A recorder fed `n` seconds from 0: the NIC sends 1000 B a second more
/// each second, we send 300, the CPU's total moves 400 a second, its busy
/// 100, this process 20.
fn fed(n: u64) -> Recorder {
    let mut r = Recorder::new();
    for i in 0..=n {
        r.record(
            i * S,
            || Some(i * 1000),
            i * 300,
            || cpu(i * 20, i * 100, i * 400),
        );
    }
    r
}

#[test]
fn the_first_sample_only_starts_the_record() {
    let mut r = Recorder::new();
    assert!(r.due(0));
    r.record(5 * S, || Some(100), 10, || cpu(1, 2, 3));
    assert_eq!(r.seconds().count(), 0);
    assert_eq!(r.report(), "none recorded yet");
}

#[test]
fn a_second_gives_rates_other_traffic_and_cpu() {
    let mut r = Recorder::new();
    r.set_neighbours(200);
    r.record(10 * S, || Some(1_000), 100, || cpu(10, 100, 1_000));
    // 2 s later: the NIC sent 9000 B, we 1000 B (4500 and 500 a second);
    // the CPU's total moved 800, its busy 200 (25 %), this process 40 (5 %).
    r.record(12 * S, || Some(10_000), 1_100, || cpu(50, 300, 1_800));
    let s: Vec<Second> = r.seconds().copied().collect();
    assert_eq!(
        s,
        vec![Second {
            at_ns: 12 * S,
            nic: Some(4_500),
            ours: 500,
            other: Some(4_500 - 500 - 200),
            process_cpu: Some(5),
            system_cpu: Some(25),
        }]
    );
}

#[test]
fn a_sample_is_read_only_once_a_second() {
    let mut r = Recorder::new();
    r.record(S, || Some(0), 0, || cpu(0, 0, 0));
    assert!(!r.due(S + S - 1));
    // Not due: neither counter is read, nothing added.
    r.record(
        S + S - 1,
        || panic!("the NIC read before a second passed"),
        5,
        || panic!("the CPU read before a second passed"),
    );
    assert_eq!(r.seconds().count(), 0);
    assert!(r.due(2 * S));
    r.record(2 * S, || Some(50), 5, || cpu(0, 0, 10));
    assert_eq!(r.seconds().count(), 1);
}

#[test]
fn the_ring_keeps_the_last_thirty_seconds() {
    let r = fed(SECONDS as u64);
    assert_eq!(r.seconds().count(), SECONDS);
    let r = fed(SECONDS as u64 + 5);
    let ats: Vec<u64> = r.seconds().map(|s| s.at_ns).collect();
    assert_eq!(ats.len(), SECONDS);
    // The oldest five dropped: the first kept second ended at 6 s.
    assert_eq!(ats[0], 6 * S);
    assert_eq!(ats[SECONDS - 1], (SECONDS as u64 + 5) * S);
    assert!(r.seconds().all(|s| s.nic == Some(1000) && s.ours == 300));
    assert!(r.seconds().all(|s| s.other == Some(700)));
    assert!(
        r.seconds()
            .all(|s| s.system_cpu == Some(25) && s.process_cpu == Some(5))
    );
}

#[test]
fn a_counter_that_goes_back_or_is_unread_gives_unread_values() {
    let mut r = Recorder::new();
    r.record(S, || Some(5_000), 0, || cpu(10, 10, 100));
    // The NIC's counter went back (a reset); the CPU's total too.
    r.record(2 * S, || Some(1_000), 0, || cpu(20, 20, 50));
    // The NIC unread; the CPU unread.
    r.record(3 * S, || None, 0, || None);
    // From an unread NIC to a read one: no rate yet.
    r.record(4 * S, || Some(9_000), 0, || cpu(30, 30, 300));
    for s in r.seconds() {
        assert_eq!((s.nic, s.other), (None, None), "{s:?}");
        assert_eq!((s.process_cpu, s.system_cpu), (None, None), "{s:?}");
    }
    assert_eq!(r.seconds().count(), 3);
    r.record(5 * S, || Some(9_100), 0, || cpu(40, 130, 400));
    let last = r.seconds().last().copied().unwrap();
    assert_eq!((last.nic, last.other), (Some(100), Some(100)));
    assert_eq!((last.process_cpu, last.system_cpu), (Some(10), Some(100)));
}

#[test]
fn other_traffic_is_the_nic_less_our_devices_saturating() {
    assert_eq!(other_of(1_000, 300, 200), 500);
    assert_eq!(other_of(1_000, 600, 400), 0);
    assert_eq!(other_of(1_000, 900, 200), 0);
    assert_eq!(other_of(1_000, u64::MAX, 1), 0);
    assert_eq!(other_of(u64::MAX, 0, 0), u64::MAX);
}

#[test]
fn rates_are_per_second_and_saturate() {
    assert_eq!(per_second(3_000, 2 * S), 1_500);
    assert_eq!(per_second(1_000, S / 2), 2_000);
    assert_eq!(per_second(7, 0), 7_000_000_000);
    assert_eq!(per_second(u64::MAX, 1), u64::MAX);
    assert_eq!(per_second(u64::MAX, S), u64::MAX);
}

#[test]
fn the_cpu_share_is_of_the_whole_machine() {
    let a = CpuTimes {
        process: 100,
        busy: 1_000,
        total: 4_000,
    };
    let b = CpuTimes {
        process: 150,
        busy: 1_300,
        total: 5_000,
    };
    assert_eq!(cpu_percent(a, b), (Some(5), Some(30)));
    // No time passed, or the total went back: neither.
    assert_eq!(cpu_percent(a, a), (None, None));
    assert_eq!(cpu_percent(b, a), (None, None));
    // One counter went back alone: that one only.
    let c = CpuTimes { process: 90, ..b };
    assert_eq!(cpu_percent(a, c), (None, Some(30)));
    let d = CpuTimes { busy: 900, ..b };
    assert_eq!(cpu_percent(a, d), (Some(5), None));
    // Huge counts saturate rather than wrap.
    let e = CpuTimes {
        process: u64::MAX,
        busy: 0,
        total: 1,
    };
    let z = CpuTimes::default();
    assert_eq!(cpu_percent(z, e), (Some(u64::MAX), Some(0)));
}

#[test]
fn a_burst_is_dumped_once_and_a_quiet_ten_seconds_ends_it() {
    let mut r = Recorder::new();
    // No refusal: never.
    assert!(!r.refused(0, false));
    // The first refusal dumps; the burst's next ones, each within 10 s of
    // the one before, do not, even past 10 s from its start.
    assert!(r.refused(5 * S, true));
    assert!(!r.refused(5 * S, true));
    assert!(!r.refused(14 * S, true));
    assert!(!r.refused(23 * S, true));
    // A tick without a refusal keeps the burst's last time.
    assert!(!r.refused(30 * S, false));
    // 10 s (to the ns) after the last refusal: a new burst.
    assert!(!r.refused(33 * S - 1, true));
    assert!(r.refused(33 * S - 1 + BURST_QUIET_NS, true));
    // Three refusals 1 s apart after it: still one dump.
    let at = 33 * S - 1 + BURST_QUIET_NS;
    assert!(!r.refused(at + S, true));
    assert!(!r.refused(at + 2 * S, true));
}

#[test]
fn the_report_lists_each_second_oldest_first_with_its_age() {
    let mut r = Recorder::new();
    r.set_neighbours(100);
    r.record(0, || Some(0), 0, || cpu(0, 0, 0));
    r.record(S, || Some(5_000), 1_000, || cpu(10, 50, 100));
    r.record(2 * S, || None, 2_000, || None);
    r.record(3 * S + S / 2, || Some(8_000), 2_500, || cpu(20, 60, 300));
    assert_eq!(
        r.report(),
        "-2s nic=5000 ours=1000 other=3900 cpu=10%/50%; \
         -1s nic=? ours=1000 other=? cpu=?%/?%; \
         -0s nic=? ours=333 other=? cpu=?%/?%"
    );
}

#[test]
fn a_closed_window_sets_the_neighbours_and_none_keeps_them() {
    let mut r = Recorder::new();
    let t = super::super::Traffic {
        other: 9,
        ours: 1_000,
        devices: vec![("a".into(), 300), ("b".into(), 200)],
    };
    r.after_window(Some(&t));
    assert_eq!(r.neighbours(), 500);
    r.after_window(None);
    assert_eq!(r.neighbours(), 500);
    let alone = super::super::Traffic {
        ours: 7,
        ..Default::default()
    };
    r.after_window(Some(&alone));
    assert_eq!(r.neighbours(), 0);
}
