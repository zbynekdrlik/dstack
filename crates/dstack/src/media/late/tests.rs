use super::*;
use std::cell::RefCell;

const S: u64 = EVERY_NS;

/// Counts and peaks a test sets per slot; a take empties the peak and the
/// time since an escape.
struct Lanes {
    counts: RefCell<[LateCounts; MAX_FLOWS]>,
}

impl Lanes {
    fn new() -> Self {
        Lanes {
            counts: RefCell::new([LateCounts::default(); MAX_FLOWS]),
        }
    }
    fn set(&self, slot: usize, count: u64, peak: u64) {
        let mut c = self.counts.borrow_mut();
        c[slot].packets = count;
        c[slot].age_peak_ns = peak;
    }
    fn set_relayed(&self, slot: usize, relayed: u64, since_escape_ns: u64) {
        let mut c = self.counts.borrow_mut();
        c[slot].relayed = relayed;
        c[slot].since_escape_ns = Some(since_escape_ns);
    }
    fn look(&self, w: &mut LateWatch, now: u64) -> Vec<Late> {
        w.look(now, |s| {
            let mut c = self.counts.borrow_mut();
            let now = c[s];
            c[s].age_peak_ns = 0;
            c[s].since_escape_ns = None;
            now
        })
    }
}

#[test]
fn the_first_look_only_learns_the_counts() {
    let lanes = Lanes::new();
    // Late packets from before the watch started (a flow already running).
    lanes.set(3, 500, 12_000_000);
    let mut w = LateWatch::new();
    assert_eq!(lanes.look(&mut w, 7 * S), []);
    assert_eq!(lanes.look(&mut w, 8 * S), [], "nothing new");
}

#[test]
fn a_rise_is_reported_once_with_its_second_s_peak() {
    let lanes = Lanes::new();
    let mut w = LateWatch::new();
    lanes.look(&mut w, 0);
    lanes.set(2, 24, 17_448_000);
    lanes.set(5, 0, 900_000);
    assert_eq!(
        lanes.look(&mut w, S),
        [Late {
            slot: 2,
            packets: 24,
            age_peak_ns: 17_448_000,
            relayed: 0,
            since_escape_ns: None
        }]
    );
    // The next second: no rise, nothing (the peak taken was this second's).
    lanes.set(2, 24, 800_000);
    assert_eq!(lanes.look(&mut w, 2 * S), []);
    // A second rise counts only the new packets.
    lanes.set(2, 30, 11_000_000);
    lanes.set(31, 1, 10_001_000);
    assert_eq!(
        lanes.look(&mut w, 3 * S),
        [
            Late {
                slot: 2,
                packets: 6,
                age_peak_ns: 11_000_000,
                relayed: 0,
                since_escape_ns: None
            },
            Late {
                slot: 31,
                packets: 1,
                age_peak_ns: 10_001_000,
                relayed: 0,
                since_escape_ns: None
            }
        ]
    );
}

#[test]
fn a_report_says_how_many_late_packets_the_relay_sent_and_how_soon_after_an_escape() {
    // dstack #177: a pass ~1 ms late on every slot at once; whose was it?
    let lanes = Lanes::new();
    let mut w = LateWatch::new();
    lanes.set(4, 10, 0);
    lanes.set_relayed(4, 3, 0);
    lanes.look(&mut w, 0);
    // Two late packets, one sent by the relay, 600 us after an escape began.
    lanes.set(4, 12, 1_088_000);
    lanes.set_relayed(4, 4, 600_000);
    assert_eq!(
        lanes.look(&mut w, S),
        [Late {
            slot: 4,
            packets: 2,
            age_peak_ns: 1_088_000,
            relayed: 1,
            since_escape_ns: Some(600_000),
        }]
    );
    // The TX thread's own late packet a second later: none relayed, and the
    // time since the escape is that second's, not the last report's.
    lanes.set(4, 13, 1_016_000);
    assert_eq!(
        lanes.look(&mut w, 2 * S),
        [Late {
            slot: 4,
            packets: 1,
            age_peak_ns: 1_016_000,
            relayed: 0,
            since_escape_ns: None,
        }]
    );
}

