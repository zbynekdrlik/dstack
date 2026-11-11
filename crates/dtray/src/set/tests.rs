// SPDX-License-Identifier: MIT OR Apache-2.0

use std::cell::RefCell;
use std::net::{Ipv4Addr, SocketAddrV4};

use dstack::arc::control::Settings;

use super::{find, latency};
use crate::discover::Local;

// Documentation addresses (RFC 5737) only.
const A: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 10), 4440);
const B: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 10), 4441);

fn local(name: &str, arc: SocketAddrV4) -> Local {
    Local {
        name: name.to_string(),
        port: 5000,
        arc,
    }
}

/// A device offering 0.5 to 5 ms, configured at `configured`.
fn settings(configured: Option<u32>) -> Settings {
    Settings {
        latency_min_ns: Some(500_000),
        latency_max_ns: Some(5_000_000),
        latency_configured_ns: configured,
        ..Settings::default()
    }
}

fn two() -> Vec<Local> {
    vec![local("Test Dev", A), local("other", B)]
}

#[test]
fn the_device_is_found_by_its_whole_name_in_any_case() {
    let devices = two();
    assert_eq!(find(&devices, "Test Dev"), Ok(&devices[0]));
    assert_eq!(find(&devices, "test dev"), Ok(&devices[0]));
    assert_eq!(find(&devices, "TEST DEV"), Ok(&devices[0]));
    assert_eq!(find(&devices, "OTHER"), Ok(&devices[1]));
    for name in ["Test", "Test Dev ", "est Dev", ""] {
        assert_eq!(
            find(&devices, name),
            Err(format!(
                "no dstack device named {name} on this PC (found: Test Dev, other)"
            )),
            "{name:?}"
        );
    }
    assert_eq!(
        find(&[], "x"),
        Err("no dstack device named x on this PC (found: none)".to_string())
    );
    let same = vec![local("Dev", A), local("dev", B), local("third", B)];
    assert_eq!(
        find(&same, "DEV"),
        Err("2 dstack devices on this PC are named DEV; nothing written".to_string())
    );
}

#[test]
fn a_new_value_is_written_and_read_back() {
    let reads = RefCell::new(vec![settings(Some(1_000_000)), settings(Some(2_000_000))]);
    let mut asked = Vec::new();
    let mut read_from = Vec::new();
    let r = latency(
        &two(),
        "test dev",
        "2",
        |arc| {
            read_from.push(arc);
            Ok(reads.borrow_mut().remove(0))
        },
        |arc, ns| {
            asked.push((arc, ns));
            Ok(settings(Some(ns)))
        },
    );
    assert_eq!(
        r,
        Ok("Test Dev: the latency is set to 2 ms; it reads back 2 ms".to_string())
    );
    assert_eq!(asked, vec![(A, 2_000_000)]);
    // Before and after the write, both from the device's own ARC.
    assert_eq!(read_from, vec![A, A]);
}

#[test]
fn the_value_already_configured_writes_nothing() {
    let r = latency(
        &two(),
        "Test Dev",
        "1",
        |_| Ok(settings(Some(1_000_000))),
        |_, _| -> Result<Settings, String> { panic!("no write for the value already set") },
    );
    assert_eq!(
        r,
        Ok("Test Dev: the latency is already 1 ms; nothing written".to_string())
    );
}

#[test]
fn no_such_device_reads_and_writes_nothing() {
    let r = latency(
        &two(),
        "nobody",
        "1",
        |_| -> Result<Settings, String> { panic!("no read") },
        |_, _| -> Result<Settings, String> { panic!("no write") },
    );
    assert_eq!(
        r,
        Err("no dstack device named nobody on this PC (found: Test Dev, other)".to_string())
    );
}

#[test]
fn an_unreadable_device_writes_nothing() {
    let r = latency(
        &two(),
        "Test Dev",
        "1",
        |_| Err("no answer".to_string()),
        |_, _| -> Result<Settings, String> { panic!("no write") },
    );
    assert_eq!(
        r,
        Err("Test Dev: its settings were not read: no answer".to_string())
    );
}

#[test]
fn a_value_inside_the_range_is_written_both_ends_included() {
    // dstack #213: a value that is none of Dante's, and the range's ends.
    for (ms, ns, shown) in [
        ("3", 3_000_000, "3 ms"),
        ("0.5", 500_000, "0.5 ms"),
        ("5", 5_000_000, "5 ms"),
        ("4.9999", 4_999_900, "4.9999 ms"),
        ("4.9999995", 5_000_000, "5 ms"),
        ("0.4999995", 500_000, "0.5 ms"),
        ("0.6666667", 666_667, "0.666667 ms"),
    ] {
        let mut asked = Vec::new();
        let r = latency(
            &two(),
            "Test Dev",
            ms,
            |_| Ok(settings(Some(1_000_000))),
            |arc, ns| {
                asked.push((arc, ns));
                Ok(settings(Some(ns)))
            },
        );
        // The second read (after the write) answers as the device would.
        assert_eq!(asked, vec![(A, ns)], "{ms:?}");
        assert_eq!(
            r,
            Err(format!(
                "Test Dev: the latency {shown} was written, but it reads back 1 ms"
            )),
            "{ms:?}"
        );
    }
    let reads = RefCell::new(vec![settings(Some(1_000_000)), settings(Some(333_000))]);
    let r = latency(
        &two(),
        "Test Dev",
        "0.333",
        |_| {
            Ok(Settings {
                latency_min_ns: Some(333_000),
                ..reads.borrow_mut().remove(0)
            })
        },
        |_, ns| Ok(settings(Some(ns))),
    );
    assert_eq!(
        r,
        Ok("Test Dev: the latency is set to 0.333 ms; it reads back 0.333 ms".to_string())
    );
}

