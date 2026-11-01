use super::*;
use std::cell::RefCell;

const S: u64 = EVERY_NS;

/// Counts and peaks a test sets per slot; a take empties the peak.
struct Lanes {
    counts: RefCell<[u64; MAX_FLOWS]>,
    peaks: RefCell<[u64; MAX_FLOWS]>,
}

impl Lanes {
    fn new() -> Self {
        Lanes {
            counts: RefCell::new([0; MAX_FLOWS]),
            peaks: RefCell::new([0; MAX_FLOWS]),
        }
    }
    fn set(&self, slot: usize, count: u64, peak: u64) {
        self.counts.borrow_mut()[slot] = count;
        self.peaks.borrow_mut()[slot] = peak;
    }
    fn look(&self, w: &mut LateWatch, now: u64) -> Vec<Late> {
        w.look(
            now,
            |s| self.counts.borrow()[s],
            |s| std::mem::take(&mut self.peaks.borrow_mut()[s]),
        )
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
            age_peak_ns: 17_448_000
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
                age_peak_ns: 11_000_000
            },
            Late {
                slot: 31,
                packets: 1,
                age_peak_ns: 10_001_000
            }
        ]
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
    assert_eq!(lanes.peaks.borrow()[0], 2_000_000);
    assert_eq!(
        lanes.look(&mut w, 11 * S),
        [Late {
            slot: 0,
            packets: 4,
            age_peak_ns: 2_000_000
        }]
    );
    assert_eq!(lanes.peaks.borrow()[0], 0, "taken at the look");
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
            age_peak_ns: 6_000_000
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
