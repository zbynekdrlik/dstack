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
