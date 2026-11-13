// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::BTreeSet;
use std::ffi::OsString;

use super::*;

/// An instance config as dasio's look: comments, the `iface` key, a
/// trailing comment on a value, CPUs, made-up names.
const CONFIG_TEXT: &str = r#"# band instance (test)
iface = "Ethernet 2"
# The name Dante Controller lists.
name = "test-asio"
manufacturer = "dasio"
model = "dasio"
sample_rate = 96000
encoding = 24
latency_ns = 2000000 # the RX latency
tx_latency_ns = 1000000
block = 256 # the buffer
work_margin = 256
pump_cpu = 4
io_cpu = 6
state_file = "C:/ProgramData/dasio/instance-1/state.toml"
rx = [
  "In 1",
  "In 2",
  "In 3",
  "In 4",
]
tx = [
  "Out 1",
  "Out 2",
]
"#;

fn fields() -> Fields {
    Fields::read(CONFIG_TEXT).expect("the test config reads")
}

#[test]
fn the_root_is_dasio_under_program_data_or_its_usual_place() {
    assert_eq!(
        root_for(Some(OsString::from("D:/Data"))),
        PathBuf::from("D:/Data").join("dasio")
    );
    assert_eq!(
        root_for(Some(OsString::new())),
        PathBuf::from(r"C:\ProgramData").join("dasio")
    );
    assert_eq!(
        root_for(None),
        PathBuf::from(r"C:\ProgramData").join("dasio")
    );
}

#[test]
fn an_instance_number_is_plain_digits_after_the_prefix() {
    assert_eq!(instance_number("instance-1"), Some(1));
    assert_eq!(instance_number("instance-12"), Some(12));
    for bad in [
        "instance-",
        "instance-+1",
        "instance- 1",
        "instance-1a",
        "Instance-1",
        "instance1",
        "instance-99999999999",
        "other",
    ] {
        assert_eq!(instance_number(bad), None, "{bad}");
    }
}

#[test]
fn instances_are_the_numbered_folders_with_a_config_by_number() {
    let root = tempfile::tempdir().expect("a temp dir");
    for (dir, config) in [
        ("instance-2", true),
        ("instance-10", true),
        ("instance-1", true),
        ("instance-3", false),
        ("other", true),
    ] {
        let d = root.path().join(dir);
        std::fs::create_dir(&d).expect("a folder");
        if config {
            std::fs::write(d.join(CONFIG), CONFIG_TEXT).expect("a config");
        }
    }
    // A file named like an instance is no folder with a config.
    std::fs::write(root.path().join("instance-4"), "x").expect("a file");
    let found = instances(root.path());
    let numbers: Vec<u32> = found.iter().map(|i| i.number).collect();
    assert_eq!(numbers, [1, 2, 10]);
    assert_eq!(found[0].dir, root.path().join("instance-1"));
    assert_eq!(
        found[0].config(),
        root.path().join("instance-1").join(CONFIG)
    );
    assert_eq!(found[2].service(), "dasio-10");
    assert_eq!(service_name(3), "dasio-3");
    assert!(instances(&root.path().join("missing")).is_empty());
}

#[test]
fn the_fields_read_as_dasio_reads_the_file() {
    let f = fields();
    assert_eq!(f.name, "test-asio");
    assert_eq!(f.iface.as_deref(), Some("Ethernet 2"));
    assert_eq!(f.block, 256);
    assert_eq!(f.latency_ns, 2_000_000);
    assert_eq!(f.tx_latency_ns, Some(1_000_000));
    assert_eq!(f.sample_rate, 96_000);
    assert_eq!(f.rx, ["In 1", "In 2", "In 3", "In 4"]);
    assert_eq!(f.tx, ["Out 1", "Out 2"]);
    assert_eq!(
        f.cpus,
        Cpus {
            pump: Some(4),
            io: Some(6),
            rx: None
        }
    );
    let no_iface = CONFIG_TEXT.replace("iface = \"Ethernet 2\"\n", "");
    assert_eq!(Fields::read(&no_iface).expect("reads").iface, None);
}

