// SPDX-License-Identifier: MIT OR Apache-2.0

use dstack::arc::control::Settings;

use super::{Choice, DANTE_NS, choices, label, ns_of_ms, pick, read_back};

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
    let named: Vec<Option<u64>> = ["0.25", "0.5", "1", "2", "5", "10"]
        .iter()
        .map(|ms| ns_of_ms(ms))
        .collect();
    let all: Vec<Option<u64>> = DANTE_NS.iter().map(|&ns| Some(u64::from(ns))).collect();
    assert_eq!(named, all);
}

#[test]
fn the_command_line_takes_any_decimal_ms_to_whole_ns() {
    // dstack #213: a value that is none of Dante's, and a device's own
    // configured 333334 ns, restored exactly.
    for (ms, ns) in [
        ("0.333", 333_000),
        ("0.333334", 333_334),
        ("0.123456", 123_456),
        ("0", 0),
        ("3", 3_000_000),
        ("1.0", 1_000_000),
        ("0.50", 500_000),
        ("12.345", 12_345_000),
        ("007", 7_000_000),
        ("0.3334", 333_400),
        ("0.000001", 1),
    ] {
        assert_eq!(ns_of_ms(ms), Some(ns), "{ms:?}");
    }
    // Past six decimals: rounded to whole ns at the seventh, half up.
    assert_eq!(ns_of_ms("0.3333334"), Some(333_333));
    assert_eq!(ns_of_ms("0.3333335"), Some(333_334));
    assert_eq!(ns_of_ms("0.33333349"), Some(333_333));
    assert_eq!(ns_of_ms("0.9999995"), Some(1_000_000));
    assert_eq!(ns_of_ms("1.0000009"), Some(1_000_001));
    assert_eq!(ns_of_ms("0.123456789"), Some(123_457));
    assert_eq!(ns_of_ms("0.0000005"), Some(1));
    assert_eq!(ns_of_ms("0.0000004"), Some(0));
    // Past a u32 of ns is still a value (outside any range, `pick`); past
    // a u64 is none.
    assert_eq!(ns_of_ms("4294.967"), Some(4_294_967_000));
    assert_eq!(ns_of_ms("4294.968"), Some(4_294_968_000));
    assert_eq!(ns_of_ms("250000"), Some(250_000_000_000));
    assert_eq!(
        ns_of_ms("18446744073709.551"),
        Some(18_446_744_073_709_551_000)
    );
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
fn junk_after_the_point_and_a_huge_whole_part_are_no_value() {
    for ms in [
        "1.x",
        "1.5a",
        "0. 5",
        "18446744073709551.999",
        "18446744073709552",
    ] {
        assert_eq!(ns_of_ms(ms), None, "{ms:?}");
    }
}

#[test]
fn any_value_inside_the_device_s_range_is_picked_both_ends_included() {
    let s = settings(Some(333_000), Some(10_000_000), Some(1_000_000));
    // Dante's values inside the range, as before (dstack #104).
    for ms in ["0.5", "1", "2", "5", "10"] {
        let ns = u32::try_from(ns_of_ms(ms).unwrap()).unwrap();
        assert_eq!(pick(&s, ms), Ok(ns), "{ms:?}");
    }
    // Any other (dstack #213), the ends included.
    assert_eq!(pick(&s, "0.333"), Ok(333_000));
    assert_eq!(pick(&s, "3"), Ok(3_000_000));
    assert_eq!(pick(&s, "9.999"), Ok(9_999_000));
    let outside = |shown: &str| {
        Err(format!(
            "{shown} is outside the latency range the device states (0.333 ms to 10 ms); \
             nothing written"
        ))
    };
    assert_eq!(pick(&s, "0.25"), outside("0.25 ms"));
    assert_eq!(pick(&s, "0.332"), outside("0.332 ms"));
    assert_eq!(pick(&s, "10.001"), outside("10.001 ms"));
    assert_eq!(pick(&s, "0"), outside("0 ms"));
    // A value past a u32 of ns is outside too, not junk.
    assert_eq!(pick(&s, "5000"), outside("5000 ms"));
    assert_eq!(pick(&s, "4294.9673"), outside("4294.9673 ms"));
    assert_eq!(pick(&s, "4294.968"), outside("4294.968 ms"));
    let all = settings(Some(0), Some(u32::MAX), None);
    assert_eq!(pick(&all, "4294.967"), Ok(4_294_967_000));
    assert_eq!(pick(&all, "4294.967295"), Ok(u32::MAX));
    assert_eq!(
        pick(&all, "4294.9672955"),
        Err(
            "4294.967296 ms is outside the latency range the device states \
             (0 ms to 4294.967295 ms); nothing written"
                .to_string()
        )
    );
    assert_eq!(
        pick(&all, "4294.968"),
        Err(
            "4294.968 ms is outside the latency range the device states \
             (0 ms to 4294.967295 ms); nothing written"
                .to_string()
        )
    );
    assert_eq!(
        pick(&s, "0,5"),
        Err("0,5 is not a latency in ms (such as 0.5 or 0.333); nothing written".to_string())
    );
    // One ns off an end.
    let s = settings(Some(333_001), Some(9_999_999), None);
    assert_eq!(
        pick(&s, "0.333"),
        Err("0.333 ms is outside the latency range the device states \
             (0.333001 ms to 9.999999 ms); nothing written"
            .to_string())
    );
    assert!(pick(&s, "10").is_err());
    assert_eq!(pick(&s, "0.334"), Ok(334_000));
    // The ends to the ns (a device's own configured value, dstack #213).
    assert_eq!(pick(&s, "0.333001"), Ok(333_001));
    assert_eq!(pick(&s, "9.999999"), Ok(9_999_999));
    assert_eq!(
        pick(&s, "9.9999995"),
        Err("10 ms is outside the latency range the device states \
             (0.333001 ms to 9.999999 ms); nothing written"
            .to_string())
    );
}

#[test]
fn no_range_no_pick() {
    let none = Err("the device names no latency range; nothing written".to_string());
    assert_eq!(pick(&settings(None, Some(10_000_000), None), "1"), none);
    assert_eq!(pick(&settings(Some(250_000), None, None), "1"), none);
    assert_eq!(pick(&settings(None, None, None), "x"), none);
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