#[test]
fn the_watch_looks_once_a_second_and_keeps_counts_between() {
    let lanes = Lanes::new();
    let mut w = LateWatch::new();
    lanes.look(&mut w, 10 * S);
    lanes.set(0, 4, 2_000_000);
    // Ticks inside the second: no look, the peak not taken.
    assert_eq!(lanes.look(&mut w, 10 * S + S / 10), []);
    assert_eq!(lanes.look(&mut w, 11 * S - 1), []);
    assert_eq!(lanes.counts.borrow()[0].age_peak_ns, 2_000_000);
    assert_eq!(
        lanes.look(&mut w, 11 * S),
        [Late {
            slot: 0,
            packets: 4,
            age_peak_ns: 2_000_000,
            relayed: 0,
            since_escape_ns: None
        }]
    );
    assert_eq!(lanes.counts.borrow()[0].age_peak_ns, 0, "taken at the look");
}

#[test]
fn a_count_that_starts_again_is_no_rise() {
    // A device restart in a test or a counter read back lower: no report,
    // the lower count learned.
    let lanes = Lanes::new();
    let mut w = LateWatch::new();
    lanes.set(1, 100, 0);
    lanes.look(&mut w, 0);
    lanes.set(1, 3, 5_000_000);
    assert_eq!(lanes.look(&mut w, S), []);
    lanes.set(1, 5, 6_000_000);
    assert_eq!(
        lanes.look(&mut w, 2 * S),
        [Late {
            slot: 1,
            packets: 2,
            age_peak_ns: 6_000_000,
            relayed: 0,
            since_escape_ns: None
        }]
    );
}

#[test]
fn the_loss_watch_reports_a_rise_in_gaps_or_drops_once_a_second() {
    let counts = RefCell::new([(0u64, 0u64, 0u64); MAX_FLOWS]);
    let mut w = RxWatch::new();
    let look = |w: &mut RxWatch, now| w.look(now, |s| counts.borrow()[s]);
    // Losses from before the watch started: learnt, not reported.
    counts.borrow_mut()[4] = (3, 96, 1);
    assert_eq!(look(&mut w, 0), []);
    // A gap of one packet on slot 4, drops on slot 9 only.
    counts.borrow_mut()[4] = (4, 128, 1);
    counts.borrow_mut()[9] = (0, 0, 2);
    assert_eq!(look(&mut w, S / 2), [], "inside the second");
    assert_eq!(
        look(&mut w, S),
        [
            Loss {
                slot: 4,
                gaps: 1,
                frames: 32,
                drops: 0
            },
            Loss {
                slot: 9,
                gaps: 0,
                frames: 0,
                drops: 2
            },
        ]
    );
    // Nothing new: quiet. A count read back lower is no loss.
    assert_eq!(look(&mut w, 2 * S), []);
    counts.borrow_mut()[4] = (0, 0, 0);
    assert_eq!(look(&mut w, 3 * S), []);
    counts.borrow_mut()[4] = (1, 8, 0);
    assert_eq!(
        look(&mut w, 4 * S),
        [Loss {
            slot: 4,
            gaps: 1,
            frames: 8,
            drops: 0
        }]
    );
}

/// dasio (2.11.2026): failed sends are named once a second with their
/// count and the last OS error; quiet seconds say nothing.
#[test]
fn the_send_watch_names_each_flow_s_new_failed_sends_with_the_os_error() {
    let mut w = SendWatch::new();
    let counts = |errors: [u64; 2], code: i32| {
        move |s: usize| match s {
            0 => (errors[0], 0, code),
            1 => (errors[1], 0, code),
            _ => (0, 0, 0),
        }
    };
    // The first look only learns.
    assert!(w.look(0, counts([5, 9], 0)).is_empty());
    // Within the second: nothing.
    assert!(w.look(EVERY_NS - 1, counts([50, 9], 10055)).is_empty());
    // A second on: slot 0 rose by 95.
    assert_eq!(
        w.look(EVERY_NS, counts([100, 9], 10055)),
        vec![SendErrors {
            slot: 0,
            errors: 95,
            retried: 0,
            os_error: 10055
        }]
    );
    // Both rise: one line each.
    let both = w.look(2 * EVERY_NS, counts([101, 12], 10035));
    assert_eq!(
        both.iter()
            .map(|e| (e.slot, e.errors, e.os_error))
            .collect::<Vec<_>>(),
        [(0, 1, 10035), (1, 3, 10035)]
    );
    // Quiet: nothing.
    assert!(w.look(3 * EVERY_NS, counts([101, 12], 10035)).is_empty());
}

