// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #319: the hosts refreshing Dante records on the 96 s cycle.

use super::*;

fn a(last: u8) -> Ipv4Addr {
    Ipv4Addr::new(192, 0, 2, last)
}

fn name(out: &mut Vec<u8>, dotted: &str) {
    for label in dotted.trim_end_matches('.').split('.') {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
}

/// A message of `questions` (name, type) with the flags word `flags` and
/// `authority` records (each an A of the first question's name).
fn message(flags: u16, questions: &[(&str, u16)], authority: u16) -> Vec<u8> {
    let mut out = vec![0, 0];
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(&(questions.len() as u16).to_be_bytes());
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&authority.to_be_bytes());
    out.extend_from_slice(&[0, 0]);
    for (q, ty) in questions {
        name(&mut out, q);
        out.extend_from_slice(&ty.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
    }
    for _ in 0..authority {
        name(&mut out, questions[0].0);
        out.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 120, 0, 4, 192, 0, 2, 1]);
    }
    out
}

fn query(questions: &[(&str, u16)]) -> Vec<u8> {
    message(0, questions, 0)
}

const ARC: &str = "_netaudio-arc._udp.local.";
const SRV: u16 = 33;
const PTR: u16 = 12;

#[test]
fn a_query_under_a_netaudio_type_is_one_and_nothing_else_is() {
    assert!(netaudio_query(&query(&[(ARC, PTR)])));
    assert!(netaudio_query(&query(&[(
        "foh-a._netaudio-arc._udp.local.",
        SRV
    )])));
    // Any label, any case, among other questions.
    assert!(netaudio_query(&query(&[
        ("songs.local.", 1),
        ("x._NetAudio-CHAN._udp.local.", SRV)
    ])));
    assert!(!netaudio_query(&query(&[("songs.local.", 1)])));
    assert!(!netaudio_query(&query(&[("_netaudio._udp.local.", PTR)])));
    assert!(!netaudio_query(&query(&[(
        "x_netaudio-arc._udp.local.",
        PTR
    )])));
    // An answer, a probe (an authority section) and no DNS at all.
    assert!(!netaudio_query(&message(0x8400, &[(ARC, PTR)], 0)));
    assert!(!netaudio_query(&message(0, &[(ARC, PTR)], 1)));
    assert!(!netaudio_query(b"x"));
}

const S: u64 = 1_000_000_000;

fn at(seconds: &[f64]) -> Vec<u64> {
    seconds.iter().map(|s| (s * 1e9).round() as u64).collect()
}

#[test]
fn three_cycles_of_ninety_six_seconds_end_a_chain() {
    let c = |t: &[f64]| cycle(&at(t), CYCLE_NS, TOLERANCE_NS);
    assert_eq!(c(&[10.0, 106.0, 202.0, 298.0]), Some(10 * S));
    // Each interval may be 2 s off, both ends included.
    assert_eq!(c(&[0.0, 94.0, 192.0, 286.0]), Some(0));
    assert_eq!(c(&[0.0, 98.0, 194.0, 292.0]), Some(0));
    assert_eq!(c(&[0.0, 93.999, 189.999, 285.999]), None);
    assert_eq!(c(&[0.0, 96.0, 194.001, 290.001]), None);
    // Two cycles are not enough.
    assert_eq!(c(&[0.0, 96.0, 192.0]), None);
    assert_eq!(c(&[]), None);
    // Other queries between the refreshes do not hide them.
    assert_eq!(
        c(&[0.0, 30.0, 96.0, 140.0, 150.0, 192.0, 250.0, 288.0]),
        Some(0)
    );
    // The chain ends at the last query.
    assert_eq!(c(&[0.0, 96.0, 192.0, 288.0, 300.0]), None);
    // A later refresh of the same chain is seen at once.
    assert_eq!(c(&[0.0, 96.0, 192.0, 288.0, 384.0]), Some(96 * S));
}

