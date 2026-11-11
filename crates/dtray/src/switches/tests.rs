// SPDX-License-Identifier: MIT OR Apache-2.0

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use dstack::arc::control::RxChannel;

use super::{
    Current, FILE_NAME, Item, Source, Step, Switch, current, due, find, find_source, items, line,
    load, matches, parse, path_for, plan, set, state, verdict,
};
use crate::menu::SETTINGS_EVERY;

// Made-up device and channel names only (no site data in the repo).
const FILE: &str = r#"
[[switch]]
label = "Solo"
device = "Test Monitor"
rx = [3, 4]

[[switch.sources]]
label = "band"
device = "Test Band"
channels = ["Out L", "Out R"]

[[switch.sources]]
label = "master"
device = "Test Master"
channels = ["Mix L", "Mix R"]
"#;

fn solo() -> Switch {
    parse(FILE).unwrap().remove(0)
}

fn rx(channel: u16, sub: Option<(&str, &str)>) -> RxChannel {
    RxChannel {
        channel,
        label: format!("{channel:02}"),
        tx_channel: sub.map(|(c, _)| c.to_string()),
        tx_device: sub.map(|(_, d)| d.to_string()),
        status: 0,
    }
}

/// The receiver's channels 1 to 5, 3 and 4 on `three` and `four`.
fn receiver(three: Option<(&str, &str)>, four: Option<(&str, &str)>) -> Vec<RxChannel> {
    vec![
        rx(1, Some(("Other 1", "Elsewhere"))),
        rx(2, None),
        rx(3, three),
        rx(4, four),
        rx(5, Some(("Out L", "Test Band"))),
    ]
}

fn on_band() -> Vec<RxChannel> {
    receiver(Some(("Out L", "Test Band")), Some(("Out R", "Test Band")))
}

fn on_master() -> Vec<RxChannel> {
    receiver(
        Some(("Mix L", "Test Master")),
        Some(("Mix R", "Test Master")),
    )
}

#[test]
fn the_file_gives_its_switches_with_their_sources() {
    let all = parse(FILE).unwrap();
    assert_eq!(
        all,
        vec![Switch {
            label: "Solo".to_string(),
            device: "Test Monitor".to_string(),
            rx: vec![3, 4],
            sources: vec![
                Source {
                    label: "band".to_string(),
                    device: "Test Band".to_string(),
                    channels: vec!["Out L".to_string(), "Out R".to_string()],
                },
                Source {
                    label: "master".to_string(),
                    device: "Test Master".to_string(),
                    channels: vec!["Mix L".to_string(), "Mix R".to_string()],
                },
            ],
        }]
    );
    // The inline form is the same file.
    let inline = r#"
        [[switch]]
        label = "Solo"
        device = "Test Monitor"
        rx = [3, 4]
        sources = [
          { label = "band", device = "Test Band", channels = ["Out L", "Out R"] },
          { label = "master", device = "Test Master", channels = ["Mix L", "Mix R"] },
        ]
    "#;
    assert_eq!(parse(inline).unwrap(), all);
    // No switch at all: an empty file.
    assert_eq!(parse(""), Ok(Vec::new()));
}

/// `FILE` with `from` replaced by `to` (once), parsed.
fn edited(from: &str, to: &str) -> Result<Vec<Switch>, String> {
    assert!(FILE.contains(from), "{from}");
    parse(&FILE.replacen(from, to, 1))
}