#[test]
fn a_bad_file_is_refused_with_its_reason() {
    let not_toml = Fields::read("name = ").expect_err("not TOML");
    assert!(not_toml.starts_with("not TOML: "), "{not_toml}");
    assert!(!not_toml.contains('\n'), "{not_toml}");
    let iface = CONFIG_TEXT.replace("iface = \"Ethernet 2\"", "iface = 2");
    assert_eq!(
        Fields::read(&iface).expect_err("iface"),
        "iface: not a string"
    );
    let block = CONFIG_TEXT.replace("block = 256", "block = 100");
    assert!(Fields::read(&block).expect_err("block").contains("block"));
}

#[test]
fn an_edit_keeps_comments_order_and_other_keys() {
    let mut new = fields();
    new.block = 128;
    let out = edit(CONFIG_TEXT, &new).expect("the edit");
    assert_eq!(out, CONFIG_TEXT.replace("block = 256", "block = 128"));
}

#[test]
fn an_edit_writes_every_field_it_changes() {
    let mut new = fields();
    new.iface = Some("Ethernet".into());
    new.latency_ns = 5_000_000;
    new.tx_latency_ns = Some(1_500_000);
    new.rx = resize(&new.rx, 2);
    new.tx = resize(&new.tx, 3);
    new.cpus = Cpus {
        pump: Some(5),
        io: None,
        rx: Some(7),
    };
    let out = edit(CONFIG_TEXT, &new).expect("the edit");
    assert_eq!(Fields::read(&out).expect("reads"), new);
    assert!(out.contains("iface = \"Ethernet\"\n"), "{out}");
    assert!(
        out.contains("latency_ns = 5000000 # the RX latency\n"),
        "{out}"
    );
    assert!(out.contains("tx_latency_ns = 1500000\n"), "{out}");
    assert!(out.contains("pump_cpu = 5\n"), "{out}");
    assert!(!out.contains("io_cpu"), "{out}");
    assert!(out.contains("rx_cpu = 7\n"), "{out}");
    assert!(
        out.contains("rx = [\n  \"In 1\",\n  \"In 2\",\n]\n"),
        "{out}"
    );
    assert!(
        out.contains("tx = [\n  \"Out 1\",\n  \"Out 2\",\n  \"3\",\n]\n"),
        "{out}"
    );
    assert!(out.starts_with("# band instance (test)\n"), "{out}");
    assert!(
        out.contains("# The name Dante Controller lists.\n"),
        "{out}"
    );
    assert!(out.contains("block = 256 # the buffer\n"), "{out}");
}

#[test]
fn an_edit_can_take_keys_out() {
    let mut new = fields();
    new.iface = None;
    new.tx_latency_ns = None;
    new.cpus.pump = None;
    let out = edit(CONFIG_TEXT, &new).expect("the edit");
    assert!(!out.contains("iface"), "{out}");
    assert!(!out.contains("tx_latency_ns"), "{out}");
    assert!(!out.contains("pump_cpu"), "{out}");
    assert!(out.contains("io_cpu = 6\n"), "{out}");
    assert_eq!(Fields::read(&out).expect("reads"), new);
}

#[test]
fn an_edit_with_no_change_is_the_same_text() {
    assert_eq!(edit(CONFIG_TEXT, &fields()).expect("the edit"), CONFIG_TEXT);
}

#[test]
fn an_edit_refuses_the_name_the_rate_and_a_bad_config() {
    let mut name = fields();
    name.name = "other".into();
    assert_eq!(
        edit(CONFIG_TEXT, &name).expect_err("name"),
        "the device's name is not changed here"
    );
    let mut rate = fields();
    rate.sample_rate = 48_000;
    assert_eq!(
        edit(CONFIG_TEXT, &rate).expect_err("rate"),
        "the sample rate is the network's, not changed here"
    );
    let mut block = fields();
    block.block = 100;
    let err = edit(CONFIG_TEXT, &block).expect_err("block");
    assert!(err.starts_with("the new settings are refused: "), "{err}");
    let mut big = fields();
    big.block = u64::MAX;
    assert_eq!(
        edit(CONFIG_TEXT, &big).expect_err("too large"),
        format!("block: {} is too large", u64::MAX)
    );
    assert!(edit("name = ", &fields()).is_err());
}

