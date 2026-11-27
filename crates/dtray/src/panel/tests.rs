// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::model::Tracker;

fn r(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
    Rect {
        left,
        top,
        right,
        bottom,
    }
}

fn inside(p: &Placement, work: Rect) -> bool {
    p.rect.left >= work.left
        && p.rect.top >= work.top
        && p.rect.right <= work.right
        && p.rect.bottom <= work.bottom
}

#[test]
fn a_bottom_taskbar_puts_the_panel_above_the_icon_its_right_edge_at_the_icon_s() {
    // The FOH notebook: 1920x1200 at 125 %, a taskbar 30 px high.
    let work = r(0, 0, 1920, 1170);
    let icon = r(1360, 1135, 1384, 1165);
    let p = place(work, icon, 600, 400, 8);
    assert_eq!(p.rect, r(784, 727, 1384, 1127));
    assert!(!p.scroll);
    assert!(inside(&p, work));
}

#[test]
fn a_small_work_area_caps_the_height_and_scrolls() {
    let work = r(0, 0, 800, 300);
    let icon = r(700, 305, 724, 329);
    let p = place(work, icon, 600, 900, 8);
    assert_eq!(p.rect, r(124, 0, 724, 300));
    assert!(p.scroll);
    assert!(inside(&p, work));
    // Exactly the work area's height: no scroll; one pixel more scrolls.
    assert!(!place(work, icon, 600, 300, 8).scroll);
    assert!(place(work, icon, 600, 301, 8).scroll);
}

#[test]
fn a_panel_wider_than_the_work_area_takes_its_width() {
    let work = r(0, 0, 640, 480);
    let p = place(work, r(600, 485, 624, 509), 1000, 200, 8);
    assert_eq!(p.rect.left, 0);
    assert_eq!(p.rect.width(), 640);
    assert!(inside(&p, work));
}

#[test]
fn an_icon_at_the_left_edge_keeps_the_panel_inside() {
    let work = r(0, 0, 1920, 1170);
    let p = place(work, r(10, 1135, 34, 1165), 600, 400, 8);
    assert_eq!(p.rect.left, 0);
    assert_eq!(p.rect.right, 600);
    assert!(inside(&p, work));
}

#[test]
fn a_taskbar_at_the_side_puts_the_panel_beside_it() {
    // Right: the panel left of the taskbar, its bottom at the icon's.
    let work = r(0, 0, 1860, 1200);
    let p = place(work, r(1880, 1100, 1904, 1124), 400, 300, 8);
    assert_eq!(p.rect, r(1460, 824, 1860, 1124));
    // An icon near the top: the panel moved down into the work area.
    let p = place(work, r(1880, 10, 1904, 34), 400, 300, 8);
    assert_eq!(p.rect.top, 0);
    assert!(inside(&p, work));
    // Left.
    let work = r(60, 0, 1920, 1200);
    let p = place(work, r(18, 1100, 42, 1124), 400, 300, 8);
    assert_eq!(p.rect, r(60, 824, 460, 1124));
}

#[test]
fn an_icon_at_the_work_area_s_edge_keeps_the_gap() {
    // A right taskbar whose icon starts right at the work area's edge: the
    // panel keeps its gap left of the icon (no clamp involved).
    let p = place(r(0, 0, 1860, 1200), r(1860, 1100, 1884, 1124), 400, 300, 8);
    assert_eq!(p.rect, r(1452, 824, 1852, 1124));
    // Left: the mirror.
    let p = place(r(60, 0, 1920, 1200), r(36, 1100, 60, 1124), 400, 300, 8);
    assert_eq!(p.rect, r(68, 824, 468, 1124));
}

#[test]
fn the_room_above_is_counted_from_the_work_area_s_top() {
    // Above: 400 px, below: 580 px, so below. Counted from 0 instead of
    // the work area's top (500), above would look larger.
    let p = place(r(0, 500, 1000, 1500), r(500, 900, 520, 920), 100, 100, 0);
    assert_eq!(p.rect.top, 920);
}

