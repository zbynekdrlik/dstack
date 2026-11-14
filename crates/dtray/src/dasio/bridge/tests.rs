// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::BTreeSet;

use dlink::bridge::{MAX_APP_BUFFER_NS, MIN_APP_BUFFER_NS};

use super::*;
use crate::dasio::{Field, Fields, Kind, ReadBack, changes, edit, plan, status_driver, verify};

/// A DAW-kind instance config (made-up names), 4 RX and 2 TX channels.
const DAW: &str = r#"# test instance
iface = "Ethernet 2"
name = "test-bridge"
manufacturer = "dasio"
model = "dasio"
sample_rate = 96000
encoding = 24
latency_ns = 2000000
tx_latency_ns = 1000000
block = 256
work_margin = 256
state_file = "C:/ProgramData/dasio/instance-1/state.toml"
rx = ["In 1", "In 2", "In 3", "In 4"]
tx = ["Out 1", "Out 2"]
"#;

/// The bridge table as dasio's docs write it, with comments.
const TABLE: &str = r#"
[bridge]
# The registered ASIO driver's name.
driver = "Test ASIO" # the driver
to_asio = [1, 2, 3, 4]
from_asio = [5, 6]
app_buffer_ns = 5000000
"#;

fn text() -> String {
    format!("{DAW}{TABLE}")
}

fn fields() -> Fields {
    Fields::read(&text()).expect("the bridge config reads")
}

fn bridge() -> Bridge {
    Bridge {
        driver: "Test ASIO".into(),
        to_asio: vec![1, 2, 3, 4],
        from_asio: vec![5, 6],
        app_buffer_ns: Some(5_000_000),
    }
}

#[test]
fn a_bridge_instance_s_file_reads_with_its_table() {
    let f = fields();
    assert_eq!(f.bridge, Some(bridge()));
    assert_eq!(f.name, "test-bridge");
    assert_eq!(f.rx.len(), 4);
    assert_eq!(Fields::read(DAW).expect("reads").bridge, None);
    // The optional keys may be left out; an inline table reads too.
    let short = format!("{DAW}bridge = {{ driver = \"D\", to_asio = [1, 2, 3, 4] }}\n");
    assert_eq!(
        Fields::read(&short).expect("reads").bridge,
        Some(Bridge {
            driver: "D".into(),
            to_asio: vec![1, 2, 3, 4],
            from_asio: Vec::new(),
            app_buffer_ns: None,
        })
    );
}

#[test]
fn a_bridge_table_of_another_shape_is_refused_with_its_reason() {
    let not_table = format!("{DAW}bridge = 3\n");
    assert_eq!(
        Fields::read(&not_table).expect_err("not a table"),
        "bridge: not a table"
    );
    for (from, to, said) in [
        ("app_buffer_ns", "app_buffer", "unknown field `app_buffer`"),
        ("driver = \"Test ASIO\"", "driver = 7", "invalid type"),
        (
            "to_asio = [1, 2, 3, 4]",
            "to_asio = [1, -2]",
            "invalid value",
        ),
        ("to_asio = [1, 2, 3, 4]", "to_asio = 1", "invalid type"),
        (
            "driver = \"Test ASIO\" # the driver\n",
            "",
            "missing field `driver`",
        ),
    ] {
        let bad = text().replace(from, to);
        let err = Fields::read(&bad).expect_err(said);
        assert!(err.starts_with("[bridge]: "), "{err}");
        assert!(err.contains(said), "{said}: {err}");
        assert!(!err.contains('\n'), "{err}");
    }
}

#[test]
fn an_untouched_bridge_file_is_written_back_unchanged() {
    assert_eq!(edit(&text(), &fields()).expect("the edit"), text());
    // Another field changed: the table stays as it was.
    let mut new = fields();
    new.block = 128;
    let out = edit(&text(), &new).expect("the edit");
    assert_eq!(out, text().replace("block = 256", "block = 128"));
}

