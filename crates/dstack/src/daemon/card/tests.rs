use super::*;

use crate::arc::client::ReadError;
use crate::arc::control::tests::Own;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn the_verbs_parse_as_dante_card_s() {
    assert_eq!(
        parse(&args(&["read", "10.0.0.12", "iem-card"])),
        Ok(Card {
            ip: "10.0.0.12".parse().unwrap(),
            expected: "iem-card".into(),
            verb: Verb::Read
        })
    );
    // An absolute path on this platform (C:\\... on Windows).
    let flag = std::env::temp_dir().join("EVENT-NOW");
    let flag_text = flag.to_str().unwrap();
    assert_eq!(
        parse(&args(&["write", "169.254.3.4", "iem-card", flag_text]))
            .unwrap()
            .verb,
        Verb::Write(flag.clone())
    );
    assert_eq!(
        parse(&args(&[
            "rename",
            "10.0.0.12",
            "iem-card",
            "iem-card-spare"
        ]))
        .unwrap()
        .verb,
        Verb::Rename("iem-card-spare".into())
    );
    for (bad, want) in [
        (&[][..], "usage"),
        (&["read", "10.0.0.12"][..], "usage"),
        (&["read", "10.0.0.12", "a", "b"][..], "usage"),
        (&["remove", "10.0.0.12", "a"][..], "usage"),
        (&["read", "224.0.0.1", "a"][..], "not a unicast"),
        (&["read", "127.0.0.1", "a"][..], "not a unicast"),
        (&["read", "255.255.255.255", "a"][..], "not a unicast"),
        (&["read", "fe80::1", "a"][..], "not a unicast"),
        (&["read", "10.0.0.12", ""][..], "empty or not printable"),
        (&["write", "10.0.0.12", "a", "flag"][..], "absolute"),
        (&["rename", "10.0.0.12", "a", "a"][..], "other than"),
        (
            &["rename", "10.0.0.12", "a", "-bad"][..],
            "not a Dante device name",
        ),
    ] {
        let got = parse(&args(bad)).expect_err("refused");
        assert!(got.contains(want), "{bad:?}: {got}");
    }
}

