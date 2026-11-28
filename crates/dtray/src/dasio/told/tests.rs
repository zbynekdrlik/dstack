// SPDX-License-Identifier: MIT OR Apache-2.0

use serde_json::json;

use super::*;
use crate::dasio::Bridge;

/// dasio's own test vector (`latencies_follow_the_device_s_timing`): B 32,
/// P 1 ms = 96, W 64, T 250 us = 24 at 96 kHz.
fn vector() -> Fields {
    Fields {
        name: "test-asio".into(),
        iface: None,
        block: 32,
        work_margin: 64,
        latency_ns: 1_000_000,
        tx_latency_ns: Some(250_000),
        sample_rate: 96_000,
        rx: vec!["1".into(), "2".into()],
        tx: vec!["1".into(), "2".into()],
        cpus: Default::default(),
        bridge: None,
    }
}

/// A dasio status line (the app part as dasio's `app_status::json`
/// writes it, the keys this module reads).
fn status(input: u64, output: u64, spare: u64) -> String {
    json!({
        "source": "dsync",
        "ready": true,
        "app": {
            "line": "DAW attached · 32 @ 96 kHz · in 1.33 / out 1.25 ms",
            "block": 32,
            "sample_rate": 96000,
            "latency_in": input,
            "latency_out": output,
            "spare_blocks": spare,
        }
    })
    .to_string()
}

#[test]
fn the_terms_add_up_as_dasio_s_latencies() {
    let f = vector();
    let t = Terms::of(&f, 0);
    assert_eq!(
        t,
        Terms {
            block: 32,
            receive: 96,
            reserve: 64,
            spare: 0,
            send: 24
        }
    );
    assert_eq!((t.input(), t.output()), (32 + 96, 32 + 64 + 24));
    // A spare block: the outputs play one block later.
    let t = Terms::of(&f, 1);
    assert_eq!((t.input(), t.output()), (32 + 96, 32 + 64 + 24 + 32));
    let t = Terms::of(&f, 2);
    assert_eq!(t.output(), 32 + 64 + 24 + 64);
}

#[test]
fn the_send_latency_follows_the_receive_latency_when_not_set() {
    let mut f = vector();
    f.tx_latency_ns = None;
    let t = Terms::of(&f, 0);
    assert_eq!(t.send, 96);
    assert_eq!(t.output(), 32 + 64 + 96);
}

#[test]
fn a_latency_is_its_nearest_whole_samples() {
    // dasio's `a_latency_in_ns_is_its_nearest_whole_samples_...`: 333 333
    // ns at 96 kHz is 32 samples, 666 666 ns 64.
    let mut f = vector();
    f.latency_ns = 333_333;
    f.tx_latency_ns = Some(666_666);
    let t = Terms::of(&f, 0);
    assert_eq!((t.input(), t.output()), (32 + 32, 32 + 64 + 64));
}

#[test]
fn the_lines_name_every_term_in_ms() {
    let t = Terms::of(&vector(), 1);
    assert_eq!(
        input_line(&t, 96_000),
        "Input 1.33 ms = DAW buffer 0.33 ms + Dante receive 1.00 ms"
    );
    assert_eq!(
        output_line(&t, 96_000),
        "Output 1.58 ms = DAW buffer 0.33 ms + app reserve 0.67 ms \
         + spare buffers 1 x 0.33 ms + Dante send 0.25 ms"
    );
}

#[test]
fn the_running_figures_come_from_the_app_part() {
    let s = status(128, 120, 0);
    assert_eq!(
        running(&s, 48_000),
        Some(Running {
            input: 128,
            output: 120,
            rate: 96_000
        })
    );
    assert_eq!(spare(&s), Some(0));
    assert_eq!(spare(&status(1, 2, 3)), Some(3));
    // No sample rate in the app part: the network's.
    let no_rate = json!({"app": {"latency_in": 1, "latency_out": 2}}).to_string();
    assert_eq!(
        running(&no_rate, 48_000),
        Some(Running {
            input: 1,
            output: 2,
            rate: 48_000
        })
    );
    for partial in [
        json!({"app": {"latency_out": 2}}),
        json!({"app": {"latency_in": 1}}),
        json!({"app": {"driver": "x"}}),
    ] {
        assert_eq!(running(&partial.to_string(), 96_000), None, "{partial}");
    }
    assert_eq!(running("not json", 96_000), None);
    assert_eq!(spare("{}"), None);
}