#[test]
fn a_host_is_named_with_its_measured_cycle_and_warned_once_an_hour() {
    let mut b = Browsers::default();
    let t = |s: f64| (s * 1e9).round() as u64;
    for s in [0.0, 96.2, 192.4] {
        b.query(a(7), t(s));
    }
    assert_eq!(b.take(t(200.0)), vec![], "two cycles");
    b.query(a(7), t(288.6));
    b.query(a(9), t(100.0));
    let named = b.take(t(290.0));
    assert_eq!(
        named,
        vec![Browser {
            source: a(7),
            period_ns: t(96.2),
            warn: true
        }]
    );
    // Listed again, not warned again within the hour.
    let again = b.take(t(300.0));
    assert_eq!(again.len(), 1);
    assert!(!again[0].warn);
    // Still refreshing an hour after the warning: warned again.
    let mut s = 288.6;
    while s < 290.0 + 3600.0 {
        s += 96.0;
        b.query(a(7), t(s));
        let listed = b.take(t(s));
        assert_eq!(listed.len(), 1, "at {s}");
        assert_eq!(listed[0].warn, s >= 290.0 + 3600.0, "at {s}");
    }
    let warned_at = t(s);
    assert!(b.take(warned_at + S).iter().all(|x| !x.warn));
    // The hour to the ns: not a ns before, at the hour exactly.
    let mut q = warned_at;
    while q + CYCLE_NS < warned_at + WARN_EVERY_NS {
        q += CYCLE_NS;
        b.query(a(7), q);
    }
    let before = b.take(warned_at + WARN_EVERY_NS - 1);
    assert_eq!(before.len(), 1);
    assert!(!before[0].warn);
    let at_hour = b.take(warned_at + WARN_EVERY_NS);
    assert_eq!(at_hour.len(), 1);
    assert!(at_hour[0].warn);
}

#[test]
fn a_host_querying_every_few_seconds_is_no_browser_on_the_cycle() {
    let c = |t: &[u64]| cycle(t, CYCLE_NS, TOLERANCE_NS);
    // Every 4 s for 400 s: many times are 96 s apart, none after a quiet
    // time.
    let dense: Vec<u64> = (0..=100).map(|k| 4 * k * S).collect();
    assert_eq!(c(&dense), None);
    // Each point needs 12 s (96 / 8) of quiet before it, exactly.
    let quiet = |gap: u64| {
        let mut v = Vec::new();
        for k in 0..4 {
            v.push(k * CYCLE_NS + 20 * S - gap);
            v.push(k * CYCLE_NS + 20 * S);
        }
        v
    };
    assert_eq!(c(&quiet(12 * S)), Some(20 * S));
    assert_eq!(c(&quiet(12 * S - 1)), None);
    // The first time kept counts as a refresh.
    assert_eq!(c(&[0, CYCLE_NS, 2 * CYCLE_NS, 3 * CYCLE_NS]), Some(0));
}

#[test]
fn a_host_drops_off_the_list_two_cycles_after_its_last_refresh() {
    let mut b = Browsers::default();
    for k in 0..4 {
        b.query(a(7), k * CYCLE_NS);
    }
    let last = 3 * CYCLE_NS;
    assert_eq!(b.take(last + 2 * CYCLE_NS).len(), 1);
    assert_eq!(b.take(last + 2 * CYCLE_NS + 1), vec![]);
    // Forgotten once nothing is heard for a whole window: 4 * (96 + 2) s.
    let window = 4 * (CYCLE_NS + TOLERANCE_NS);
    b.take(last + window);
    assert_eq!(b.hosts(), 1);
    b.take(last + window + 1);
    assert_eq!(b.hosts(), 0);
}

#[test]
fn a_query_within_a_second_of_the_last_is_the_same_refresh() {
    let mut b = Browsers::default();
    b.query(a(1), 10 * S);
    b.query(a(1), 10 * S + S - 1);
    b.query(a(1), 11 * S);
    assert_eq!(b.times_of(a(1)), vec![10 * S, 11 * S]);
}

#[test]
fn times_older_than_the_window_leave_and_at_most_max_times_stay() {
    let window = 4 * (CYCLE_NS + TOLERANCE_NS);
    let mut b = Browsers::default();
    b.query(a(1), 0);
    b.query(a(1), window);
    assert_eq!(b.times_of(a(1)), vec![0, window]);
    b.query(a(1), window + 1 + S);
    assert_eq!(b.times_of(a(1)), vec![window, window + 1 + S]);
    // A long cycle's window holds more than MAX_TIMES at one a second.
    let mut long = Browsers::with_cycle(200 * S, 2 * S, S);
    for k in 0..=MAX_TIMES as u64 {
        long.query(a(2), k * S);
    }
    let kept = long.times_of(a(2));
    assert_eq!(kept.len(), MAX_TIMES);
    assert_eq!(kept[0], S);
}