#[test]
fn an_edit_refuses_a_change_that_reads_back_otherwise() {
    // A duplicate channel name is refused by the config itself.
    let mut dup = fields();
    dup.tx = vec!["Out 1".into(), "out 1".into()];
    assert!(
        edit(CONFIG_TEXT, &dup)
            .expect_err("dup")
            .starts_with("the new settings are refused: ")
    );
}

#[test]
fn a_resize_keeps_the_names_and_numbers_new_ones() {
    let names: Vec<String> = ["In 1", "In 2"].map(String::from).to_vec();
    assert_eq!(resize(&names, 4), ["In 1", "In 2", "3", "4"]);
    assert_eq!(resize(&names, 1), ["In 1"]);
    assert_eq!(resize(&names, 2), names);
    let taken: Vec<String> = ["3", "x"].map(String::from).to_vec();
    assert_eq!(resize(&taken, 3), ["3", "x", "3-2"]);
    let more: Vec<String> = ["3", "3-2"].map(String::from).to_vec();
    assert_eq!(resize(&more, 3), ["3", "3-2", "3-3"]);
}

#[test]
fn a_resized_list_is_a_valid_config() {
    let mut new = fields();
    new.rx = resize(&new.rx, MAX_CHANNELS);
    new.tx = resize(&new.tx, MIN_CHANNELS);
    let out = edit(CONFIG_TEXT, &new).expect("the edit");
    assert_eq!(Fields::read(&out).expect("reads").rx.len(), 64);
}

#[test]
fn only_the_rx_latency_applies_at_once() {
    let all = [
        Field::Iface,
        Field::Block,
        Field::RxLatency,
        Field::TxLatency,
        Field::Inputs,
        Field::Outputs,
        Field::PumpCpu,
        Field::IoCpu,
        Field::RxCpu,
    ];
    for f in all {
        let want = if f == Field::RxLatency {
            Kind::Live
        } else {
            Kind::Restart
        };
        assert_eq!(f.kind(), want, "{f:?}");
        assert!(!f.label().is_empty());
        assert_eq!(
            f.reshapes(),
            matches!(f, Field::Block | Field::Inputs | Field::Outputs),
            "{f:?}"
        );
    }
    assert_eq!(Field::Iface.label(), "Ethernet interface");
    assert_eq!(Field::PumpCpu.label(), "Audio pump CPU");
    assert_eq!(Field::IoCpu.label(), "Send (TX) CPU");
    assert_eq!(Field::RxCpu.label(), "Receive (RX) CPU");
}

#[test]
fn changes_list_each_changed_field_in_order() {
    let old = fields();
    assert!(changes(&old, &old).is_empty());
    let mut new = old.clone();
    new.cpus.rx = Some(1);
    new.iface = None;
    new.latency_ns = 1_000_000;
    assert_eq!(
        changes(&old, &new),
        [Field::Iface, Field::RxLatency, Field::RxCpu]
    );
    let mut each = old.clone();
    each.block = 64;
    each.tx_latency_ns = None;
    each.rx = resize(&old.rx, 2);
    each.tx = resize(&old.tx, 4);
    each.cpus.pump = None;
    each.cpus.io = Some(1);
    assert_eq!(
        changes(&old, &each),
        [
            Field::Block,
            Field::TxLatency,
            Field::Inputs,
            Field::Outputs,
            Field::PumpCpu,
            Field::IoCpu
        ]
    );
    assert_eq!(restarts(&changes(&old, &each)), 6);
    assert_eq!(restarts(&changes(&old, &new)), 2);
    assert_eq!(restarts(&[Field::RxLatency]), 0);
}

#[test]
fn the_banner_counts_the_restart_changes() {
    assert_eq!(banner("band", 0, true), None);
    let one = banner("band", 1, false).expect("a banner");
    assert_eq!(
        one,
        "↻ 1 change needs a restart of band (~5 s of silence). \
         The DAW may keep the driver (the buffer and the channel counts stay)."
    );
    let two = banner("band", 2, true).expect("a banner");
    assert!(
        two.starts_with("↻ 2 changes need a restart of band "),
        "{two}"
    );
    assert!(two.contains("must release the driver first"), "{two}");
}