#[test]
fn the_told_text_matches_a_running_instance_or_says_it_differs() {
    let f = vector();
    let same = told_text(&f, Some(&status(128, 120, 0)));
    assert_eq!(
        same,
        "Input 1.33 ms = DAW buffer 0.33 ms + Dante receive 1.00 ms\r\n\
         Output 1.25 ms = DAW buffer 0.33 ms + app reserve 0.67 ms \
         + spare buffers 0 x 0.33 ms + Dante send 0.25 ms\r\n\
         dasio tells Ableton the same now (in 1.33 / out 1.25 ms)."
    );
    // The spare buffer comes from the status and counts in the sum.
    let spare = told_text(&f, Some(&status(128, 152, 1)));
    assert!(spare.contains("Output 1.58 ms"), "{spare}");
    assert!(
        spare.ends_with("the same now (in 1.33 / out 1.58 ms)."),
        "{spare}"
    );
    // A value chosen that is not applied yet.
    let mut more = f.clone();
    more.work_margin = 128;
    let differs = told_text(&more, Some(&status(128, 120, 0)));
    assert!(differs.contains("Output 1.92 ms"), "{differs}");
    assert!(
        differs.ends_with(
            "dasio tells Ableton in 1.33 / out 1.25 ms now; the values above apply at its \
             next restart."
        ),
        "{differs}"
    );
    // An input that differs alone is a difference too.
    let input = told_text(&f, Some(&status(129, 120, 0)));
    assert!(input.contains("in 1.34 / out 1.25 ms now;"), "{input}");
}

#[test]
fn the_told_text_without_a_status_counts_no_spare_buffer() {
    let f = vector();
    let down = told_text(&f, None);
    assert!(down.contains("Output 1.25 ms"), "{down}");
    assert!(
        down.ends_with(
            "Spare buffers not known: counted as 0.\r\ndasio is not running: nothing to compare."
        ),
        "{down}"
    );
    let unsaid = told_text(&f, Some("{}"));
    assert!(
        unsaid.ends_with(
            "Spare buffers not known: counted as 0.\r\n\
             dasio's status does not say what it tells Ableton."
        ),
        "{unsaid}"
    );
}

#[test]
fn a_bridge_instance_shows_only_what_dasio_reports() {
    let mut f = vector();
    f.bridge = Some(Bridge {
        driver: "Test ASIO".into(),
        to_asio: vec![1, 2],
        from_asio: vec![1, 2],
        app_buffer_ns: None,
        to_asio_app_buffer_ns: None,
        from_asio_app_buffer_ns: None,
        converter_taps: None,
    });
    let lead = "A bridge instance adds its own conversion to the ASIO driver, so no sum is \
                shown here. ";
    assert_eq!(
        told_text(&f, Some(&status(500, 900, 0))),
        format!("{lead}dasio reports in 5.21 / out 9.38 ms now.")
    );
    let bridge_status = json!({"app": {"kind": "bridge", "driver": "Test ASIO"}}).to_string();
    assert_eq!(
        told_text(&f, Some(&bridge_status)),
        format!("{lead}dasio's status does not report it for a bridge instance.")
    );
    assert_eq!(told_text(&f, None), format!("{lead}dasio is not running."));
}

#[test]
fn the_spare_buffers_read_from_the_status() {
    let f = vector();
    assert_eq!(spare_text(&f, Some(&status(1, 2, 1))), "1 (adds 0.33 ms)");
    assert_eq!(spare_text(&f, Some(&status(1, 2, 0))), "0 (adds 0.00 ms)");
    assert_eq!(spare_text(&f, Some(&status(1, 2, 3))), "3 (adds 1.00 ms)");
    assert_eq!(
        spare_text(&f, Some("{}")),
        "not known (the status does not say)"
    );
    assert_eq!(spare_text(&f, None), "not known (dasio is not running)");
}

#[test]
fn each_latency_field_says_what_it_adds() {
    assert!(hint(Field::Block).contains("adds to the input and the output"));
    assert!(hint(Field::WorkMargin).contains("adds to the output"));
    assert!(hint(Field::RxLatency).contains("adds to the input"));
    assert!(hint(Field::TxLatency).contains("when not set: the receive latency"));
    assert_eq!(hint(Field::Inputs), "");
    assert_eq!(hint(Field::Iface), "");
    assert!(SPARE_HINT.contains("adds one DAW buffer to the output"));
}