#[test]
fn a_bad_file_is_refused_with_its_reason() {
    let cases = [
        (
            r#"label = "Solo""#,
            r#"label = " ""#,
            "a switch has no label",
        ),
        (
            r#"device = "Test Monitor""#,
            r#"device = """#,
            "switch Solo: the device name is empty",
        ),
        ("rx = [3, 4]", "rx = []", "switch Solo: no RX channel"),
        (
            "rx = [3, 4]",
            "rx = [0, 4]",
            "switch Solo: RX channels count from 1",
        ),
        (
            "rx = [3, 4]",
            "rx = [4, 4]",
            "switch Solo: RX 4 is listed twice",
        ),
        (
            r#"label = "band""#,
            r#"label = """#,
            "switch Solo: a source has no label",
        ),
        (
            r#"device = "Test Band""#,
            r#"device = "  ""#,
            "switch Solo, source band: the device name is empty",
        ),
        (
            r#"channels = ["Out L", "Out R"]"#,
            r#"channels = ["Out L"]"#,
            "switch Solo, source band: 1 channels for 2 RX channels",
        ),
        (
            r#"channels = ["Out L", "Out R"]"#,
            r#"channels = ["Out L", "Out R", "Out C"]"#,
            "switch Solo, source band: 3 channels for 2 RX channels",
        ),
        (
            r#"channels = ["Out L", "Out R"]"#,
            r#"channels = ["Out L", ""]"#,
            "switch Solo, source band: channel 2 name is empty",
        ),
        (
            r#"label = "master""#,
            r#"label = "BAND""#,
            "switch Solo: source BAND is listed twice",
        ),
    ];
    for (from, to, why) in cases {
        assert_eq!(edited(from, to), Err(why.to_string()), "{to}");
    }
    // Names: at most 31 bytes of printable ASCII.
    let long = "x".repeat(32);
    let fine = "x".repeat(31);
    let names = [
        (
            r#"device = "Test Monitor""#,
            format!(r#"device = "{long}""#),
            "switch Solo: the device name is longer than 31 bytes",
        ),
        (
            r#"device = "Test Master""#,
            format!(r#"device = "{long}""#),
            "switch Solo, source master: the device name is longer than 31 bytes",
        ),
        (
            r#""Out R""#,
            format!(r#""{long}""#),
            "switch Solo, source band: channel 2 name is longer than 31 bytes",
        ),
        (
            r#""Out L""#,
            r#""Out\tL""#.to_string(),
            "switch Solo, source band: channel 1 name is not printable ASCII",
        ),
        (
            r#"device = "Test Monitor""#,
            "device = \"Mon\u{e9}\"".to_string(),
            "switch Solo: the device name is not printable ASCII",
        ),
    ];
    for (from, to, why) in names {
        assert_eq!(edited(from, &to), Err(why.to_string()), "{to}");
    }
    for (from, to) in [
        (
            r#"device = "Test Monitor""#,
            format!(r#"device = "{fine}""#),
        ),
        (r#""Out R""#, format!(r#""{fine}""#)),
        (r#""Out L""#, r#"" ~!""#.to_string()),
    ] {
        assert!(edited(from, &to).is_ok(), "{to}");
    }
    // No source.
    let none = "[[switch]]\nlabel = \"S\"\ndevice = \"D\"\nrx = [1]\nsources = []\n";
    assert_eq!(parse(none), Err("switch S: no source".to_string()));
    // Two switches of one label, case aside.
    let two = format!("{FILE}{}", FILE.replacen("\"Solo\"", "\"SOLO\"", 1));
    assert_eq!(parse(&two), Err("switch SOLO is listed twice".to_string()));
    // Two switches of their own labels are fine.
    let two = format!("{FILE}{}", FILE.replacen("\"Solo\"", "\"Talk\"", 1));
    assert_eq!(parse(&two).unwrap().len(), 2);
    // Unknown keys, a missing key, a wrong type and bad TOML.
    for text in [
        FILE.replacen("rx = [3, 4]", "rx = [3, 4]\ncolour = 1", 1),
        FILE.replacen("channels = [\"Out L\", \"Out R\"]", "channel = 1", 1),
        FILE.replacen("rx = [3, 4]", "rx = \"3\"", 1),
        "[[switch]\n".to_string(),
        "other = 1\n".to_string(),
    ] {
        let err = parse(&text).unwrap_err();
        assert!(!err.is_empty(), "{text}");
    }
}

#[test]
fn the_file_sits_beside_the_devices_folder_unless_named() {
    let devices = Path::new("/etc/dstack/devices");
    assert_eq!(
        path_for(devices, None),
        PathBuf::from("/etc/dstack").join(FILE_NAME)
    );
    assert_eq!(
        path_for(devices, Some(OsString::new())),
        PathBuf::from("/etc/dstack").join(FILE_NAME)
    );
    assert_eq!(
        path_for(devices, Some(OsString::from("/tmp/x.toml"))),
        PathBuf::from("/tmp/x.toml")
    );
    assert_eq!(
        path_for(Path::new("devices"), None),
        PathBuf::from(FILE_NAME)
    );
    assert_eq!(FILE_NAME, "switches.toml");
}

#[test]
fn load_reads_a_file_and_no_file_is_no_switch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE_NAME);
    assert_eq!(load(&path), Ok(Vec::new()));
    std::fs::write(&path, FILE).unwrap();
    assert_eq!(load(&path), Ok(vec![solo()]));
    std::fs::write(&path, "rx = ").unwrap();
    let err = load(&path).unwrap_err();
    assert!(err.starts_with(&format!("{}: ", path.display())), "{err}");
    // A folder in the file's place cannot be read: the reason, not none.
    let err = load(dir.path()).unwrap_err();
    assert!(
        err.starts_with(&format!("{}: ", dir.path().display())),
        "{err}"
    );
}

#[test]
fn a_switch_and_a_source_are_found_by_label_in_any_case() {
    let all = vec![solo()];
    assert_eq!(find(&all, "solo"), Ok(&all[0]));
    assert_eq!(find(&all, "SOLO"), Ok(&all[0]));
    assert_eq!(
        find(&all, "Sol"),
        Err("no switch Sol (switches: Solo)".to_string())
    );
    assert_eq!(
        find(&[], "x"),
        Err("no switch x (switches: none)".to_string())
    );
    let sw = &all[0];
    assert_eq!(find_source(sw, "Master"), Ok(&sw.sources[1]));
    assert_eq!(find_source(sw, "band"), Ok(&sw.sources[0]));
    assert_eq!(
        find_source(sw, "both"),
        Err("switch Solo has no source both (sources: band, master)".to_string())
    );
}

#[test]
fn the_current_source_needs_every_rx_channel_on_it() {
    let sw = solo();
    assert_eq!(current(&sw, &Ok(on_band())), Current::Source(0));
    assert_eq!(current(&sw, &Ok(on_master())), Current::Source(1));
    // Names compare case aside.
    let upper = receiver(Some(("OUT L", "test band")), Some(("out r", "TEST BAND")));
    assert_eq!(current(&sw, &Ok(upper)), Current::Source(0));
    // Partial: one channel on band, one on master; or one unsubscribed.
    let mixed = receiver(Some(("Out L", "Test Band")), Some(("Mix R", "Test Master")));
    assert_eq!(current(&sw, &Ok(mixed)), Current::Other);
    let half = receiver(Some(("Out L", "Test Band")), None);
    assert_eq!(current(&sw, &Ok(half)), Current::Other);
    // The right device, the other channel order; the right channel, another
    // device.
    let swapped = receiver(Some(("Out R", "Test Band")), Some(("Out L", "Test Band")));
    assert_eq!(current(&sw, &Ok(swapped)), Current::Other);
    let wrong = receiver(
        Some(("Out L", "Test Master")),
        Some(("Out R", "Test Master")),
    );
    assert_eq!(current(&sw, &Ok(wrong)), Current::Other);
    // None subscribed; a receiver without the switch's channels.
    assert_eq!(current(&sw, &Ok(receiver(None, None))), Current::Other);
    let short = vec![rx(1, Some(("Out L", "Test Band")))];
    assert_eq!(current(&sw, &Ok(short)), Current::Other);
    assert_eq!(current(&sw, &Ok(Vec::new())), Current::Other);
    // Unread.
    assert_eq!(
        current(&sw, &Err("timed out".to_string())),
        Current::Unread("timed out".to_string())
    );
}

#[test]
fn a_device_or_a_channel_alone_does_not_match() {
    let sw = solo();
    let band = &sw.sources[0];
    let device_only = vec![
        RxChannel {
            tx_channel: None,
            ..rx(3, Some(("Out L", "Test Band")))
        },
        RxChannel {
            tx_channel: None,
            ..rx(4, Some(("Out R", "Test Band")))
        },
    ];
    assert!(!matches(&sw, band, &device_only));
    let channel_only = vec![
        RxChannel {
            tx_device: None,
            ..rx(3, Some(("Out L", "Test Band")))
        },
        RxChannel {
            tx_device: None,
            ..rx(4, Some(("Out R", "Test Band")))
        },
    ];
    assert!(!matches(&sw, band, &channel_only));
    assert!(matches(&sw, band, &on_band()));
    // A source built without one channel per RX channel never matches,
    // even where the channels it has are on.
    let short = Source {
        channels: vec!["Out L".to_string()],
        ..band.clone()
    };
    assert!(!matches(&sw, &short, &on_band()));
}

#[test]
fn the_state_and_line_name_the_source() {
    let sw = solo();
    assert_eq!(state(&sw, &Current::Source(0)), "band");
    assert_eq!(state(&sw, &Current::Source(1)), "master");
    assert_eq!(state(&sw, &Current::Source(2)), "other");
    assert_eq!(state(&sw, &Current::Other), "other");
    assert_eq!(state(&sw, &Current::Unread("x".to_string())), "unread");
    assert_eq!(line(&sw, &Current::Source(1)), "Solo: master");
    assert_eq!(line(&sw, &Current::Other), "Solo: other");
    assert_eq!(
        line(&sw, &Current::Unread("not found".to_string())),
        "Solo: unread (not found)"
    );
}

#[test]
fn the_plan_touches_only_the_switch_s_rx_channels() {
    let sw = solo();
    assert_eq!(
        plan(&sw, &sw.sources[1]),
        vec![
            Step {
                rx: 3,
                tx_channel: "Mix L",
                tx_device: "Test Master",
            },
            Step {
                rx: 4,
                tx_channel: "Mix R",
                tx_device: "Test Master",
            },
        ]
    );
    assert_eq!(
        plan(&sw, &sw.sources[0])
            .iter()
            .map(|s| (s.rx, s.tx_channel, s.tx_device))
            .collect::<Vec<_>>(),
        vec![(3, "Out L", "Test Band"), (4, "Out R", "Test Band")]
    );
}

#[test]
fn the_verdict_is_the_read_back() {
    let sw = solo();
    let master = &sw.sources[1];
    assert_eq!(
        verdict(&sw, master, Ok(on_master())),
        Ok("Solo: switched to master; it reads back master".to_string())
    );
    assert_eq!(
        verdict(&sw, master, Ok(on_band())),
        Err("Solo: master was asked; it reads back band".to_string())
    );
    let half = receiver(Some(("Mix L", "Test Master")), Some(("Out R", "Test Band")));
    assert_eq!(
        verdict(&sw, master, Ok(half)),
        Err("Solo: master was asked; it reads back other".to_string())
    );
    assert_eq!(
        verdict(&sw, master, Err("timed out".to_string())),
        Err("Solo: master was asked, but not read back: timed out".to_string())
    );
}

#[test]
fn a_choice_subscribes_the_listed_channels_and_reads_back() {
    let sw = solo();
    let reads = RefCell::new(vec![on_band(), on_master()]);
    let mut sent = Vec::new();
    let r = set(
        &sw,
        &sw.sources[1],
        || Ok(reads.borrow_mut().remove(0)),
        |s| {
            sent.push((s.rx, s.tx_channel.to_string(), s.tx_device.to_string()));
            Ok(())
        },
    );
    assert_eq!(
        r,
        Ok("Solo: switched to master; it reads back master".to_string())
    );
    assert_eq!(
        sent,
        vec![
            (3, "Mix L".to_string(), "Test Master".to_string()),
            (4, "Mix R".to_string(), "Test Master".to_string()),
        ]
    );
    assert!(reads.borrow().is_empty(), "read before and after");
}

#[test]
fn a_choice_already_on_writes_nothing() {
    let sw = solo();
    let r = set(
        &sw,
        &sw.sources[0],
        || Ok(on_band()),
        |_| panic!("no subscription for the source already on"),
    );
    assert_eq!(r, Ok("Solo: already band; nothing written".to_string()));
}

#[test]
fn a_choice_that_cannot_read_writes_nothing() {
    let sw = solo();
    let r = set(
        &sw,
        &sw.sources[0],
        || Err("timed out".to_string()),
        |_| panic!("no subscription without a read"),
    );
    assert_eq!(
        r,
        Err("Solo: the receiver was not read: timed out".to_string())
    );
}

#[test]
fn a_refused_subscription_stops_the_choice() {
    let sw = solo();
    let mut reads = 0;
    let mut sent = Vec::new();
    let r = set(
        &sw,
        &sw.sources[1],
        || {
            reads += 1;
            Ok(on_band())
        },
        |s| {
            sent.push(s.rx);
            Err("refused".to_string())
        },
    );
    assert_eq!(
        r,
        Err("Solo: RX 3 was not subscribed to master: refused".to_string())
    );
    assert_eq!(sent, vec![3]);
    assert_eq!(reads, 1);
}

#[test]
fn a_choice_the_device_ignored_is_an_error() {
    let sw = solo();
    let r = set(&sw, &sw.sources[1], || Ok(on_band()), |_| Ok(()));
    assert_eq!(
        r,
        Err("Solo: master was asked; it reads back band".to_string())
    );
}

#[test]
fn switches_are_read_at_once_then_every_settings_round() {
    assert!(due(None, 0));
    assert!(due(None, 77));
    assert!(!due(Some(5), 5));
    assert!(!due(Some(5), 5 + SETTINGS_EVERY - 1));
    assert!(due(Some(5), 5 + SETTINGS_EVERY));
    // Ticks wrap.
    assert!(!due(Some(u32::MAX), SETTINGS_EVERY - 2));
    assert!(due(Some(u32::MAX), SETTINGS_EVERY - 1));
}

#[test]
fn the_menu_items_show_the_current_source_checked() {
    let sw = solo();
    let talk = Switch {
        label: "Talk".to_string(),
        ..solo()
    };
    let all = vec![sw.clone(), talk.clone(), sw];
    let got = items(
        &all,
        &[Current::Source(1), Current::Unread("not found".to_string())],
    );
    assert_eq!(
        got,
        vec![
            Item {
                text: "Solo: master".to_string(),
                sources: vec![("band".to_string(), false), ("master".to_string(), true)],
                note: None,
            },
            Item {
                text: "Talk: unread".to_string(),
                sources: vec![("band".to_string(), false), ("master".to_string(), false)],
                note: Some("not found".to_string()),
            },
            Item {
                text: "Solo: unread".to_string(),
                sources: vec![("band".to_string(), false), ("master".to_string(), false)],
                note: Some("not read yet".to_string()),
            },
        ]
    );
    let other = items(&all[..1], &[Current::Other]);
    assert_eq!(other[0].text, "Solo: other");
    assert_eq!(other[0].note, None);
    assert!(other[0].sources.iter().all(|(_, on)| !on));
    let first = items(&all[..1], &[Current::Source(0)]);
    assert_eq!(
        first[0].sources,
        vec![("band".to_string(), true), ("master".to_string(), false)]
    );
}