#[test]
fn the_other_instances_cpus_are_busy() {
    let mut a = fields();
    a.cpus = Cpus {
        pump: Some(1),
        io: Some(2),
        rx: Some(2),
    };
    let mut b = fields();
    b.cpus = Cpus {
        pump: None,
        io: Some(9),
        rx: None,
    };
    assert_eq!(busy_cpus(&[a, b]), BTreeSet::from([1, 2, 9]));
    assert!(busy_cpus(&[]).is_empty());
    assert_eq!(cpu_label(None), "Any");
    assert_eq!(cpu_label(Some(3)), "CPU 3");
}

#[test]
fn a_cpu_is_refused_when_busy_or_past_the_pc() {
    let busy = BTreeSet::from([2, 5]);
    let ok = Cpus {
        pump: Some(4),
        io: Some(6),
        rx: Some(6),
    };
    assert!(cpu_problems(&ok, &busy, 8).is_empty());
    assert!(cpu_problems(&Cpus::default(), &busy, 8).is_empty());
    let bad = Cpus {
        pump: Some(5),
        io: Some(8),
        rx: Some(7),
    };
    assert_eq!(
        cpu_problems(&bad, &busy, 8),
        [
            "Audio pump CPU: CPU 5 is another instance's",
            "Send (TX) CPU: CPU 8, this PC has CPUs 0 to 7",
        ]
    );
    let rx = Cpus {
        pump: None,
        io: None,
        rx: Some(2),
    };
    assert_eq!(
        cpu_problems(&rx, &busy, 8),
        ["Receive (RX) CPU: CPU 2 is another instance's"]
    );
}

#[test]
fn a_plan_of_only_the_latency_writes_it_live_and_no_file() {
    let mut new = fields();
    new.latency_ns = 5_000_000;
    let p = plan(CONFIG_TEXT, &fields(), &new, &BTreeSet::new(), 8).expect("a plan");
    assert_eq!(
        p,
        Plan {
            latency: Some(5_000_000),
            config: None,
            release: false
        }
    );
}

#[test]
fn a_plan_with_restart_changes_writes_the_file_without_the_live_latency() {
    let mut new = fields();
    new.latency_ns = 5_000_000;
    new.cpus.rx = Some(7);
    let p = plan(CONFIG_TEXT, &fields(), &new, &BTreeSet::new(), 8).expect("a plan");
    assert_eq!(p.latency, Some(5_000_000));
    assert!(!p.release);
    let file = Fields::read(p.config.as_deref().expect("a file")).expect("reads");
    assert_eq!(file.latency_ns, 2_000_000);
    assert_eq!(file.cpus.rx, Some(7));
}

#[test]
fn a_plan_compares_with_the_device_s_own_latency() {
    // The device runs at 5 ms (a controller set it); the file says 2 ms.
    let mut old = fields();
    old.latency_ns = 5_000_000;
    let mut new = old.clone();
    new.latency_ns = 2_000_000;
    let p = plan(CONFIG_TEXT, &old, &new, &BTreeSet::new(), 8).expect("a plan");
    assert_eq!(p.latency, Some(2_000_000));
    assert_eq!(p.config, None);
    // A restart change beside it keeps the file's own latency.
    new.block = 128;
    old.latency_ns = 1_000_000;
    let p = plan(CONFIG_TEXT, &old, &new, &BTreeSet::new(), 8).expect("a plan");
    let file = Fields::read(p.config.as_deref().expect("a file")).expect("reads");
    assert_eq!(file.latency_ns, 2_000_000);
    assert_eq!(file.block, 128);
}

#[test]
fn a_plan_that_reshapes_waits_for_the_release() {
    for change in [
        |f: &mut Fields| f.block = 128,
        |f: &mut Fields| f.rx = resize(&f.rx, 8),
        |f: &mut Fields| f.tx = resize(&f.tx, 8),
    ] {
        let mut new = fields();
        change(&mut new);
        let p = plan(CONFIG_TEXT, &fields(), &new, &BTreeSet::new(), 8).expect("a plan");
        assert!(p.release);
        assert_eq!(p.latency, None);
        assert!(p.config.is_some());
    }
}