#[test]
fn an_edited_driver_and_map_are_written_back_the_rest_kept() {
    let mut new = fields();
    let b = new.bridge.as_mut().expect("a bridge");
    b.driver = "Other ASIO".into();
    b.from_asio = vec![2, 1];
    let out = edit(&text(), &new).expect("the edit");
    assert_eq!(
        out,
        text()
            .replace("\"Test ASIO\"", "\"Other ASIO\"")
            .replace("from_asio = [5, 6]", "from_asio = [2, 1]")
    );
    assert_eq!(Fields::read(&out).expect("reads"), new);
    // The to_asio map, and the app buffer set and taken out.
    let mut to = fields();
    let b = to.bridge.as_mut().expect("a bridge");
    b.to_asio = vec![4, 3, 2, 1];
    b.app_buffer_ns = Some(7_500_000);
    let out = edit(&text(), &to).expect("the edit");
    assert!(out.contains("to_asio = [4, 3, 2, 1]\n"), "{out}");
    assert!(out.contains("app_buffer_ns = 7500000\n"), "{out}");
    let mut none = fields();
    none.bridge.as_mut().expect("a bridge").app_buffer_ns = None;
    let out = edit(&text(), &none).expect("the edit");
    assert!(!out.contains("app_buffer_ns"), "{out}");
    assert!(
        out.contains("# The registered ASIO driver's name.\ndriver = \"Test ASIO\" # the driver\n"),
        "{out}"
    );
    assert_eq!(Fields::read(&out).expect("reads"), none);
}

#[test]
fn a_file_without_a_bridge_gets_none_added() {
    let mut new = Fields::read(DAW).expect("reads");
    new.block = 128;
    let out = edit(DAW, &new).expect("the edit");
    assert!(!out.contains("[bridge]"), "{out}");
    assert_eq!(out, DAW.replace("block = 256", "block = 128"));
}

#[test]
fn write_adds_a_table_and_takes_it_out() {
    let mut doc: DocumentMut = DAW.parse().expect("toml");
    write(&mut doc, None, Some(&bridge())).expect("written");
    let added = doc.to_string();
    assert_eq!(Fields::read(&added).expect("reads").bridge, Some(bridge()));
    assert!(added.contains("[bridge]\n"), "{added}");
    let mut doc: DocumentMut = text().parse().expect("toml");
    write(&mut doc, Some(&bridge()), None).expect("written");
    assert_eq!(Fields::read(&doc.to_string()).expect("reads").bridge, None);
    // A bridge key of another shape is not written over.
    let mut doc: DocumentMut = format!("{DAW}bridge = 3\n").parse().expect("toml");
    assert_eq!(
        write(&mut doc, None, Some(&bridge())),
        Err("bridge: not a table".to_string())
    );
    let mut big = bridge();
    big.app_buffer_ns = Some(u64::MAX);
    let mut doc: DocumentMut = text().parse().expect("toml");
    assert_eq!(
        write(&mut doc, Some(&bridge()), Some(&big)),
        Err(format!("app_buffer_ns: {} is too large", u64::MAX))
    );
}

#[test]
fn a_good_bridge_has_no_problems() {
    assert!(bridge().problems(4, 2).is_empty());
    // One side only is a bridge too.
    let mut rx_only = bridge();
    rx_only.from_asio.clear();
    assert!(rx_only.problems(4, 0).is_empty());
    let mut tx_only = bridge();
    tx_only.to_asio.clear();
    assert!(tx_only.problems(0, 2).is_empty());
}

#[test]
fn a_map_of_the_wrong_length_is_refused_with_a_message() {
    assert_eq!(
        bridge().problems(6, 2),
        ["Dante RX to driver outputs: 4 given for 6 RX channels (one for each)"]
    );
    assert_eq!(
        bridge().problems(4, 1),
        ["Driver inputs to Dante TX: 2 given for 1 TX channels (one for each)"]
    );
    // Through the plan: the inputs grown, the map not.
    let old = fields();
    let mut new = old.clone();
    new.rx = crate::dasio::resize(&new.rx, 6);
    let err = plan(&text(), &old, &new, &BTreeSet::new(), 8).expect_err("refused");
    assert_eq!(
        err,
        "Dante RX to driver outputs: 4 given for 6 RX channels (one for each)"
    );
    new.bridge.as_mut().expect("a bridge").to_asio = vec![1, 2, 3, 4, 5, 6];
    let p = plan(&text(), &old, &new, &BTreeSet::new(), 8).expect("the plan");
    let config = p.config.expect("a restart");
    assert!(
        config.contains("to_asio = [1, 2, 3, 4, 5, 6]\n"),
        "{config}"
    );
}

