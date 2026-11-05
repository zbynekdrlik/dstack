//! The follower's re-anchors: absorbed within the bound, re-aligned past
//! it ([`super::Absorb`], [`super::Absorbing`]).

use std::sync::{Arc, Mutex};

use dsync_ptp::follower::model::ClockModel;

use super::*;

/// A locked model anchored at raw counter `m0` on PTP `ptp0`, rate 0,
/// with dsync's counts `epoch` and `reanchors`.
fn model(m0: i64, ptp0: i64, epoch: u32, reanchors: u32) -> ClockModel {
    ClockModel {
        anchored: true,
        locked: true,
        settled: true,
        m0,
        ptp0,
        epoch,
        reanchors,
        windows: 50,
        ..ClockModel::default()
    }
}

const M0: i64 = 1_000_000_000;
const P0: i64 = 5_000_000_000_000;

/// The step of a re-anchor: the new model's time at its anchor less the
/// old model's at that instant (its rate counted).
#[test]
fn a_re_anchor_s_step_is_measured_at_the_new_anchor() {
    let old = model(M0, P0, 0, 0);
    let new = model(2 * M0, P0 + M0 + 2_557, 1, 1);
    assert_eq!(re_anchor_step(&old, &new), Some(2_557));
    let fast = ClockModel {
        rate_ppm: 10.0,
        ..old
    };
    // 1 s at +10 ppm: the old model is 10 µs further at the new anchor.
    assert_eq!(re_anchor_step(&fast, &new), Some(2_557 - 10_000));
    let unanchored = ClockModel {
        anchored: false,
        ..old
    };
    assert_eq!(re_anchor_step(&unanchored, &new), None);
    assert_eq!(re_anchor_step(&old, &ClockModel::default()), None);
}

/// iemmixer (4.11.2026, v0.6.29 live, source internal, the grandmaster's
/// box rebooted): "new time base: re-anchored by +2557ns (epoch 1)", then
/// "-5710ns (epoch 2)", each a pump re-bootstrap and every TX flow started
/// again, for steps under one sample. Such a re-anchor is absorbed (no
/// re-align); a step past [`HANDOVER_NS`], or one with no anchored model
/// before it, is a re-align.
#[test]
fn a_small_re_anchor_is_absorbed_and_a_big_one_re_aligns() {
    let mut a = Absorb::default();
    assert_eq!(a.feed(model(M0, P0, 0, 0)), (0, 0), "the first model");
    assert_eq!(a.feed(model(M0, P0, 0, 0)), (0, 0), "unchanged");
    // A rate word moves the anchor, not the counts: nothing.
    assert_eq!(a.feed(model(M0 + 5, P0 + 5, 0, 0)), (0, 0));
    // +2557 ns, a new time base: absorbed.
    assert_eq!(a.feed(model(2 * M0, P0 + M0 + 2_557, 1, 1)), (0, 1));
    // -5710 ns, another: absorbed.
    let base = P0 + 2 * M0 + 2_557 - 5_710;
    assert_eq!(a.feed(model(3 * M0, base, 2, 2)), (0, 2));
    // At the bound either way: absorbed; one past it: a re-align.
    let base = base + M0 + HANDOVER_NS;
    assert_eq!(a.feed(model(4 * M0, base, 2, 3)), (0, 3));
    let base = base + M0 - HANDOVER_NS;
    assert_eq!(a.feed(model(5 * M0, base, 2, 4)), (0, 4));
    let base = base + M0 + HANDOVER_NS + 1;
    assert_eq!(a.feed(model(6 * M0, base, 2, 5)), (1, 4));
    let base = base + M0 - HANDOVER_NS - 1;
    assert_eq!(
        a.feed(model(7 * M0, base, 3, 5)),
        (2, 4),
        "the epoch alone changed"
    );
    // A re-anchor after a model with no anchor: a re-align.
    let mut a = Absorb::default();
    a.feed(ClockModel::default());
    assert_eq!(a.feed(model(M0, P0, 0, 1)), (1, 0));
}

/// A model source the test drives.
struct FakeModel(Mutex<(ClockModel, bool)>);

impl Model for Arc<FakeModel> {
    fn model(&self) -> ClockModel {
        self.0.lock().unwrap().0
    }
    fn alive(&self) -> bool {
        self.0.lock().unwrap().1
    }
    fn now_ns(&self) -> Option<i64> {
        let m = self.model();
        m.ptp_at(m.m0)
    }
}