#[test]
fn a_plan_refuses_no_change_a_busy_cpu_and_a_bad_file() {
    assert_eq!(
        plan(CONFIG_TEXT, &fields(), &fields(), &BTreeSet::new(), 8),
        Err("nothing changed".to_string())
    );
    let mut new = fields();
    new.cpus.rx = Some(3);
    assert_eq!(
        plan(CONFIG_TEXT, &fields(), &new, &BTreeSet::from([3]), 8),
        Err("Receive (RX) CPU: CPU 3 is another instance's".to_string())
    );
    // A pin the config already has is not this apply's to refuse: a
    // buffer change on a PC of 4 CPUs (the config pins CPU 6) goes on.
    let mut small = fields();
    small.block = 128;
    assert!(plan(CONFIG_TEXT, &fields(), &small, &BTreeSet::from([6]), 4).is_ok());
    // A changed picker past the PC is refused.
    let mut far = fields();
    far.cpus.pump = Some(9);
    assert_eq!(
        plan(CONFIG_TEXT, &fields(), &far, &BTreeSet::new(), 8),
        Err("Audio pump CPU: CPU 9, this PC has CPUs 0 to 7".to_string())
    );
    let mut cpu = fields();
    cpu.cpus.rx = Some(1);
    assert!(plan("name = ", &fields(), &cpu, &BTreeSet::new(), 8).is_err());
    let mut big = fields();
    big.latency_ns = u64::from(u32::MAX) + 1;
    assert_eq!(
        plan(CONFIG_TEXT, &fields(), &big, &BTreeSet::new(), 8),
        Err("the RX latency is too large".to_string())
    );
}

const STATUS: &str = r#"{"clock":{"ready":true,"source":"dsync","holdover":false},"name":"test-asio","app":{"line":"DAW attached · 256 @ 96 kHz","state":"ok","daw_attached":true,"block":256}}"#;

#[test]
fn the_status_line_shows_the_app_line_and_the_clock() {
    assert_eq!(
        status_line(&Ok(STATUS.to_string())),
        "Running · DAW attached · 256 @ 96 kHz · clock: dsync ready"
    );
    let bare = r#"{"clock":{"ready":false}}"#;
    assert_eq!(
        status_line(&Ok(bare.to_string())),
        "Running · clock: ? not ready"
    );
    assert_eq!(
        status_line(&Ok("garbage".to_string())),
        "Running · status not readable"
    );
    assert_eq!(
        status_line(&Err("no answer".to_string())),
        "Not running (no answer)"
    );
}