#[test]
fn a_device_s_own_sub_us_latency_is_restored_exactly() {
    // dstack #213: a device configured at 333334 ns states that as its
    // minimum; it is set to 1 ms, then back to 0.333334 ms.
    let own = |configured| Settings {
        latency_min_ns: Some(333_334),
        ..settings(Some(configured))
    };
    let reads = RefCell::new(vec![own(1_000_000), own(333_334)]);
    let mut asked = Vec::new();
    let r = latency(
        &two(),
        "Test Dev",
        "0.333334",
        |_| Ok(reads.borrow_mut().remove(0)),
        |_, ns| {
            asked.push(ns);
            Ok(own(ns))
        },
    );
    assert_eq!(
        r,
        Ok("Test Dev: the latency is set to 0.333334 ms; it reads back 0.333334 ms".to_string())
    );
    assert_eq!(asked, vec![333_334]);
    // Already there: nothing written.
    let r = latency(
        &two(),
        "Test Dev",
        "0.333334",
        |_| Ok(own(333_334)),
        |_, _| -> Result<Settings, String> { panic!("no write for the value already set") },
    );
    assert_eq!(
        r,
        Ok("Test Dev: the latency is already 0.333334 ms; nothing written".to_string())
    );
}

#[test]
fn a_value_already_configured_that_is_none_of_dante_s_writes_nothing() {
    let r = latency(
        &two(),
        "Test Dev",
        "3.5",
        |_| Ok(settings(Some(3_500_000))),
        |_, _| -> Result<Settings, String> { panic!("no write for the value already set") },
    );
    assert_eq!(
        r,
        Ok("Test Dev: the latency is already 3.5 ms; nothing written".to_string())
    );
}

#[test]
fn a_value_outside_the_range_or_no_value_writes_nothing() {
    for (ms, shown) in [
        ("0.25", "0.25 ms"),
        ("0.4999", "0.4999 ms"),
        ("0.4999994", "0.499999 ms"),
        ("5.001", "5.001 ms"),
        ("10", "10 ms"),
    ] {
        let r = latency(
            &two(),
            "Test Dev",
            ms,
            |_| Ok(settings(Some(1_000_000))),
            |_, _| -> Result<Settings, String> { panic!("no write") },
        );
        assert_eq!(
            r,
            Err(format!(
                "Test Dev: {shown} is outside the latency range the device states \
                 (0.5 ms to 5 ms); nothing written"
            )),
            "{ms:?}"
        );
    }
    for ms in ["x", "", "1 ms", "-1"] {
        let r = latency(
            &two(),
            "Test Dev",
            ms,
            |_| Ok(settings(Some(1_000_000))),
            |_, _| -> Result<Settings, String> { panic!("no write") },
        );
        assert_eq!(
            r,
            Err(format!(
                "Test Dev: {ms} is not a latency in ms (such as 0.5 or 0.333); nothing written"
            )),
            "{ms:?}"
        );
    }
    let r = latency(
        &two(),
        "Test Dev",
        "1",
        |_| Ok(Settings::default()),
        |_, _| -> Result<Settings, String> { panic!("no write") },
    );
    assert_eq!(
        r,
        Err("Test Dev: the device names no latency range; nothing written".to_string())
    );
}

#[test]
fn a_refused_write_is_the_reason() {
    let r = latency(
        &two(),
        "Test Dev",
        "0.5",
        |_| Ok(settings(Some(1_000_000))),
        |_, _| Err("the configured latency reads Some(1000000) ns, not 500000 ns".to_string()),
    );
    assert_eq!(
        r,
        Err("Test Dev: the latency 0.5 ms was not set: \
             the configured latency reads Some(1000000) ns, not 500000 ns"
            .to_string())
    );
}

#[test]
fn the_read_back_decides() {
    // The device answers the write, then reads back another value.
    let reads = RefCell::new(vec![settings(Some(1_000_000)), settings(Some(1_000_000))]);
    let r = latency(
        &two(),
        "Test Dev",
        "5",
        |_| Ok(reads.borrow_mut().remove(0)),
        |_, ns| Ok(settings(Some(ns))),
    );
    assert_eq!(
        r,
        Err("Test Dev: the latency 5 ms was written, but it reads back 1 ms".to_string())
    );
    // Or it is not read back at all.
    let reads = RefCell::new(vec![
        Ok(settings(Some(1_000_000))),
        Err("timed out".to_string()),
    ]);
    let r = latency(
        &two(),
        "Test Dev",
        "5",
        |_| reads.borrow_mut().remove(0),
        |_, ns| Ok(settings(Some(ns))),
    );
    assert_eq!(
        r,
        Err("Test Dev: the latency was written, but not read back: timed out".to_string())
    );
}