#[test]
fn a_top_taskbar_puts_the_panel_below_the_icon() {
    let work = r(0, 40, 1920, 1200);
    let p = place(work, r(1800, 8, 1824, 32), 400, 300, 8);
    assert_eq!(p.rect, r(1424, 40, 1824, 340));
}

#[test]
fn an_icon_inside_the_work_area_picks_the_larger_side() {
    // An auto-hidden taskbar or the overflow flyout: the icon is inside.
    let work = r(0, 0, 1920, 1200);
    let low = place(work, r(1000, 1100, 1024, 1124), 400, 300, 8);
    assert_eq!(low.rect.bottom, 1092, "above the icon");
    let high = place(work, r(1000, 100, 1024, 124), 400, 300, 8);
    assert_eq!(high.rect.top, 132, "below the icon");
    // Equal room: above.
    let mid = place(r(0, 0, 1000, 1000), r(500, 480, 520, 520), 100, 100, 0);
    assert_eq!(mid.rect.bottom, 480);
}

#[test]
fn a_second_monitor_s_work_area_holds_the_panel() {
    // The tray on a monitor right of the primary, at a negative or offset
    // origin: the panel never strays onto the other monitor.
    let work = r(1920, 0, 3840, 1040);
    let p = place(work, r(1925, 1045, 1949, 1075), 500, 2000, 8);
    assert!(inside(&p, work));
    assert_eq!(p.rect.left, 1920);
    assert!(p.scroll);
    let work = r(-1280, -200, 0, 824);
    let p = place(work, r(-40, 830, -16, 854), 500, 300, 8);
    assert!(inside(&p, work));
    assert_eq!(p.rect, r(-516, 522, -16, 822));
}

#[test]
fn an_empty_work_area_gives_a_one_pixel_window_not_a_panic() {
    let work = r(0, 0, 0, 0);
    let p = place(work, r(0, 0, 0, 0), 400, 300, 8);
    assert_eq!(p.rect, r(0, 0, 1, 1), "at the work area's corner");
    assert!(p.scroll);
}

#[test]
fn dips_scale_with_the_monitor_s_dpi() {
    assert_eq!(scaled(10, 96), 10);
    assert_eq!(scaled(100, 120), 125);
    assert_eq!(scaled(1, 144), 2, "1.5 rounds up");
    assert_eq!(scaled(3, 120), 4, "3.75");
    assert_eq!(scaled(5, 115), 6, "5.99");
    assert_eq!(scaled(5, 105), 5, "5.47");
    assert_eq!(scaled(-8, 120), -10);
    assert_eq!(scaled(i32::MAX, 960), i32::MAX);
}

fn dlisten(app: &str) -> String {
    format!(
        r#"{{"clock":{{"ready":1,"source":"dsync"}},"channels":[{{"status":{RECEIVING}}}],"app":{app}}}"#,
        RECEIVING = 0x0101_0009
    )
}

const OUTPUTS: &str = r#"["Speakers (Realtek(R) Audio)","Headphones (Realtek(R) Audio)"]"#;

#[test]
fn a_listening_app_plays_on_its_output_and_the_picker_selects_it() {
    let s = dlisten(&format!(
        r#"{{"line":"x","state":"ok","outputs":{OUTPUTS},"output":1,"pinned":"Headphones"}}"#
    ));
    let o = output_row(&s).unwrap();
    assert_eq!(o.plays, "Plays on: Headphones (Realtek(R) Audio)");
    assert_eq!(o.warning, None, "the --output one");
    assert_eq!(o.choices.len(), 2);
    assert_eq!(o.selected, Some(1));
    // Any case, exact name too.
    let s = dlisten(&format!(
        r#"{{"outputs":{OUTPUTS},"output":0,"pinned":"speakers (realtek(r) audio)"}}"#
    ));
    assert_eq!(output_row(&s).unwrap().warning, None);
}