#[test]
fn every_value_dasio_refuses_is_named() {
    let b = Bridge {
        driver: " ".into(),
        to_asio: vec![0, 2, 2, 0, 2],
        from_asio: vec![3, 3],
        app_buffer_ns: Some(MIN_APP_BUFFER_NS - 1),
    };
    let p = b.problems(5, 2);
    assert_eq!(
        p,
        [
            "ASIO driver: none chosen".to_string(),
            "Dante RX to driver outputs: channels count from 1".into(),
            "Dante RX to driver outputs: channel 0 twice".into(),
            "Dante RX to driver outputs: channel 2 twice".into(),
            "Driver inputs to Dante TX: channel 3 twice".into(),
            format!(
                "App buffer: {}",
                AppBuffer::new(0, 0)
                    .set(Some(MIN_APP_BUFFER_NS - 1))
                    .problem()
                    .expect("out of range")
            ),
        ]
    );
    let none = Bridge {
        driver: "D".into(),
        to_asio: Vec::new(),
        from_asio: Vec::new(),
        app_buffer_ns: None,
    };
    assert_eq!(none.problems(0, 0), ["the bridge carries no channel"]);
}

#[test]
fn the_app_buffer_takes_dlink_s_range() {
    let at = |ns: u64| {
        let mut b = bridge();
        b.app_buffer_ns = Some(ns);
        b.problems(4, 2).len()
    };
    assert_eq!(at(MIN_APP_BUFFER_NS), 0);
    assert_eq!(at(MAX_APP_BUFFER_NS), 0);
    assert_eq!(at(MIN_APP_BUFFER_NS - 1), 1);
    assert_eq!(at(MAX_APP_BUFFER_NS + 1), 1);
}

#[test]
fn the_bridge_fields_are_changes_that_need_a_restart() {
    let old = fields();
    let mut new = old.clone();
    assert!(changes(&old, &new).is_empty());
    let b = new.bridge.as_mut().expect("a bridge");
    b.driver = "X".into();
    b.to_asio = vec![4, 3, 2, 1];
    b.from_asio = vec![1, 2];
    b.app_buffer_ns = None;
    let changed = changes(&old, &new);
    assert_eq!(
        changed,
        [
            Field::Driver,
            Field::ToAsio,
            Field::FromAsio,
            Field::AppBuffer
        ]
    );
    for f in changed {
        assert_eq!(f.kind(), Kind::Restart, "{f:?}");
        assert!(!f.reshapes(), "{f:?}");
    }
    // One at a time: each part is compared on its own.
    for (i, field) in [
        Field::Driver,
        Field::ToAsio,
        Field::FromAsio,
        Field::AppBuffer,
    ]
    .into_iter()
    .enumerate()
    {
        let mut one = old.clone();
        let b = one.bridge.as_mut().expect("a bridge");
        match i {
            0 => b.driver = "X".into(),
            1 => b.to_asio = vec![4, 3, 2, 1],
            2 => b.from_asio = vec![1, 2],
            _ => b.app_buffer_ns = None,
        }
        assert_eq!(changes(&old, &one), [field]);
    }
    // A table added or taken out changes every part.
    let mut daw = old.clone();
    daw.bridge = None;
    assert_eq!(changes(&old, &daw).len(), 4);
    assert_eq!(Field::Driver.label(), "ASIO driver");
    assert_eq!(Field::ToAsio.label(), "Dante RX to driver outputs");
    assert_eq!(Field::FromAsio.label(), "Driver inputs to Dante TX");
    assert_eq!(Field::AppBuffer.label(), "App buffer");
}

fn back(f: &Fields) -> ReadBack {
    ReadBack {
        block: None,
        rx: Some(f.rx.len()),
        tx: Some(f.tx.len()),
        latency_ns: Some(f.latency_ns),
        iface: f.iface.clone(),
        driver: f.bridge.as_ref().map(|b| b.driver.clone()),
    }
}

#[test]
fn a_bridge_reads_back_by_its_driver_not_its_block() {
    let f = fields();
    assert_eq!(
        verify(&f, &back(&f)),
        Ok(
            "Applied: test-bridge runs as a bridge to \"Test ASIO\", 4 in / 2 out, \
             RX latency 2 ms. Buffer, channel maps, app buffer, CPUs and TX latency \
             are in the file (the device does not report them)."
                .to_string()
        )
    );
    let mut other = back(&f);
    other.driver = Some("Other".into());
    other.block = Some(1);
    assert_eq!(
        verify(&f, &other),
        Err("ASIO driver: Test ASIO applied, Other read back".to_string())
    );
    let mut none = back(&f);
    none.driver = None;
    assert_eq!(
        verify(&f, &none),
        Err("ASIO driver: not read back".to_string())
    );
    // The DAW kind is still checked by its block, a driver aside.
    let mut daw = f.clone();
    daw.bridge = None;
    let mut b = back(&daw);
    b.driver = Some("Test ASIO".into());
    assert_eq!(verify(&daw, &b), Err("Buffer: not read back".to_string()));
}

