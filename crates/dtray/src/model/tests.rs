// SPDX-License-Identifier: MIT OR Apache-2.0

use dstack::subscribe::SubStatus;

use super::{
    APP_LINE_MAX, App, AppState, Colour, Counts, NOT_RUNNING, Status, TOOLTIP_MAX, Tracker, View,
    app_line, app_of, clip, copy_json, judge, keys, line, missing, missing_line, parse, print_text,
};
use crate::discover::Reading;

#[test]
fn an_app_line_is_one_line_of_at_most_64_units() {
    assert_eq!(app_line("ASIO\n64\tok\r"), "ASIO 64 ok ");
    assert_eq!(app_line("plain"), "plain");
    let long = "x".repeat(APP_LINE_MAX + 10);
    assert_eq!(app_line(&long), "x".repeat(APP_LINE_MAX));
    assert_eq!(app_line(&"x".repeat(APP_LINE_MAX)).len(), APP_LINE_MAX);
    // In a status: the line as shown.
    let v: serde_json::Value =
        serde_json::from_str(r#"{"line":"a\nb","state":"ok"}"#).expect("JSON");
    assert_eq!(app_of(&v).and_then(|a| a.line).as_deref(), Some("a b"));
}

const RECEIVING: u32 = 0x0101_0009;

/// A status line as `dstack::ctl::status` writes it, with the fields a
/// reader uses set: the clock, the RX channels' status words, the RX
/// flows' late counts and the engine's.
fn status_line(ready: u8, channels: &[u32], rx_late: &[u64], engine: Option<(u64, u64)>) -> String {
    let chs: Vec<String> = channels
        .iter()
        .enumerate()
        .map(|(i, s)| {
            format!(r#"{{"rx":{},"status":{s},"written":0,"holes":0,"late":7,"duplicate":0,"early":0}}"#, i + 1)
        })
        .collect();
    let flows: Vec<String> = rx_late
        .iter()
        .enumerate()
        .map(|(i, l)| format!(r#"{{"flow":{},"granted":1,"late":{l},"packets":5}}"#, i + 1))
        .collect();
    let engine = engine.map_or(String::new(), |(m, o)| {
        format!(r#","engine":{{"media":1,"late":3,"missed":{m},"overruns":{o}}}"#)
    });
    format!(
        r#"{{"clock":{{"ready":{ready},"epoch":1,"holdover":0,"source":"dsync"}},"tx":[],"rx":[{}],"channels":[{}]{engine}}}"#,
        flows.join(","),
        chs.join(",")
    )
}

fn ok(name: &str, line: String) -> Reading {
    (name.to_string(), Ok(line))
}

fn healthy() -> String {
    status_line(1, &[RECEIVING, RECEIVING], &[0], Some((0, 0)))
}

#[test]
fn no_device_is_grey() {
    let v = Tracker::default().update(&[]);
    assert_eq!(
        v,
        View {
            colour: Colour::Grey,
            tooltip: "dstack: no devices on this PC".into(),
            lines: vec![],
        }
    );
    assert_eq!(print_text(&v), "dstack: no devices on this PC\n");
}

#[test]
fn a_healthy_device_is_green() {
    let v = Tracker::default().update(&[ok("app", healthy())]);
    assert_eq!(v.colour, Colour::Green);
    assert_eq!(v.tooltip, "dstack: 1 device, ok");
    assert_eq!(
        v.lines,
        vec!["app: clock dsync ready; RX 2/2 connected; late +0; missed +0, overruns +0"]
    );
    assert_eq!(
        print_text(&v),
        "dstack: 1 device, ok\napp: clock dsync ready; RX 2/2 connected; late +0; missed +0, overruns +0\n"
    );
}

#[test]
fn an_unreadable_status_is_red() {
    let readings = vec![
        ok("good", healthy()),
        ("gone".to_string(), Err("no answer: refused".to_string())),
    ];
    let v = Tracker::default().update(&readings);
    assert_eq!(v.colour, Colour::Red);
    assert_eq!(v.tooltip, "dstack: 2 devices, gone: status unreadable");
    assert_eq!(v.lines[1], "gone: status unreadable (no answer: refused)");
    // A line that is no JSON object is unreadable too.
    for bad in ["not json", "[1,2]", "17"] {
        let v = Tracker::default().update(&[ok("odd", bad.into())]);
        assert_eq!(v.colour, Colour::Red, "{bad}");
        assert_eq!(
            v.lines,
            vec!["odd: status unreadable (the status is not JSON)"]
        );
    }
}

#[test]
fn a_clock_not_ready_is_red() {
    let v = Tracker::default().update(&[ok("app", status_line(0, &[RECEIVING], &[0], None))]);
    assert_eq!(v.colour, Colour::Red);
    assert_eq!(v.tooltip, "dstack: 1 device, app: clock not ready");
    assert_eq!(
        v.lines,
        vec!["app: clock dsync not ready; RX 1/1 connected; late +0"]
    );
}

#[test]
fn a_subscription_not_connected_is_orange() {
    let pending = SubStatus::from_code(1).expect("Unresolved").code();
    let failed = SubStatus::from_code(21).expect("a failure").code();
    // Channel 3 has no subscription (0): it counts for nothing.
    let v = Tracker::default().update(&[ok(
        "app",
        status_line(1, &[RECEIVING, pending, 0, failed], &[0], None),
    )]);
    assert_eq!(v.colour, Colour::Orange);
    assert_eq!(
        v.tooltip,
        "dstack: 1 device, app: 2 of 3 subscriptions not connected"
    );
    assert_eq!(
        v.lines,
        vec!["app: clock dsync ready; RX 1/3 connected; late +0"]
    );
    // No subscription at all is fine.
    let v = Tracker::default().update(&[ok("app", status_line(1, &[0, 0], &[], None))]);
    assert_eq!(v.colour, Colour::Green);
}

#[test]
fn growing_late_counts_are_orange_for_one_update() {
    let mut t = Tracker::default();
    // The first sight shows no growth, whatever the counts so far.
    let v = t.update(&[ok(
        "app",
        status_line(1, &[RECEIVING], &[40, 2], Some((5, 6))),
    )]);
    assert_eq!(v.colour, Colour::Green);
    assert!(
        v.lines[0].ends_with("late +0; missed +0, overruns +0"),
        "{}",
        v.lines[0]
    );
    // RX late grows by 3 (summed over the flows).
    let v = t.update(&[ok(
        "app",
        status_line(1, &[RECEIVING], &[41, 4], Some((5, 6))),
    )]);
    assert_eq!(v.colour, Colour::Orange);
    assert_eq!(v.tooltip, "dstack: 1 device, app: late +3");
    assert!(
        v.lines[0].ends_with("late +3; missed +0, overruns +0"),
        "{}",
        v.lines[0]
    );
    // Unchanged: green again.
    let v = t.update(&[ok(
        "app",
        status_line(1, &[RECEIVING], &[41, 4], Some((5, 6))),
    )]);
    assert_eq!(v.colour, Colour::Green);
    // The engine's missed grows.
    let v = t.update(&[ok(
        "app",
        status_line(1, &[RECEIVING], &[41, 4], Some((7, 6))),
    )]);
    assert_eq!(v.tooltip, "dstack: 1 device, app: missed +2");
    assert!(
        v.lines[0].ends_with("late +0; missed +2, overruns +0"),
        "{}",
        v.lines[0]
    );
    // The engine's overruns grow.
    let v = t.update(&[ok(
        "app",
        status_line(1, &[RECEIVING], &[41, 4], Some((7, 9))),
    )]);
    assert_eq!(v.colour, Colour::Orange);
    assert_eq!(v.tooltip, "dstack: 1 device, app: overruns +3");
    // The device restarted (counts went down): no growth, a new baseline.
    let v = t.update(&[ok("app", status_line(1, &[RECEIVING], &[1], Some((0, 0))))]);
    assert_eq!(v.colour, Colour::Green);
    let v = t.update(&[ok("app", status_line(1, &[RECEIVING], &[2], Some((0, 0))))]);
    assert_eq!(v.tooltip, "dstack: 1 device, app: late +1");
}

#[test]
fn a_device_gone_and_back_starts_from_its_new_counts() {
    let mut t = Tracker::default();
    t.update(&[ok("app", status_line(1, &[], &[10], None))]);
    t.update(&[]);
    let v = t.update(&[ok("app", status_line(1, &[], &[50], None))]);
    assert_eq!(v.colour, Colour::Green);
}

#[test]
fn counts_survive_an_unreadable_status() {
    let mut t = Tracker::default();
    t.update(&[ok("app", status_line(1, &[], &[50], None))]);
    let v = t.update(&[("app".into(), Err("x".into()))]);
    assert_eq!(v.colour, Colour::Red);
    // What grew while it could not be read shows once it reads again.
    let v = t.update(&[ok("app", status_line(1, &[], &[90], None))]);
    assert_eq!(v.tooltip, "dstack: 1 device, app: late +40");
    // A device never read before an error has nothing to keep.
    let mut t = Tracker::default();
    t.update(&[("new".into(), Err("x".into()))]);
    let v = t.update(&[ok("new", status_line(1, &[], &[7], None))]);
    assert_eq!(v.colour, Colour::Green);
}

#[test]
fn two_devices_of_one_name_keep_their_own_counts() {
    let mut t = Tracker::default();
    t.update(&[
        ok("twin", status_line(1, &[], &[100], None)),
        ok("twin", status_line(1, &[], &[5], None)),
    ]);
    let v = t.update(&[
        ok("twin", status_line(1, &[], &[100], None)),
        ok("twin", status_line(1, &[], &[5], None)),
    ]);
    assert_eq!(v.colour, Colour::Green, "{:?}", v.lines);
    assert_eq!(v.lines.len(), 2);
    let readings = [
        ok("a", "{}".into()),
        ok("b", "{}".into()),
        ok("a", "{}".into()),
        ok("a", "{}".into()),
    ];
    assert_eq!(keys(&readings), vec!["a", "b", "a (2)", "a (3)"]);
    let v: serde_json::Value = serde_json::from_str(&copy_json(&readings)).expect("JSON");
    assert_eq!(v.as_object().expect("an object").len(), 4);
}

#[test]
fn counts_are_per_device() {
    let mut t = Tracker::default();
    t.update(&[
        ok("a", status_line(1, &[], &[10], None)),
        ok("b", status_line(1, &[], &[0], None)),
    ]);
    let v = t.update(&[
        ok("a", status_line(1, &[], &[10], None)),
        ok("b", status_line(1, &[], &[4], None)),
    ]);
    assert_eq!(v.tooltip, "dstack: 2 devices, b: late +4");
}

#[test]
fn the_worst_device_names_the_tooltip_and_the_first_of_equals() {
    let orange = status_line(1, &[1], &[], None);
    let red = status_line(0, &[], &[], None);
    let v = Tracker::default().update(&[
        ok("o1", orange.clone()),
        ok("r1", red.clone()),
        ok("o2", orange.clone()),
        ok("r2", red),
    ]);
    assert_eq!(v.colour, Colour::Red);
    assert_eq!(v.tooltip, "dstack: 4 devices, r1: clock not ready");
    assert_eq!(v.lines.len(), 4);
    let v = Tracker::default().update(&[
        ok("g", healthy()),
        ok("o1", orange.clone()),
        ok("o2", orange),
    ]);
    assert_eq!(v.colour, Colour::Orange);
    assert_eq!(
        v.tooltip,
        "dstack: 3 devices, o1: 1 of 1 subscriptions not connected"
    );
    let v = Tracker::default().update(&[ok("g1", healthy()), ok("g2", healthy())]);
    assert_eq!(v.tooltip, "dstack: 2 devices, ok");
}

#[test]
fn missing_fields_read_as_absent() {
    // No clock: not ready; no channels, flows or engine: nothing counted.
    let s = parse("{}").expect("an object");
    assert_eq!(
        s,
        Status {
            ready: false,
            source: None,
            holdover: false,
            subscribed: 0,
            connected: 0,
            counts: Counts::default(),
            engine: false,
            app: None,
        }
    );
    let v = Tracker::default().update(&[ok("bare", "{}".into())]);
    assert_eq!(v.colour, Colour::Red);
    assert_eq!(
        v.lines,
        vec!["bare: clock ? not ready; RX 0/0 connected; late +0"]
    );
    // Fields of the wrong kind are absent too; a channel without its
    // status counts for nothing, an engine without counts is an engine.
    let s = parse(
        r#"{"clock":{"ready":true,"holdover":1,"source":5},"channels":[{"rx":1},{"status":"x"},{"status":-1}],"rx":[{"flow":1},{"late":"x"},{"late":2}],"engine":{}}"#,
    )
    .expect("an object");
    assert!(s.ready && s.holdover && s.engine);
    assert_eq!(s.source, None);
    assert_eq!((s.subscribed, s.connected), (0, 0));
    assert_eq!(
        s.counts,
        Counts {
            late: 2,
            missed: 0,
            overruns: 0,
            send_errors: 0,
        }
    );
    // A flag of 0 or false is not set; "engine" that is no object is none.
    let s = parse(r#"{"clock":{"ready":false,"holdover":0},"engine":3}"#).expect("an object");
    assert!(!s.ready && !s.holdover && !s.engine);
    let s = parse(r#"{"clock":{"ready":2,"source":"system"}}"#).expect("an object");
    assert!(s.ready);
    assert_eq!(s.source.as_deref(), Some("system"));
}

#[test]
fn a_status_word_too_large_counts_as_subscribed_not_connected() {
    let s =
        parse(r#"{"clock":{"ready":1},"channels":[{"status":4294967296}]}"#).expect("an object");
    assert_eq!((s.subscribed, s.connected), (1, 0));
}

#[test]
fn the_line_shows_holdover_and_the_engine_only_when_there() {
    let s = Status {
        ready: true,
        source: Some("system".into()),
        holdover: true,
        subscribed: 4,
        connected: 4,
        counts: Counts::default(),
        engine: false,
        app: None,
    };
    let grew = Counts {
        late: 1,
        missed: 2,
        overruns: 3,
        send_errors: 0,
    };
    assert_eq!(
        line("d", &s, grew),
        "d: clock system ready, holdover; RX 4/4 connected; late +1"
    );
    let s = Status { engine: true, ..s };
    assert_eq!(
        line("d", &s, grew),
        "d: clock system ready, holdover; RX 4/4 connected; late +1; missed +2, overruns +3"
    );
}

#[test]
fn judge_weighs_the_clock_then_subscriptions_then_counts() {
    let s = Status {
        ready: false,
        source: None,
        holdover: false,
        subscribed: 2,
        connected: 1,
        counts: Counts::default(),
        engine: true,
        app: None,
    };
    let grew = Counts {
        late: 1,
        missed: 1,
        overruns: 1,
        send_errors: 0,
    };
    assert_eq!(
        judge(&s, grew),
        (Colour::Red, Some("clock not ready".into()))
    );
    let s = Status { ready: true, ..s };
    assert_eq!(
        judge(&s, grew),
        (
            Colour::Orange,
            Some("1 of 2 subscriptions not connected".into())
        )
    );
    let s = Status { connected: 2, ..s };
    assert_eq!(judge(&s, grew), (Colour::Orange, Some("late +1".into())));
    assert_eq!(judge(&s, Counts::default()), (Colour::Green, None));
}

#[test]
fn colours_order_worst_last() {
    assert!(Colour::Grey < Colour::Green);
    assert!(Colour::Green < Colour::Orange);
    assert!(Colour::Orange < Colour::Red);
}

#[test]
fn the_tooltip_is_clipped() {
    let name = "n".repeat(200);
    let v = Tracker::default().update(&[(name, Err("x".into()))]);
    assert_eq!(v.tooltip.chars().count(), TOOLTIP_MAX);
    assert!(v.tooltip.starts_with("dstack: 1 device, nnn"));
    assert_eq!(clip("ab\u{e9}d", 3), "ab\u{e9}");
    assert_eq!(clip("ab", 3), "ab");
    assert_eq!(clip("abc", 3), "abc");
    // A character outside the BMP is two UTF-16 units: never cut in half.
    assert_eq!(clip("ab\u{1f3b5}", 3), "ab");
    assert_eq!(clip("ab\u{1f3b5}", 4), "ab\u{1f3b5}");
    let v = Tracker::default().update(&[("\u{1f3b5}".repeat(100), Err("x".into()))]);
    assert!(v.tooltip.encode_utf16().count() <= TOOLTIP_MAX);
    assert!(v.tooltip.encode_utf16().count() >= TOOLTIP_MAX - 1);
}

#[test]
fn counts_since_never_go_below_0() {
    let now = Counts {
        late: 5,
        missed: 1,
        overruns: 9,
        send_errors: 7,
    };
    let before = Counts {
        late: 2,
        missed: 4,
        overruns: 9,
        send_errors: 3,
    };
    assert_eq!(
        now.since(before),
        Counts {
            late: 3,
            missed: 0,
            overruns: 0,
            send_errors: 4,
        }
    );
    // A device that restarted: its refused sends grew by 0.
    let restarted = Counts {
        send_errors: 1,
        ..Counts::default()
    };
    assert_eq!(restarted.since(before).send_errors, 0);
}

#[test]
fn copy_json_names_each_device() {
    let readings = vec![
        ok("a", r#"{"clock":{"ready":1}}"#.into()),
        ("b".to_string(), Err("no answer: refused".to_string())),
        ok("c", "garbage".into()),
    ];
    let v: serde_json::Value = serde_json::from_str(&copy_json(&readings)).expect("JSON");
    assert_eq!(v["a"]["clock"]["ready"], 1);
    assert_eq!(v["b"]["error"], "no answer: refused");
    assert_eq!(v["c"]["error"], "the status is not JSON");
    assert_eq!(copy_json(&[]), "{}");
}

/// `line` with the app part `app` (a JSON object's text) at its end.
fn with_app(line: String, app: &str) -> String {
    format!(
        "{},\"app\":{app}}}",
        line.strip_suffix('}').expect("an object")
    )
}

#[test]
fn the_app_part_reads_its_line_and_state() {
    let v = |t: &str| serde_json::from_str::<serde_json::Value>(t).expect("JSON");
    assert_eq!(
        app_of(&v(r#"{"line":"ASIO 64","state":"warn","x":1}"#)),
        Some(App {
            line: Some("ASIO 64".into()),
            state: Some(AppState::Warn),
            stale: false,
        })
    );
    assert_eq!(
        app_of(&v(r#"{"state":"ok"}"#)).and_then(|a| a.state),
        Some(AppState::Ok)
    );
    assert_eq!(
        app_of(&v(r#"{"state":"error"}"#)).and_then(|a| a.state),
        Some(AppState::Error)
    );
    // Unknown or wrong-kind fields read as absent; an empty line is none.
    assert_eq!(
        app_of(&v(r#"{"line":"","state":"ERROR"}"#)),
        Some(App {
            line: None,
            state: None,
            stale: false,
        })
    );
    assert_eq!(
        app_of(&v(r#"{"line":7,"state":true}"#)),
        Some(App {
            line: None,
            state: None,
            stale: false,
        })
    );
    // dstack's stale mark: only `true` counts.
    assert_eq!(
        app_of(&v(r#"{"state":"ok","stale":true}"#)),
        Some(App {
            line: None,
            state: Some(AppState::Ok),
            stale: true,
        })
    );
    for not_stale in [r#"{"stale":false}"#, r#"{"stale":1}"#, "{}"] {
        assert!(
            !app_of(&v(not_stale)).expect("an object").stale,
            "{not_stale}"
        );
    }
    assert_eq!(app_of(&v("[]")), None);
    assert_eq!(app_of(&v("\"ok\"")), None);
    // In a status: parsed; a non-object app is no app.
    let s = parse(&with_app(healthy(), r#"{"line":"x","state":"ok"}"#)).expect("status");
    assert_eq!(s.app.and_then(|a| a.line), Some("x".to_string()));
    let s = parse(&with_app(healthy(), "1")).expect("status");
    assert_eq!(s.app, None);
    assert_eq!(parse(&healthy()).expect("status").app, None);
}

#[test]
fn the_app_line_follows_the_device_s_and_its_state_colours_the_device() {
    let line_ok = with_app(healthy(), r#"{"line":"ASIO 64 attached","state":"ok"}"#);
    let v = Tracker::default().update(&[ok("app", line_ok)]);
    assert_eq!(v.colour, Colour::Green);
    assert_eq!(v.tooltip, "dstack: 1 device, ok");
    assert_eq!(
        v.lines,
        vec![
            "app: clock dsync ready; RX 2/2 connected; late +0; missed +0, overruns +0; ASIO 64 attached"
        ]
    );
    let warn = with_app(healthy(), r#"{"line":"no DAW","state":"warn"}"#);
    let v = Tracker::default().update(&[ok("app", warn)]);
    assert_eq!(v.colour, Colour::Orange);
    assert_eq!(v.tooltip, "dstack: 1 device, app: app warning: no DAW");
    let error = with_app(healthy(), r#"{"state":"error"}"#);
    let v = Tracker::default().update(&[ok("app", error)]);
    assert_eq!(v.colour, Colour::Red);
    assert_eq!(v.tooltip, "dstack: 1 device, app: app error");
    assert_eq!(
        v.lines,
        vec!["app: clock dsync ready; RX 2/2 connected; late +0; missed +0, overruns +0"]
    );
    // No state, or an unknown one: the device's own colour.
    let none = with_app(healthy(), r#"{"line":"hi"}"#);
    assert_eq!(
        Tracker::default().update(&[ok("app", none)]).colour,
        Colour::Green
    );
}

#[test]
fn judge_weighs_the_app_s_error_after_the_clock_and_its_warning_after_subscriptions() {
    let app = |state| {
        Some(App {
            line: Some("l".into()),
            state: Some(state),
            stale: false,
        })
    };
    let s = Status {
        ready: true,
        source: None,
        holdover: false,
        subscribed: 2,
        connected: 1,
        counts: Counts::default(),
        engine: false,
        app: app(AppState::Error),
    };
    let grew = Counts {
        late: 1,
        ..Counts::default()
    };
    assert_eq!(
        judge(
            &Status {
                ready: false,
                ..s.clone()
            },
            grew
        ),
        (Colour::Red, Some("clock not ready".into()))
    );
    assert_eq!(judge(&s, grew), (Colour::Red, Some("app error: l".into())));
    let warn = Status {
        app: app(AppState::Warn),
        ..s.clone()
    };
    assert_eq!(
        judge(&warn, grew),
        (
            Colour::Orange,
            Some("1 of 2 subscriptions not connected".into())
        )
    );
    let warn = Status {
        connected: 2,
        ..warn
    };
    assert_eq!(
        judge(&warn, grew),
        (Colour::Orange, Some("app warning: l".into()))
    );
    let fine = Status {
        app: app(AppState::Ok),
        ..warn
    };
    let stale = |state| Status {
        app: Some(App {
            line: Some("l".into()),
            state: Some(state),
            stale: true,
        }),
        ..fine.clone()
    };
    // A stale part warns after the app's own warning, before the counts.
    assert_eq!(
        judge(&stale(AppState::Ok), grew),
        (Colour::Orange, Some("app status stale: l".into()))
    );
    assert_eq!(
        judge(&stale(AppState::Warn), grew),
        (Colour::Orange, Some("app warning: l".into()))
    );
    assert_eq!(
        judge(&stale(AppState::Error), grew),
        (Colour::Red, Some("app error: l".into()))
    );
    assert_eq!(judge(&fine, grew), (Colour::Orange, Some("late +1".into())));
    assert_eq!(judge(&fine, Counts::default()), (Colour::Green, None));
}

#[test]
fn an_expected_device_not_running_is_red_and_one_found_is_as_usual() {
    let expected = vec!["App".to_string(), "Stopped one".to_string()];
    // Found case aside: as usual; the other is missing.
    let v = Tracker::default().update_with(&[ok("app", healthy())], &expected);
    assert_eq!(v.colour, Colour::Red);
    assert_eq!(
        v.tooltip,
        format!("dstack: 2 devices, Stopped one: {NOT_RUNNING}")
    );
    assert_eq!(v.lines.len(), 2);
    assert_eq!(v.lines[1], "Stopped one: expected, not running");
    // Nothing running at all: red, not grey.
    let v = Tracker::default().update_with(&[], &expected[1..]);
    assert_eq!(
        v,
        View {
            colour: Colour::Red,
            tooltip: "dstack: 1 device, Stopped one: expected, not running".into(),
            lines: vec!["Stopped one: expected, not running".into()],
        }
    );
    // Everything expected is running: the view of the readings alone.
    let all = Tracker::default().update_with(&[ok("app", healthy())], &expected[..1]);
    assert_eq!(all, Tracker::default().update(&[ok("app", healthy())]));
    // A red device first keeps the tooltip's first reason.
    let v = Tracker::default().update_with(
        &[("gone".to_string(), Err("x".to_string()))],
        &expected[1..],
    );
    assert_eq!(v.tooltip, "dstack: 2 devices, gone: status unreadable");
    // An orange device first: the missing one's red wins.
    let orange = status_line(1, &[0x0000_0001], &[], None);
    let v = Tracker::default().update_with(&[ok("app", orange)], &expected[1..]);
    assert_eq!(v.colour, Colour::Red);
    assert_eq!(
        v.tooltip,
        "dstack: 2 devices, Stopped one: expected, not running"
    );
}

#[test]
fn missing_names_are_the_expected_no_reading_has() {
    let readings = vec![ok("Alpha", healthy()), ("beta".into(), Err("x".into()))];
    let expected: Vec<String> = ["ALPHA", "gamma", "Beta", "delta"]
        .map(String::from)
        .to_vec();
    assert_eq!(missing(&readings, &expected), vec!["gamma", "delta"]);
    assert!(missing(&readings, &[]).is_empty());
    assert_eq!(missing(&[], &expected).len(), 4);
    assert_eq!(missing_line("x"), "x: expected, not running");
}

/// [`status_line`] with TX lanes whose `send_errors` are `errors`.
fn with_tx(errors: &[u64]) -> String {
    let lanes: Vec<String> = errors
        .iter()
        .enumerate()
        .map(|(i, e)| {
            format!(
                r#"{{"flow":{},"live":1,"packets":9,"send_errors":{e},"send_retries":4}}"#,
                i + 1
            )
        })
        .collect();
    healthy().replace(r#""tx":[]"#, &format!(r#""tx":[{}]"#, lanes.join(",")))
}

#[test]
fn growing_send_errors_are_orange_and_shown_on_the_line() {
    let mut t = Tracker::default();
    // The first sight: no growth, whatever was refused before.
    let v = t.update(&[ok("app", with_tx(&[5, 2]))]);
    assert_eq!(v.colour, Colour::Green);
    assert!(!v.lines[0].contains("send errors"), "{}", v.lines[0]);
    // Two lanes refuse 3 more between them: orange, `send errors +3`.
    let v = t.update(&[ok("app", with_tx(&[7, 3]))]);
    assert_eq!(v.colour, Colour::Orange);
    assert_eq!(v.tooltip, "dstack: 1 device, app: send errors +3");
    assert_eq!(
        v.lines[0],
        "app: clock dsync ready; RX 2/2 connected; late +0; missed +0, overruns +0; send errors +3"
    );
    // Unchanged (retries alone are healthy): green, no send errors shown.
    let v = t.update(&[ok("app", with_tx(&[7, 3]))]);
    assert_eq!(v.colour, Colour::Green);
    assert!(!v.lines[0].contains("send errors"), "{}", v.lines[0]);
    // One more: +1.
    let v = t.update(&[ok("app", with_tx(&[7, 4]))]);
    assert!(v.lines[0].ends_with("; send errors +1"), "{}", v.lines[0]);
    // A lane's count that is no number counts for nothing.
    let s = parse(&healthy().replace(
        r#""tx":[]"#,
        r#""tx":[{"send_errors":"x"},{"flow":2},{"send_errors":6}]"#,
    ))
    .expect("an object");
    assert_eq!(s.counts.send_errors, 6);
}

#[test]
fn judge_weighs_send_errors_after_the_late_counts() {
    let s = parse(&healthy()).expect("an object");
    let grew = Counts {
        send_errors: 2,
        ..Counts::default()
    };
    assert_eq!(
        judge(&s, grew),
        (Colour::Orange, Some("send errors +2".into()))
    );
    let grew = Counts {
        overruns: 1,
        ..grew
    };
    assert_eq!(
        judge(&s, grew),
        (Colour::Orange, Some("overruns +1".into()))
    );
}