#[test]
fn another_output_than_the_pinned_one_is_a_warning() {
    // dtray's pick moved the run to the speakers; --output still names
    // the headphones (the 26.11.2026 service's case, by hand).
    let s = dlisten(&format!(
        r#"{{"outputs":{OUTPUTS},"output":0,"pinned":"Headphones"}}"#
    ));
    let o = output_row(&s).unwrap();
    assert_eq!(o.plays, "Plays on: Speakers (Realtek(R) Audio)");
    assert_eq!(
        o.warning.as_deref(),
        Some("Not the --output \"Headphones\" output: a restart plays on \"Headphones\" again")
    );
    assert_eq!(o.selected, Some(0));
    // Names past the shown length are compared whole: the --output text
    // past the 64th character of a long endpoint name is still its own.
    let long = format!("{} Headphones", "x".repeat(70));
    let s = dlisten(&format!(
        r#"{{"outputs":["{long}"],"output":0,"pinned":"Headphones"}}"#
    ));
    let o = output_row(&s).unwrap();
    assert_eq!(o.warning, None);
    assert_eq!(o.plays, format!("Plays on: {}", "x".repeat(64)));
    // No --output: whatever plays is the run's own pick.
    let s = dlisten(&format!(r#"{{"outputs":{OUTPUTS},"output":0}}"#));
    assert_eq!(output_row(&s).unwrap().warning, None);
}

#[test]
fn a_run_that_plays_nothing_says_so() {
    let s = dlisten(
        r#"{"outputs":["Speakers (Realtek(R) Audio)"],"waiting_for":"Headphones","pinned":"Headphones"}"#,
    );
    let o = output_row(&s).unwrap();
    assert_eq!(o.plays, "Plays on: nothing");
    assert_eq!(
        o.warning.as_deref(),
        Some(
            "Plays nothing: no output holding \"Headphones\" (its --output) is there; \
             it plays as soon as one comes"
        )
    );
    assert_eq!(o.selected, None);
    assert_eq!(o.choices.len(), 1, "a pick still works while it waits");
    // Waiting for an output picked in dtray (dlisten names the run after
    // it until the next start): never called the --output one.
    let s = dlisten(
        r#"{"outputs":["Speakers (Realtek(R) Audio)"],"waiting_for":"USB DAC","pinned":"Headphones"}"#,
    );
    assert_eq!(
        output_row(&s).unwrap().warning.as_deref(),
        Some(
            "Plays nothing: the output \"USB DAC\" chosen in dtray is not there; it plays \
             as soon as it comes (a restart plays on \"Headphones\")"
        )
    );
    let s = dlisten(r#"{"outputs":[],"waiting_for":"USB DAC"}"#);
    assert_eq!(
        output_row(&s).unwrap().warning.as_deref(),
        Some(
            "Plays nothing: the output \"USB DAC\" chosen in dtray is not there; it plays as soon as it comes"
        )
    );
    // Neither an output nor a wait: nothing is open.
    let s = dlisten(&format!(r#"{{"outputs":{OUTPUTS}}}"#));
    let o = output_row(&s).unwrap();
    assert_eq!(o.plays, "Plays on: nothing");
    assert_eq!(
        o.warning.as_deref(),
        Some("Plays nothing: no output is open")
    );
    // An index past the list: nothing named, nothing selected.
    let s = dlisten(&format!(r#"{{"outputs":{OUTPUTS},"output":7}}"#));
    let o = output_row(&s).unwrap();
    assert_eq!(o.plays, "Plays on: nothing");
    assert_eq!(o.selected, None);
    // An empty pinned or waiting text counts as none.
    let s = dlisten(&format!(
        r#"{{"outputs":{OUTPUTS},"output":0,"pinned":"","waiting_for":""}}"#
    ));
    assert_eq!(output_row(&s).unwrap().warning, None);
}

#[test]
fn a_device_without_outputs_has_no_output_part() {
    assert_eq!(output_row(&dlisten(r#"{"line":"x"}"#)), None);
    assert_eq!(output_row(&dlisten(r#"{"outputs":"Speakers"}"#)), None);
    assert_eq!(output_row(r#"{"clock":{"ready":1}}"#), None);
    assert_eq!(output_row("not json"), None);
    // An empty list is still an app with outputs: it plays nothing.
    assert!(output_row(&dlisten(r#"{"outputs":[]}"#)).is_some());
}

#[test]
fn the_rows_follow_the_readings_then_the_missing_devices() {
    let readings: Vec<Reading> = vec![
        (
            "MBC-dlisten".into(),
            Ok(dlisten(&format!(
                r#"{{"line":"Headphones","state":"ok","outputs":{OUTPUTS},"output":1}}"#
            ))),
        ),
        ("gone".into(), Err("no answer".into())),
    ];
    let expected = vec!["MBC-dlisten".to_string(), "Stopped one".to_string()];
    let view = Tracker::default().update_with(&readings, &expected);
    let rows = super::rows(&readings, &view);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].name, "MBC-dlisten");
    assert_eq!(rows[0].colour, Colour::Green);
    assert!(
        rows[0]
            .line
            .starts_with("clock dsync ready; RX 1/1 connected"),
        "{}",
        rows[0].line
    );
    assert_eq!(rows[0].output.as_ref().map(|o| o.selected), Some(Some(1)));
    assert_eq!(rows[1].name, "gone");
    assert_eq!(rows[1].colour, Colour::Red);
    assert_eq!(rows[1].line, "status unreadable (no answer)");
    assert_eq!(rows[1].output, None);
    assert_eq!(rows[2].name, "Stopped one");
    assert_eq!(rows[2].line, "expected, not running");
    assert_eq!(rows[2].colour, Colour::Red);
    assert_eq!(
        shape(&rows),
        vec![
            (
                "MBC-dlisten".to_string(),
                Some(vec![
                    "Speakers (Realtek(R) Audio)".to_string(),
                    "Headphones (Realtek(R) Audio)".to_string()
                ])
            ),
            ("gone".to_string(), None),
            ("Stopped one".to_string(), None),
        ]
    );
}

#[test]
fn a_view_shorter_than_its_lines_colours_the_rest_red() {
    let view = View {
        colour: Colour::Red,
        tooltip: String::new(),
        lines: vec!["a: x".into()],
        colours: vec![],
    };
    let got = super::rows(&[], &view);
    assert_eq!(got[0].name, "a");
    assert_eq!(got[0].line, "x");
    assert_eq!(got[0].colour, Colour::Red);
    // A line with no name in front stays whole.
    let view = View {
        lines: vec!["odd".into()],
        colours: vec![Colour::Green],
        ..view
    };
    let got = super::rows(&[], &view);
    assert_eq!((got[0].name.as_str(), got[0].line.as_str()), ("odd", "odd"));
}

#[test]
fn each_colour_has_its_word() {
    assert_eq!(colour_word(Colour::Grey), "no device");
    assert_eq!(colour_word(Colour::Green), "ok");
    assert_eq!(colour_word(Colour::Orange), "warning");
    assert_eq!(colour_word(Colour::Red), "error");
}

#[test]
fn the_scroll_stays_within_the_content() {
    assert_eq!(scroll_to(0, 40, 1000, 300), 40);
    assert_eq!(scroll_to(680, 40, 1000, 300), 700, "the last page");
    assert_eq!(scroll_to(700, 1, 1000, 300), 700);
    assert_eq!(scroll_to(10, -40, 1000, 300), 0);
    assert_eq!(scroll_to(0, 40, 300, 300), 0, "it fits: no scroll");
    assert_eq!(scroll_to(0, 40, 200, 300), 0);
    assert_eq!(scroll_to(699, 1, 1000, 300), 700);
    assert_eq!(scroll_to(i32::MAX, 1, i32::MAX, 0), i32::MAX);
}
