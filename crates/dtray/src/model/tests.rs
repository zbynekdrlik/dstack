// SPDX-License-Identifier: MIT OR Apache-2.0

use dstack::subscribe::SubStatus;

use super::{
    Colour, Counts, Status, TOOLTIP_MAX, Tracker, View, clip, copy_json, judge, line, parse,
    print_text,
};
use crate::discover::Reading;

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
    // An unreadable reading forgets the counts too.
    t.update(&[("app".into(), Err("x".into()))]);
    let v = t.update(&[ok("app", status_line(1, &[], &[90], None))]);
    assert_eq!(v.colour, Colour::Green);
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
            overruns: 0
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
    };
    let grew = Counts {
        late: 1,
        missed: 2,
        overruns: 3,
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
    };
    let grew = Counts {
        late: 1,
        missed: 1,
        overruns: 1,
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
}

#[test]
fn counts_since_never_go_below_0() {
    let now = Counts {
        late: 5,
        missed: 1,
        overruns: 9,
    };
    let before = Counts {
        late: 2,
        missed: 4,
        overruns: 9,
    };
    assert_eq!(
        now.since(before),
        Counts {
            late: 3,
            missed: 0,
            overruns: 0
        }
    );
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