/// dstack #1: a flow whose refused packets all went out on a retry is
/// reported too (not lost); one that lost packets as well is lost.
#[test]
fn the_send_watch_reports_retried_sends_apart_from_lost_ones() {
    let mut w = SendWatch::new();
    let counts = |errors: u64, retries: u64| {
        move |s: usize| match s {
            3 => (errors, retries, 10055),
            _ => (0, 0, 0),
        }
    };
    assert!(w.look(0, counts(2, 10)).is_empty());
    // Retries alone: reported, nothing lost.
    let r = w.look(EVERY_NS, counts(2, 14));
    assert_eq!(
        r,
        vec![SendErrors {
            slot: 3,
            errors: 0,
            retried: 4,
            os_error: 10055
        }]
    );
    assert!(!r[0].lost());
    // Both: lost.
    let r = w.look(2 * EVERY_NS, counts(3, 15));
    assert_eq!((r[0].errors, r[0].retried), (1, 1));
    assert!(r[0].lost());
    // Lost alone.
    let r = w.look(3 * EVERY_NS, counts(5, 15));
    assert_eq!((r[0].errors, r[0].retried), (2, 0));
    assert!(r[0].lost());
    // Quiet.
    assert!(w.look(4 * EVERY_NS, counts(5, 15)).is_empty());
}

/// dstack #1: the host frame sender's whole-batch refusals, once a second,
/// never per packet.
#[test]
fn the_host_watch_reports_refused_batches_once_a_second() {
    let mut w = HostWatch::new();
    // The first look only learns.
    assert_eq!(w.look(0, 4), None);
    assert_eq!(w.look(S - 1, 9), None, "between looks");
    assert_eq!(w.look(S, 9), Some(5));
    assert_eq!(w.look(2 * S, 9), None, "none since");
    assert_eq!(w.look(3 * S, 10), Some(1));
    assert_eq!(w.look(3 * S + 1, 12), None, "the next look is a second on");
    assert_eq!(w.look(4 * S, 12), Some(2));
}

/// dstack #266: an empty peer name reads `-` in a line, any other as is.
#[test]
fn an_empty_peer_name_reads_a_dash() {
    assert_eq!(named(""), "-");
    assert_eq!(named("FOH-Card"), "FOH-Card");
}

/// dstack #278: the RX minute watch learns at its first look and then
/// gives each slot's reorders of the minute, once a minute.
#[test]
fn the_rx_minute_watch_gives_each_minute_s_reorders() {
    use super::{MINUTE_NS, RxMinute};
    let counts = RefCell::new([0u64; MAX_FLOWS]);
    let mut w = RxMinute::default();
    let look = |w: &mut RxMinute, now| w.look(now, |s| counts.borrow()[s]);
    counts.borrow_mut()[2] = 7;
    let (first, rose) = look(&mut w, 5).unwrap();
    assert!(first);
    assert_eq!(rose[2], 7, "the first look learns all");
    counts.borrow_mut()[2] = 10;
    counts.borrow_mut()[31] = 1;
    assert_eq!(look(&mut w, 5 + MINUTE_NS - 1), None, "inside the minute");
    let (first, rose) = look(&mut w, 5 + MINUTE_NS).unwrap();
    assert!(!first);
    let mut want = [0u64; MAX_FLOWS];
    want[2] = 3;
    want[31] = 1;
    assert_eq!(rose, want);
    // A count read back lower is no reorder.
    counts.borrow_mut()[2] = 0;
    let (_, rose) = look(&mut w, 5 + 2 * MINUTE_NS).unwrap();
    assert_eq!(rose, [0; MAX_FLOWS]);
}