#[test]
fn the_daw_and_the_block_come_from_the_app_part() {
    assert_eq!(daw_attached(STATUS), Some(true));
    assert_eq!(
        daw_attached(&STATUS.replace("\"daw_attached\":true", "\"daw_attached\":false")),
        Some(false)
    );
    assert_eq!(daw_attached(r#"{"app":{}}"#), None);
    assert_eq!(daw_attached(r#"{"name":"x"}"#), None);
    assert_eq!(daw_attached("garbage"), None);
    assert_eq!(status_block(STATUS), Some(256));
    assert_eq!(status_block(r#"{"app":{"block":"x"}}"#), None);
}

fn back_of(f: &Fields) -> ReadBack {
    ReadBack {
        block: Some(f.block),
        rx: Some(f.rx.len()),
        tx: Some(f.tx.len()),
        latency_ns: Some(f.latency_ns),
        iface: f.iface.clone(),
    }
}

#[test]
fn a_read_back_as_applied_is_the_done_line() {
    let f = fields();
    assert_eq!(
        verify(&f, &back_of(&f)),
        Ok(
            "Applied: test-asio runs with buffer 256, 4 in / 2 out, RX latency 2 ms. \
            CPUs and TX latency are in the file (the device does not report them)."
                .to_string()
        )
    );
    // No interface asked: the adapter is not checked.
    let mut any = f.clone();
    any.iface = None;
    let mut back = back_of(&f);
    back.iface = None;
    assert!(verify(&any, &back).is_ok());
}

#[test]
fn every_mismatch_and_every_value_not_read_is_listed() {
    let f = fields();
    let back = ReadBack {
        block: Some(128),
        rx: Some(4),
        tx: None,
        latency_ns: Some(1_000_000),
        iface: Some("Ethernet".into()),
    };
    assert_eq!(
        verify(&f, &back),
        Err(
            "Buffer: 256 applied, 128 read back; Outputs: not read back; \
             Dante latency (RX): 2 ms applied, 1 ms read back; \
             Ethernet interface: Ethernet 2 applied, Ethernet read back"
                .to_string()
        )
    );
    let mut rx = back_of(&f);
    rx.rx = Some(3);
    assert_eq!(
        verify(&f, &rx),
        Err("Inputs: 4 applied, 3 read back".to_string())
    );
    let mut iface = back_of(&f);
    iface.iface = None;
    assert_eq!(
        verify(&f, &iface),
        Err("Ethernet interface: not read back".to_string())
    );
}

/// A root with instance 1 holding [`CONFIG_TEXT`].
fn one_instance() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().expect("a temp dir");
    let dir = root.path().join("instance-1");
    std::fs::create_dir(&dir).expect("a folder");
    std::fs::write(dir.join(CONFIG), CONFIG_TEXT).expect("a config");
    (root, dir)
}

#[test]
fn install_writes_the_window_s_edit_into_an_existing_instance() {
    let (root, dir) = one_instance();
    let mut new = fields();
    new.block = 64;
    let text = edit(CONFIG_TEXT, &new).expect("the edit");
    assert_eq!(install(root.path(), 1, &text), Ok(dir.join(CONFIG)));
    assert_eq!(
        std::fs::read_to_string(dir.join(CONFIG)).expect("read"),
        text
    );
    assert!(!dir.join(STAGED).exists());
}

#[test]
fn install_refuses_a_bad_config_and_a_missing_instance() {
    let (root, dir) = one_instance();
    let err = install(root.path(), 1, "name = ").expect_err("bad");
    assert!(err.starts_with("the new config is refused: "), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.join(CONFIG)).expect("read"),
        CONFIG_TEXT
    );
    assert!(!dir.join(STAGED).exists());
    let missing = install(root.path(), 2, CONFIG_TEXT).expect_err("missing");
    assert!(missing.contains("instance-2"), "{missing}");
    assert!(!root.path().join("instance-2").exists());
}

#[test]
fn install_refuses_a_file_that_changes_more_than_the_window_s_fields() {
    let (root, dir) = one_instance();
    // A swapped staged file: a valid config that moves the state file.
    let swapped = CONFIG_TEXT.replace("instance-1/state.toml", "elsewhere/state.toml");
    assert_eq!(
        install(root.path(), 1, &swapped),
        Err(
            "the new config changes more than the settings window's fields; nothing written"
                .to_string()
        )
    );
    // Its comments changed too: refused the same way.
    let comment = CONFIG_TEXT.replace("# band instance (test)", "# other");
    assert!(install(root.path(), 1, &comment).is_err());
    // Another device name is refused by the edit itself.
    let named = CONFIG_TEXT.replace("\"test-asio\"", "\"other\"");
    let err = install(root.path(), 1, &named).expect_err("name");
    assert!(err.starts_with("the new config is refused: "), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.join(CONFIG)).expect("read"),
        CONFIG_TEXT
    );
}

#[test]
fn the_helper_takes_plain_instance_numbers_only() {
    assert_eq!(instance_arg("3"), Ok(3));
    for bad in ["", "-1", "1 ", "x", "../1"] {
        assert_eq!(
            instance_arg(bad),
            Err(format!("instance: {bad} is not an instance number"))
        );
    }
}

/// A root with instance 1 (an old config), and a staged new config.
fn staged_root() -> (tempfile::TempDir, PathBuf, String) {
    let root = tempfile::tempdir().expect("a temp dir");
    let dir = root.path().join("instance-1");
    std::fs::create_dir(&dir).expect("a folder");
    std::fs::write(dir.join(CONFIG), CONFIG_TEXT).expect("a config");
    let mut new = fields();
    new.block = 128;
    let text = edit(CONFIG_TEXT, &new).expect("the edit");
    let staged = root.path().join("staged.toml");
    std::fs::write(&staged, &text).expect("staged");
    (root, staged, text)
}

#[test]
fn the_helper_writes_the_file_then_restarts_the_instance_s_service() {
    let (root, staged, text) = staged_root();
    let mut restarted = Vec::new();
    let line = apply_staged(root.path(), "1", &staged, |s| {
        restarted.push(s.to_string());
        Ok(())
    })
    .expect("applied");
    let config = root.path().join("instance-1").join(CONFIG);
    assert_eq!(
        line,
        format!("{} written, dasio-1 restarted", config.display())
    );
    assert_eq!(restarted, ["dasio-1"]);
    assert_eq!(std::fs::read_to_string(config).expect("read"), text);
}

#[test]
fn the_helper_says_when_the_file_was_written_but_the_restart_failed() {
    let (root, staged, _) = staged_root();
    let err = apply_staged(root.path(), "1", &staged, |_| Err("no".into())).expect_err("fails");
    let config = root.path().join("instance-1").join(CONFIG);
    assert_eq!(
        err,
        format!(
            "{} was written, but dasio-1 did not restart: no",
            config.display()
        )
    );
}

#[test]
fn the_helper_restarts_nothing_when_it_writes_nothing() {
    let (root, staged, _) = staged_root();
    let never = |_: &str| -> Result<(), String> { panic!("no restart") };
    assert!(apply_staged(root.path(), "x", &staged, never).is_err());
    assert!(apply_staged(root.path(), "2", &staged, never).is_err());
    let missing = root.path().join("missing.toml");
    let err = apply_staged(root.path(), "1", &missing, never).expect_err("no file");
    assert!(err.starts_with(&missing.display().to_string()), "{err}");
    std::fs::write(&staged, "name = ").expect("bad");
    assert!(apply_staged(root.path(), "1", &staged, never).is_err());
    assert_eq!(
        std::fs::read_to_string(root.path().join("instance-1").join(CONFIG)).expect("read"),
        CONFIG_TEXT
    );
}

#[test]
fn the_verdict_file_sits_in_the_instance_s_folder_and_reads_back() {
    assert_eq!(
        result_path(Path::new("/d"), 2),
        Path::new("/d")
            .join("instance-2")
            .join("dtray-apply.result")
    );
    for outcome in [Ok("done".to_string()), Err("refused\nhere".to_string())] {
        assert_eq!(parse_result(&result_text(&outcome)), outcome);
    }
    assert_eq!(result_text(&Ok("a".into())), "ok\na");
    assert_eq!(result_text(&Err("b".into())), "error\nb");
    assert_eq!(
        parse_result("what"),
        Err("the helper's verdict is not readable: \"what\"".to_string())
    );
    assert!(parse_result("other\nx").is_err());
}

#[test]
fn the_helper_s_parameters_quote_the_file() {
    assert_eq!(
        helper_params(2, Path::new("C:/Users/A B/new.toml")),
        "--dasio-apply 2 \"C:/Users/A B/new.toml\""
    );
}

#[test]
fn only_a_dtray_under_program_files_may_elevate() {
    let pf = Path::new("C:/Program Files");
    assert_eq!(
        may_elevate(Path::new("C:/Program Files/dasio/dtray.exe"), Some(pf)),
        Ok(())
    );
    assert_eq!(
        may_elevate(Path::new("c:/program files/DASIO/dtray.exe"), Some(pf)),
        Ok(())
    );
    let err = may_elevate(Path::new("C:/Users/a/dtray.exe"), Some(pf)).expect_err("user");
    assert_eq!(
        err,
        "settings are changed only by a dtray installed under C:/Program Files \
         (this one runs from C:/Users/a/dtray.exe)"
    );
    // The folder itself and a look-alike prefix are not inside it.
    assert!(may_elevate(pf, Some(pf)).is_err());
    assert!(may_elevate(Path::new("C:/Program Files2/dtray.exe"), Some(pf)).is_err());
    assert!(may_elevate(Path::new("C:/x.exe"), None).is_err());
    assert!(may_elevate(Path::new("C:/x.exe"), Some(Path::new(""))).is_err());
}

#[test]
fn only_the_changed_cpu_pickers_are_checked() {
    let old = Cpus {
        pump: Some(1),
        io: Some(2),
        rx: None,
    };
    let new = Cpus {
        pump: Some(1),
        io: Some(3),
        rx: Some(4),
    };
    assert_eq!(
        changed_cpus(&old, &new),
        Cpus {
            pump: None,
            io: Some(3),
            rx: Some(4)
        }
    );
    assert_eq!(changed_cpus(&old, &old), Cpus::default());
    let any = Cpus { pump: None, ..old };
    assert_eq!(changed_cpus(&old, &any), Cpus::default());
}

#[cfg(unix)]
#[test]
fn install_refuses_a_linked_folder() {
    let (root, dir) = one_instance();
    let linked = tempfile::tempdir().expect("a temp dir");
    std::os::unix::fs::symlink(&dir, linked.path().join("instance-2")).expect("a link");
    let err = install(linked.path(), 2, CONFIG_TEXT).expect_err("linked");
    assert!(err.ends_with("is a link; nothing written"), "{err}");
    let root_link = linked.path().join("root");
    std::os::unix::fs::symlink(root.path(), &root_link).expect("a link");
    let err = install(&root_link, 1, CONFIG_TEXT).expect_err("linked root");
    assert!(err.ends_with("is a link; nothing written"), "{err}");
    // The same instance through its real folder is written.
    assert!(install(root.path(), 1, CONFIG_TEXT).is_ok());
}

const SEC: std::time::Duration = std::time::Duration::from_secs(1);

#[test]
fn a_restart_lasts_until_the_device_is_back_after_it_went_away() {
    let r = Restart::<std::time::Duration>::started();
    assert_eq!(r.gone, None);
    // The old process still answers (UAC, the helper's write).
    assert!(!r.restarting());
    let r = r.next(true, true, SEC).expect("still restarting");
    assert_eq!((r.up, r.gone), (true, None));
    assert!(!r.restarting());
    // Gone at 3 s.
    let r = r.next(false, true, 3 * SEC).expect("still restarting");
    assert_eq!(r.gone, Some(3 * SEC));
    assert!(r.restarting());
    // Still gone later, also after the apply ended: the first miss counts.
    let r = r.next(false, true, 9 * SEC).expect("still restarting");
    assert_eq!(r.gone, Some(3 * SEC));
    let r = r.next(false, false, 20 * SEC).expect("still restarting");
    assert_eq!(r.gone, Some(3 * SEC));
    // Back.
    assert_eq!(r.next(true, false, 23 * SEC), None);
    assert_eq!(r.next(true, true, 23 * SEC), None);
}

#[test]
fn a_restart_ends_after_the_wait_from_when_the_device_went_away() {
    let r = Restart {
        up: true,
        gone: Some(10 * SEC),
    };
    let just = std::time::Duration::from_millis(1);
    assert_eq!(r.next(false, true, 10 * SEC + RESTART_WAIT - just), Some(r));
    assert_eq!(r.next(false, true, 10 * SEC + RESTART_WAIT), None);
    assert_eq!(r.next(false, false, 10 * SEC + RESTART_WAIT), None);
    assert_eq!(RESTART_WAIT, 60 * SEC);
}

#[test]
fn an_apply_that_ends_with_the_device_never_gone_ends_the_restart() {
    let r = Restart::<std::time::Duration>::started();
    // Declined UAC, a refused file: the device stayed up.
    assert_eq!(r.next(true, false, SEC), None);
    // An instance that was not running and the apply failed: no
    // "Restarting…" either.
    assert_eq!(r.next(false, false, SEC), None);
}

#[test]
fn an_instance_down_before_the_apply_shows_its_status_not_restarting() {
    let r = Restart::<std::time::Duration>::started();
    // Down while UAC asks: the real status, and a failed apply ends it.
    let r = r.next(false, true, SEC).expect("the apply runs");
    assert_eq!(r, Restart::started());
    assert!(!r.restarting());
    assert_eq!(r.next(false, false, 2 * SEC), None);
    // The helper starts it, then a restart takes it away again.
    let r = r.next(true, true, 2 * SEC).expect("the apply runs");
    let r = r.next(false, true, 3 * SEC).expect("the apply runs");
    assert_eq!(r.gone, Some(3 * SEC));
    assert!(r.restarting());
}

#[test]
fn the_status_says_restarting_only_while_the_device_is_missing() {
    let missing = Err("not found on this PC".to_string());
    assert_eq!(shown_status(&missing, true), "Restarting…");
    assert_eq!(
        shown_status(&missing, false),
        "Not running (not found on this PC)"
    );
    let up = Ok(STATUS.to_string());
    assert_eq!(shown_status(&up, true), status_line(&up));
    assert_eq!(shown_status(&up, false), status_line(&up));
}
