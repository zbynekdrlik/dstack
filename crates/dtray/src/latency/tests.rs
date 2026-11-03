// SPDX-License-Identifier: MIT OR Apache-2.0

use dstack::arc::control::Settings;

use super::{Choice, DANTE_NS, choices, label};

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