#[test]
fn changes_parse_as_dante_card_s() {
    let got = parse_changes(
        r#"[{"type":"rx","channel":2,"label":"Vox L"},{"type":"tx","channel":3,"label":"3"}]"#,
    )
    .unwrap();
    assert_eq!(
        got,
        vec![
            Change {
                kind: "rx",
                channel: 2,
                label: "Vox L".into()
            },
            Change {
                kind: "tx",
                channel: 3,
                label: "3".into()
            },
        ]
    );
    for (bad, want) in [
        ("nope", "not JSON"),
        ("[]", "non-empty list"),
        ("{}", "non-empty list"),
        (r#"[{"type":"rx","channel":2}]"#, "exactly"),
        (
            r#"[{"type":"rx","channel":2,"label":"a","x":1}]"#,
            "exactly",
        ),
        (
            r#"[{"type":"mix","channel":2,"label":"a"}]"#,
            "no tx/rx channel",
        ),
        (
            r#"[{"type":"rx","channel":0,"label":"a"}]"#,
            "no tx/rx channel",
        ),
        (
            r#"[{"type":"rx","channel":256,"label":"a"}]"#,
            "no tx/rx channel",
        ),
        (
            r#"[{"type":"rx","channel":2.5,"label":"a"}]"#,
            "no tx/rx channel",
        ),
        (
            r#"[{"type":"rx","channel":2,"label":"a.b"}]"#,
            "not a label",
        ),
        (r#"[{"type":"rx","channel":2,"label":""}]"#, "not a label"),
        (
            r#"[{"type":"rx","channel":2,"label":"a"},{"type":"rx","channel":2,"label":"b"}]"#,
            "twice",
        ),
    ] {
        let got = parse_changes(bad).expect_err("refused");
        assert!(got.contains(want), "{bad}: {got}");
    }
}

#[test]
fn read_gives_the_name_counts_and_labels() {
    let mut own = Own::new();
    let name = own.cfg.name.clone();
    let out = read(&mut own, &name).unwrap();
    assert_eq!(out.name, name);
    assert_eq!(out.tx_count, own.cfg.tx.len());
    assert_eq!(out.rx_count, own.cfg.rx.len());
    assert_eq!(out.tx[&1], own.cfg.tx[0]);
    assert_eq!(out.rx[&2], own.cfg.rx[1]);
    // Keys in dante_card.py's order, labels by number.
    let line = serde_json::to_string(&out).unwrap();
    assert!(line.starts_with("{\"name\":"), "{line}");
    assert!(
        line.find("\"rx_count\"") < line.find("\"tx_count\""),
        "{line}"
    );
    assert!(line.contains("\"tx\":{\"1\":"), "{line}");
}

#[test]
fn another_device_is_refused_and_nothing_is_written() {
    let mut own = Own::new();
    let before = own.state.clone();
    assert!(
        read(&mut own, "someone-else")
            .unwrap_err()
            .0
            .contains("not the expected one")
    );
    let changes = parse_changes(r#"[{"type":"rx","channel":1,"label":"X"}]"#).unwrap();
    let flag = std::env::temp_dir().join("dstack-card-never");
    let err = write(&mut own, "someone-else", &flag, &changes)
        .unwrap_err()
        .0;
    assert!(err.contains("nothing was written"), "{err}");
    assert_eq!(own.state, before);
}

#[test]
fn a_channel_the_device_lacks_writes_nothing() {
    let mut own = Own::new();
    let name = own.cfg.name.clone();
    let before = own.state.clone();
    let past = own.cfg.rx.len() + 1;
    let changes = parse_changes(&format!(
        r#"[{{"type":"rx","channel":1,"label":"A"}},{{"type":"rx","channel":{past},"label":"B"}}]"#
    ))
    .unwrap();
    let flag = std::env::temp_dir().join("dstack-card-never");
    let err = write(&mut own, &name, &flag, &changes).unwrap_err().0;
    assert!(err.contains(&format!("no rx channel {past}")), "{err}");
    assert_eq!(own.state, before);
}

#[test]
fn write_sends_in_order_and_reads_back() {
    let mut own = Own::new();
    let name = own.cfg.name.clone();
    let changes = parse_changes(
        r#"[{"type":"rx","channel":2,"label":"Vox L"},{"type":"tx","channel":1,"label":"Click"}]"#,
    )
    .unwrap();
    let flag = std::env::temp_dir().join("dstack-card-never");
    let out = write(&mut own, &name, &flag, &changes).unwrap();
    assert_eq!(out.sent.len(), 2);
    assert!(out.failed.is_none());
    assert!(out.error.is_none(), "{out:?}");
    assert!(out.read_back);
    assert!(out.mismatches.is_empty());
    assert_eq!(out.rx.as_ref().unwrap()[&2], "Vox L");
    assert_eq!(out.tx.as_ref().unwrap()[&1], "Click");
    let line = serde_json::to_string(&out).unwrap();
    assert!(
        line.starts_with("{\"sent\":[{\"type\":\"rx\",\"channel\":2,\"label\":\"Vox L\"}"),
        "{line}"
    );
    assert_eq!(own.state.rx[1], "Vox L");
}

#[test]
fn the_flag_stops_the_writes() {
    let mut own = Own::new();
    let name = own.cfg.name.clone();
    let dir = tempfile::tempdir().unwrap();
    let flag = dir.path().join("EVENT-NOW");
    std::fs::write(&flag, "").unwrap();
    let changes = parse_changes(r#"[{"type":"rx","channel":2,"label":"Vox L"}]"#).unwrap();
    let out = write(&mut own, &name, &flag, &changes).unwrap();
    assert!(out.sent.is_empty());
    assert!(
        out.error.as_deref().unwrap().contains("appeared"),
        "{out:?}"
    );
    assert_eq!(own.state.rx[1], own.cfg.rx[1]);
}

#[test]
fn a_refused_write_stops_the_rest_and_is_reported() {
    let mut own = Own::new();
    let name = own.cfg.name.clone();
    // Our device refuses an RX label another RX channel already holds
    // (names are unique per direction): the second change is refused.
    let dup = own.cfg.rx[0].clone();
    let changes = vec![
        Change {
            kind: "rx",
            channel: 3,
            label: "Fine".into(),
        },
        Change {
            kind: "rx",
            channel: 2,
            label: dup,
        },
        Change {
            kind: "rx",
            channel: 4,
            label: "Never".into(),
        },
    ];
    let flag = std::env::temp_dir().join("dstack-card-never");
    let out = write(&mut own, &name, &flag, &changes).unwrap();
    assert_eq!(out.sent.len(), 1, "{out:?}");
    assert_eq!(out.failed.as_ref().map(|c| c.channel), Some(2), "{out:?}");
    assert!(
        out.error
            .as_deref()
            .unwrap()
            .contains("writing rx 2 failed"),
        "{out:?}"
    );
    assert!(out.read_back);
    assert_eq!(own.state.rx[3], own.cfg.rx[3], "the rest is not written");
}

#[test]
fn rename_renames_our_device_and_reads_it_back() {
    let mut own = Own::new();
    let name = own.cfg.name.clone();
    let out = rename(&mut own, &name, "new-name").unwrap();
    assert!(out.renamed, "{out:?}");
    assert_eq!(out.name.as_deref(), Some("new-name"));
    assert!(out.error.is_none(), "{out:?}");
    assert_eq!(own.state.name, "new-name");
}

/// Our device, but silent to a device rename (as inferno and netaudio's
/// own devices may be).
struct NoRename(Own);

impl Call for NoRename {
    fn call(&mut self, op1: u16, body: &[u8]) -> Result<(u16, Vec<u8>), ReadError> {
        if op1 == crate::arc::control::SET_NAME {
            return Err(ReadError::Timeout { op: op1 });
        }
        self.0.call(op1, body)
    }
}

#[test]
fn rename_against_a_device_that_does_not_answer_it_is_reported() {
    let mut dev = NoRename(Own::new());
    let name = dev.0.cfg.name.clone();
    let out = rename(&mut dev, &name, "new-name").unwrap();
    assert!(!out.renamed);
    assert_eq!(out.name.as_deref(), Some(name.as_str()));
    assert!(
        out.error.as_deref().unwrap().contains("renaming failed"),
        "{out:?}"
    );
}