/// The follower as the linked clock reads it: its view's epoch counts only
/// the re-aligns, its counts name the absorbed ones, and a linked clock on
/// it keeps its epoch over a small re-anchor and moves it on a big one.
#[test]
fn a_linked_clock_on_the_follower_keeps_its_epoch_over_a_small_re_anchor() {
    let src = Arc::new(FakeModel(Mutex::new((model(M0, P0, 0, 0), true))));
    let f = Absorbing::new(Arc::clone(&src));
    assert_eq!(
        f.view(),
        View {
            alive: true,
            locked: true,
            settled: true,
            anchored: true,
            epoch: 0
        }
    );
    assert_eq!(f.now_ns(), Some(P0));
    let service = Arc::new(MonoClock::new());
    let linked = Linked::new(Some(Box::new(f)), service, None);
    assert_eq!(linked.tick(), Source::Internal);
    let e = linked.epoch();
    src.0.lock().unwrap().0 = model(2 * M0, P0 + M0 + 2_557, 1, 1);
    assert_eq!(linked.epoch(), e, "+2557 ns: no re-align");
    assert_eq!(linked.tick(), Source::Internal);
    assert_eq!(linked.epoch(), e);
    let c = linked.follower_counts().unwrap();
    assert_eq!(
        (c.counts.polls, c.counts.re_anchors, c.absorbed),
        (50, 1, 1)
    );
    // A 1 ms step: a re-align.
    src.0.lock().unwrap().0 = model(3 * M0, P0 + 2 * M0 + 2_557 + 1_000_000, 1, 2);
    assert_ne!(linked.epoch(), e);
    let c = linked.follower_counts().unwrap();
    assert_eq!((c.counts.re_anchors, c.absorbed), (2, 1));
    // Holdover, and a dead thread: as dsync says.
    {
        let mut g = src.0.lock().unwrap();
        g.0.locked = false;
        g.0.settled = false;
        g.0.holdover = true;
        g.1 = false;
    }
    let v = Absorbing::new(Arc::clone(&src)).view();
    assert_eq!(
        (v.alive, v.locked, v.settled, v.anchored),
        (false, false, false, true),
        "{v:?}"
    );
    let c = Absorbing::new(Arc::clone(&src)).counts();
    assert!(c.counts.in_holdover);
    assert_eq!(c.counts.holdovers, 0);
}

/// A reader that finds the tracker busy (another thread feeding it) reads
/// the counts last published, and never waits.
#[test]
fn a_busy_tracker_is_read_from_its_last_counts() {
    let src = Arc::new(FakeModel(Mutex::new((model(M0, P0, 0, 0), true))));
    let f = Absorbing::new(Arc::clone(&src));
    assert_eq!(f.view().epoch, 0);
    src.0.lock().unwrap().0 = model(2 * M0, P0 + M0 + 1_000_000, 1, 1);
    {
        let _held = f.absorb.lock().unwrap();
        assert_eq!(f.view().epoch, 0, "busy: the last counts");
    }
    assert_eq!(f.view().epoch, 1);
}

/// A lock poisoned by a panic elsewhere is still fed: the epoch never
/// freezes.
#[test]
fn a_poisoned_tracker_is_still_fed() {
    let src = Arc::new(FakeModel(Mutex::new((model(M0, P0, 0, 0), true))));
    let f = Absorbing::new(Arc::clone(&src));
    assert_eq!(f.view().epoch, 0);
    std::thread::scope(|s| {
        let _ = s
            .spawn(|| {
                let _g = f.absorb.lock().unwrap();
                panic!("poison the tracker");
            })
            .join();
    });
    assert!(f.absorb.is_poisoned());
    src.0.lock().unwrap().0 = model(2 * M0, P0 + M0 + 1_000_000, 1, 1);
    assert_eq!(f.view().epoch, 1);
}

/// dstack #1 (iemmixer 5.11.2026): after a grandmaster change the follower
/// relocked at -39.8 µs and took 6 min 41 s to take over again, and the
/// status could not say why. Its counts carry what the selector decides
/// on: dsync's lock, settled, last error and recent |e| p90.
#[test]
fn the_follower_counts_carry_its_lock_settled_and_errors() {
    let unsettled = ClockModel {
        settled: false,
        offset_ns: Some(-39_806),
        recent_abs_e_ns: Some(41_250),
        ..model(M0, P0, 0, 0)
    };
    let src = Arc::new(FakeModel(Mutex::new((unsettled, true))));
    let c = Absorbing::new(Arc::clone(&src)).counts();
    assert_eq!(
        (c.locked, c.settled, c.offset_ns, c.recent_abs_e_ns),
        (true, false, Some(-39_806), Some(41_250))
    );
    let fresh = ClockModel {
        locked: false,
        settled: false,
        offset_ns: None,
        recent_abs_e_ns: None,
        ..model(M0, P0, 0, 0)
    };
    *src.0.lock().unwrap() = (fresh, true);
    let c = Absorbing::new(src).counts();
    assert_eq!(
        (c.locked, c.settled, c.offset_ns, c.recent_abs_e_ns),
        (false, false, None, None)
    );
}