#[test]
fn the_driver_comes_from_the_status_app_part() {
    assert_eq!(
        status_driver(r#"{"app":{"kind":"bridge","driver":"Test ASIO"}}"#).as_deref(),
        Some("Test ASIO")
    );
    assert_eq!(status_driver(r#"{"app":{"driver":3}}"#), None);
    assert_eq!(status_driver(r#"{"app":{}}"#), None);
    assert_eq!(status_driver("not json"), None);
}

#[test]
fn a_map_is_shown_and_typed_as_a_comma_list() {
    assert_eq!(map_text(&[1, 2, 10]), "1, 2, 10");
    assert_eq!(map_text(&[]), "");
    assert_eq!(parse_map("M", " 1,2 , 10 "), Ok(vec![1, 2, 10]));
    assert_eq!(parse_map("M", "  "), Ok(Vec::new()));
    assert_eq!(parse_map("M", "7"), Ok(vec![7]));
    for bad in ["1,,2", "1, x", "-1", "1;2", "1,"] {
        let err = parse_map("M", bad).expect_err(bad);
        assert!(err.starts_with("M: \""), "{bad}: {err}");
        assert!(err.ends_with("is not a channel number (write them as 1, 2, 3)"));
    }
    assert_eq!(
        parse_map("M", "99999999999"),
        Err("M: 99999999999 is too large".to_string())
    );
}

#[test]
fn the_app_buffer_is_shown_and_typed_in_ms() {
    assert_eq!(buffer_text(None), "");
    assert_eq!(buffer_text(Some(5_000_000)), "5");
    assert_eq!(buffer_text(Some(1_333_333)), "1.333333");
    assert_eq!(buffer_text(Some(2_500_000)), "2.5");
    assert_eq!(buffer_text(Some(1)), "0.000001");
    assert_eq!(parse_buffer(""), Ok(None));
    assert_eq!(parse_buffer(" 5 "), Ok(Some(5_000_000)));
    assert_eq!(parse_buffer("2.5"), Ok(Some(2_500_000)));
    assert_eq!(parse_buffer("1.333333"), Ok(Some(1_333_333)));
    assert_eq!(parse_buffer("0.000001"), Ok(Some(1)));
    assert_eq!(parse_buffer("3."), Ok(Some(3_000_000)));
    for ns in [5_000_000, 1_333_333, 1, 150_000_000] {
        assert_eq!(parse_buffer(&buffer_text(Some(ns))), Ok(Some(ns)), "{ns}");
    }
    for bad in [".5", "1.2345678", "x", "1.x", "-1", "1,5", "18446744073710"] {
        let err = parse_buffer(bad).expect_err(bad);
        assert_eq!(
            err,
            format!("App buffer: \"{bad}\" is not a time in ms (like 5 or 2.5)")
        );
    }
}

#[test]
fn the_window_s_texts_give_the_bridge_or_every_mistake() {
    assert_eq!(
        from_texts("Test ASIO", "1, 2, 3, 4", "5,6", "5"),
        Ok(bridge())
    );
    let mut default = bridge();
    default.app_buffer_ns = None;
    default.from_asio.clear();
    assert_eq!(from_texts("Test ASIO", "1,2,3,4", " ", ""), Ok(default));
    assert_eq!(
        from_texts("D", "x", "1", "5"),
        Err(
            "Dante RX to driver outputs: \"x\" is not a channel number (write them as 1, 2, 3)"
                .into()
        )
    );
    assert_eq!(
        from_texts("D", "1", "y", "5"),
        Err(
            "Driver inputs to Dante TX: \"y\" is not a channel number (write them as 1, 2, 3)"
                .into()
        )
    );
    assert_eq!(
        from_texts("D", "1", "1", "z"),
        Err("App buffer: \"z\" is not a time in ms (like 5 or 2.5)".into())
    );
    let all = from_texts("D", "x", "y", "z").expect_err("three");
    assert_eq!(all.matches("; ").count(), 2, "{all}");
    assert!(all.starts_with("Dante RX"), "{all}");
    assert!(all.ends_with("(like 5 or 2.5)"), "{all}");
}