#[test]
fn past_the_most_hosts_the_one_heard_longest_ago_leaves() {
    let mut b = Browsers::default();
    for i in 0..MAX_HOSTS as u32 {
        b.query(Ipv4Addr::from(0x0a00_0000 + i), u64::from(i) * S + S);
    }
    assert_eq!(b.hosts(), MAX_HOSTS);
    // A known host is no new one.
    b.query(Ipv4Addr::from(0x0a00_0000), 300 * S);
    assert_eq!(b.hosts(), MAX_HOSTS);
    b.query(a(1), 301 * S);
    assert_eq!(b.hosts(), MAX_HOSTS);
    // 10.0.0.0 was heard again; 10.0.0.1 is the oldest now.
    assert!(b.times_of(Ipv4Addr::from(0x0a00_0001)).is_empty());
    assert_eq!(b.times_of(Ipv4Addr::from(0x0a00_0000)).len(), 2);
    assert_eq!(b.times_of(a(1)), vec![301 * S]);
}

#[test]
fn only_a_dante_query_from_port_5353_counts() {
    let mut b = Browsers::default();
    let q = query(&[(ARC, PTR)]);
    b.heard(a(1), 5353, &q, S);
    b.heard(a(2), 40000, &q, S);
    b.heard(a(3), 5353, &query(&[("songs.local.", 1)]), S);
    assert_eq!(b.times_of(a(1)), vec![S]);
    assert!(b.times_of(a(2)).is_empty());
    assert!(b.times_of(a(3)).is_empty());
}

/// dstack #319, field 16.11.2026: Dante hardware (an Audinate module)
/// refreshes its own cache every 96 s, which is normal Dante behaviour. A
/// source that announces `_netaudio-arc` records with no `dstack_vers` is
/// such hardware and never named; a host with no records, or one whose ARC
/// record states `dstack_vers`, still is.
#[test]
fn dante_hardware_on_the_cycle_is_not_named_and_any_other_host_is() {
    let mut b = Browsers::default();
    let (hardware, pc, ours) = (a(103), a(20), a(30));
    // The hardware answers with its ARC record (no dstack_vers) before its
    // chain is complete; the dstack device states its version.
    b.arc_from(hardware, false, 50 * S);
    b.arc_from(ours, true, 50 * S);
    for k in 0..4 {
        for host in [hardware, pc, ours] {
            b.query(host, k * CYCLE_NS);
        }
    }
    let named: Vec<Ipv4Addr> = b.take(3 * CYCLE_NS).iter().map(|x| x.source).collect();
    assert_eq!(named, vec![pc, ours]);
}

/// The four cycle queries of `host` ending at `end_ns`.
fn refreshes(b: &mut Browsers, host: Ipv4Addr, end_ns: u64) {
    for k in (0..4).rev() {
        b.query(host, end_ns - k * CYCLE_NS);
    }
}

#[test]
fn a_hardware_mark_lasts_an_hour_and_a_dstack_record_outweighs_it() {
    let end = 10_000 * S;
    // The mark's hour to the ns: hidden a ns before it ends, named at it.
    let mut b = Browsers::default();
    b.arc_from(a(1), false, end + 1 - HARDWARE_NS);
    refreshes(&mut b, a(1), end);
    assert!(b.take(end).is_empty());
    assert_eq!(b.take(end + 1).len(), 1);
    // A source stating dstack_vers within the hour is named, whichever
    // came last.
    let mut b = Browsers::default();
    b.arc_from(a(2), false, end - 20 * S);
    b.arc_from(a(2), true, end - 10 * S);
    b.arc_from(a(3), true, end - 20 * S);
    b.arc_from(a(3), false, end - 10 * S);
    // ... but one whose dstack record is over an hour old is hardware.
    b.arc_from(a(4), true, end - HARDWARE_NS);
    b.arc_from(a(4), false, end - 10 * S);
    for host in [a(2), a(3), a(4)] {
        refreshes(&mut b, host, end);
    }
    let named: Vec<Ipv4Addr> = b.take(end).iter().map(|x| x.source).collect();
    assert_eq!(named, vec![a(2), a(3)]);
}

#[test]
fn past_the_most_marks_the_source_heard_longest_ago_is_forgotten() {
    let mut b = Browsers::default();
    // The oldest mark is the highest address, so neither address order
    // nor arrival order alone picks it.
    b.arc_from(a(200), false, S);
    for k in 1..MAX_HOSTS {
        b.arc_from(a(u8::try_from(k).unwrap()), false, (k as u64 + 1) * S);
    }
    b.arc_from(a(201), false, 100 * S);
    let end = 1_000 * S;
    for host in [a(200), a(1), a(201)] {
        refreshes(&mut b, host, end);
    }
    let named: Vec<Ipv4Addr> = b.take(end).iter().map(|x| x.source).collect();
    assert_eq!(named, vec![a(200)], "only the forgotten mark is named");
}
