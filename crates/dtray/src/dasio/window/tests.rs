// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
    Rect {
        left,
        top,
        right,
        bottom,
    }
}

#[test]
fn the_window_is_centred_in_the_work_area() {
    // 1920 x 1200 at 125 %, DPI unaware: 1536 x 912 logical, taskbar off.
    let work = rect(0, 0, 1536, 912);
    assert_eq!(place(work, 1056, 541), (240, 185));
    // An offset work area (a second monitor, a taskbar on the left).
    let work = rect(100, 50, 1100, 650);
    assert_eq!(place(work, 800, 400), (200, 150));
}

#[test]
fn a_window_larger_than_the_work_area_keeps_its_top_left_on_it() {
    // 1366 x 768 at 125 %: 1093 x 566 logical over the taskbar.
    let work = rect(0, 0, 1093, 566);
    assert_eq!(place(work, 1056, 541), (18, 12));
    assert_eq!(place(work, 1200, 700), (0, 0));
    let work = rect(-1093, 40, 0, 606);
    assert_eq!(place(work, 1200, 700), (-1093, 40));
}

#[test]
fn a_taller_tab_keeps_the_window_where_it_fits() {
    let work = rect(0, 0, 1536, 912);
    assert_eq!(keep(work, (240, 185), 1056, 681), (240, 185));
    // It would run under the taskbar: up just enough.
    assert_eq!(keep(work, (240, 300), 1056, 681), (240, 231));
    // Past the right edge: left just enough.
    assert_eq!(keep(work, (600, 100), 1056, 541), (480, 100));
    // Larger than the work area: its top-left.
    let small = rect(0, 0, 1093, 566);
    assert_eq!(keep(small, (18, 12), 1056, 681), (18, 0));
    assert_eq!(keep(small, (-50, -20), 1200, 700), (0, 0));
}
