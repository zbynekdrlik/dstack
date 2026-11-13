use super::*;

fn lost(holes: u64, frames: u64) -> Lost {
    Lost { holes, frames }
}

#[test]
fn a_hole_opens_and_an_empty_one_is_none() {
    let mut h = Holes::default();
    assert_eq!(h.open(10, 10), Lost::default());
    assert!(h.open_holes().is_empty());
    assert_eq!(h.open(10, 42), Lost::default());
    assert_eq!(h.open_holes(), [(10, 42)]);
}

#[test]
fn a_packet_that_covers_a_hole_removes_it() {
    let mut h = Holes::default();
    h.open(32, 64);
    h.open(96, 128);
    assert_eq!(h.fill(32, 64), (true, Lost::default()));
    assert_eq!(h.open_holes(), [(96, 128)]);
    // Wider than the hole: still filled.
    assert_eq!(h.fill(90, 130), (true, Lost::default()));
    assert!(h.open_holes().is_empty());
}

#[test]
fn a_packet_beside_a_hole_fills_nothing() {
    let mut h = Holes::default();
    h.open(32, 64);
    assert_eq!(h.fill(0, 32), (false, Lost::default()));
    assert_eq!(h.fill(64, 96), (false, Lost::default()));
    assert_eq!(h.open_holes(), [(32, 64)]);
}

#[test]
fn a_packet_over_one_end_of_a_hole_trims_it() {
    let mut h = Holes::default();
    h.open(32, 96);
    assert_eq!(h.fill(20, 40), (true, Lost::default()));
    assert_eq!(h.open_holes(), [(40, 96)]);
    assert_eq!(h.fill(80, 100), (true, Lost::default()));
    assert_eq!(h.open_holes(), [(40, 80)]);
}

#[test]
fn a_packet_over_two_holes_trims_both() {
    let mut h = Holes::default();
    h.open(0, 32);
    h.open(64, 96);
    h.open(200, 300);
    assert_eq!(h.fill(16, 80), (true, Lost::default()));
    assert_eq!(h.open_holes(), [(0, 16), (80, 96), (200, 300)]);
    // Across one whole hole into the next.
    assert_eq!(h.fill(10, 250), (true, Lost::default()));
    assert_eq!(h.open_holes(), [(0, 10), (250, 300)]);
}

#[test]
fn a_packet_inside_a_hole_splits_it() {
    let mut h = Holes::default();
    h.open(0, 32);
    h.open(64, 160);
    h.open(200, 232);
    assert_eq!(h.fill(96, 128), (true, Lost::default()));
    assert_eq!(h.open_holes(), [(0, 32), (64, 96), (128, 160), (200, 232)]);
}

#[test]
fn a_full_set_loses_its_oldest_to_a_new_hole_or_a_split() {
    let mut h = Holes::default();
    for k in 0..HOLES as u64 {
        assert_eq!(h.open(100 * k, 100 * k + 10), Lost::default());
    }
    assert_eq!(h.open_holes().len(), HOLES);
    // A 17th hole: the oldest (0..10) is lost.
    assert_eq!(h.open(5_000, 5_032), lost(1, 10));
    assert_eq!(h.open_holes().first(), Some(&(100, 110)));
    assert_eq!(h.open_holes().last(), Some(&(5_000, 5_032)));
    // A split of the newest needs room: the oldest (100..110) goes.
    assert_eq!(h.fill(5_010, 5_020), (true, lost(1, 10)));
    let open = h.open_holes();
    assert_eq!(open.len(), HOLES);
    assert_eq!(open[0], (200, 210));
    assert_eq!(&open[HOLES - 2..], [(5_000, 5_010), (5_020, 5_032)]);
    // A split of the oldest itself in a full set: its first part is the
    // oldest and goes, the second stays in order.
    assert_eq!(h.fill(202, 205), (true, lost(1, 2)));
    assert_eq!(h.open_holes()[0], (205, 210));
    assert_eq!(h.open_holes()[1], (300, 310));
}

#[test]
fn a_hole_expires_once_its_end_is_at_the_playout_point() {
    let mut h = Holes::default();
    h.open(64, 96);
    h.open(160, 192);
    assert_eq!(h.expire(95), Lost::default());
    assert_eq!(h.expire(96), lost(1, 32));
    assert_eq!(h.open_holes(), [(160, 192)]);
    assert_eq!(h.expire(96), Lost::default());
    h.open(300, 301);
    assert_eq!(h.expire(10_000), lost(2, 33));
    assert!(h.open_holes().is_empty());
}

#[test]
fn a_clear_loses_every_open_hole() {
    let mut h = Holes::default();
    assert_eq!(h.clear(), Lost::default());
    h.open(0, 5);
    h.open(10, 30);
    assert_eq!(h.clear(), lost(2, 25));
    assert!(h.open_holes().is_empty());
    assert_eq!(h.clear(), Lost::default());
}
