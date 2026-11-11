// SPDX-License-Identifier: MIT OR Apache-2.0

use dstack::arc::control::Settings;

use super::{Choice, DANTE_NS, choices, label, ns_of_ms, read_back};

fn settings(min: Option<u32>, max: Option<u32>, configured: Option<u32>) -> Settings {
    Settings {
        latency_min_ns: min,
        latency_max_ns: max,
        latency_configured_ns: configured,
        ..Settings::default()
    }
}

fn values(c: &[Choice]) -> Vec<u32> {
    c.iter().map(|c| c.ns).collect()
}

#[test]
fn labels_are_milliseconds_without_trailing_zeros() {
    assert_eq!(label(250_000), "0.25 ms");
    assert_eq!(label(500_000), "0.5 ms");
    assert_eq!(label(1_000_000), "1 ms");
    assert_eq!(label(10_000_000), "10 ms");
    assert_eq!(label(1_500_001), "1.500001 ms");
    assert_eq!(label(0), "0 ms");
    let all: Vec<String> = DANTE_NS.iter().map(|&ns| label(ns)).collect();
    assert_eq!(all, ["0.25 ms", "0.5 ms", "1 ms", "2 ms", "5 ms", "10 ms"]);
}

#[test]
fn choices_are_dante_s_values_within_the_range_both_ends_included() {
    let c = choices(&settings(Some(1_000_000), Some(5_000_000), Some(2_000_000)));
    assert_eq!(
        c,
        vec![
            Choice {
                ns: 1_000_000,
                label: "1 ms".into(),
                checked: false
            },
            Choice {
                ns: 2_000_000,
                label: "2 ms".into(),
                checked: true
            },
            Choice {
                ns: 5_000_000,
                label: "5 ms".into(),
                checked: false
            },
        ]
    );
    // Just inside or outside an end.
    let c = choices(&settings(Some(250_001), Some(9_999_999), None));
    assert_eq!(values(&c), vec![500_000, 1_000_000, 2_000_000, 5_000_000]);
    assert!(c.iter().all(|c| !c.checked));
    let c = choices(&settings(Some(0), Some(u32::MAX), Some(250_000)));
    assert_eq!(values(&c), DANTE_NS.to_vec());
    assert_eq!(c.iter().filter(|c| c.checked).count(), 1);
    assert!(c[0].checked);
    // A configured value that is none of Dante's checks nothing.
    let c = choices(&settings(Some(0), Some(u32::MAX), Some(3_000_000)));
    assert!(c.iter().all(|c| !c.checked));
}

#[test]
fn no_range_no_choice() {
    assert!(choices(&settings(None, Some(10_000_000), Some(1_000_000))).is_empty());
    assert!(choices(&settings(Some(250_000), None, Some(1_000_000))).is_empty());
    assert!(choices(&settings(None, None, None)).is_empty());
    // A range that holds none of the values.
    assert!(choices(&settings(Some(3_000_000), Some(4_000_000), None)).is_empty());
}

#[test]
fn the_command_line_takes_the_menu_s_labels_without_ms() {
    let named: Vec<Option<u32>> = ["0.25", "0.5", "1", "2", "5", "10"]
        .iter()
        .map(|ms| ns_of_ms(ms))
        .collect();
    let all: Vec<Option<u32>> = DANTE_NS.iter().map(|&ns| Some(ns)).collect();
    assert_eq!(named, all);
}

#[test]
fn the_command_line_takes_any_decimal_ms_rounded_to_whole_us() {
    // dstack #213: a value that is none of Dante's (dasio-test-1's 0.333).
    for (ms, ns) in [
        ("0.333", 333_000),
        ("0", 0),
        ("3", 3_000_000),
        ("1.0", 1_000_000),
        ("0.50", 500_000),
        ("12.345", 12_345_000),
        ("007", 7_000_000),
    ] {
        assert_eq!(ns_of_ms(ms), Some(ns), "{ms:?}");
    }
    // Rounded to whole µs at the fourth decimal, half up.
    assert_eq!(ns_of_ms("0.3334"), Some(333_000));
    assert_eq!(ns_of_ms("0.3335"), Some(334_000));
    assert_eq!(ns_of_ms("0.33349"), Some(333_000));
    assert_eq!(ns_of_ms("0.9995"), Some(1_000_000));
    assert_eq!(ns_of_ms("1.0009"), Some(1_001_000));
    assert_eq!(ns_of_ms("0.123456789"), Some(123_000));
    assert_eq!(ns_of_ms("0.0005"), Some(1_000));
    assert_eq!(ns_of_ms("0.0004"), Some(0));
    // The largest value a u32 of ns holds, and one µs past it.
    assert_eq!(ns_of_ms("4294.967"), Some(4_294_967_000));
    assert_eq!(ns_of_ms("4294.968"), None);
    assert_eq!(ns_of_ms("250000"), None);
    assert_eq!(ns_of_ms("99999999999999999999999"), None);
}

#[test]
fn junk_is_no_value() {
    for ms in [
        "", ".", ".5", "1.", "1 ms", "1ms", "-1", "+1", "x", "1e3", " 1", "1 ", "1.2.3", "0,5",
        "0x10", "1_000", "\u{0661}",
    ] {
        assert_eq!(ns_of_ms(ms), None, "{ms:?}");
    }
}

#[test]
fn the_read_back_is_the_verdict() {
    assert_eq!(
        read_back(2_000_000, Some(2_000_000)),
        Ok("the latency is set to 2 ms; it reads back 2 ms".to_string())
    );
    assert_eq!(
        read_back(2_000_000, Some(1_000_000)),
        Err("the latency 2 ms was written, but it reads back 1 ms".to_string())
    );
    assert_eq!(
        read_back(2_000_000, None),
        Err(
            "the latency 2 ms was written, but the device reads back no configured latency"
                .to_string()
        )
    );
}
