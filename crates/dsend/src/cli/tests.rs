// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

use std::net::Ipv4Addr;

fn args(text: &str) -> Vec<String> {
    text.split_whitespace().map(String::from).collect()
}

fn run(text: &str) -> Options {
    match parse(&args(text)) {
        Ok(Command::Run(o)) => o,
        other => panic!("{text:?}: {other:?}"),
    }
}

fn error(text: &str) -> String {
    match parse(&args(text)) {
        Err(e) => e,
        other => panic!("{text:?} should fail: {other:?}"),
    }
}

#[test]
fn no_arguments_run_at_the_defaults() {
    assert_eq!(
        run(""),
        Options {
            name: None,
            output: None,
            rate: 96_000,
            budget_ms: None,
            place: None,
            ports: None,
            process_id: None,
            state: None,
        }
    );
}

#[test]
fn every_option_is_read() {
    let o = run(
        "--name foh-send --output speakers --rate 96000 --budget-ms 20 --ip 192.0.2.7 \
         --ports 4441,8801,4456 --process-id 3 --state /tmp/x.toml",
    );
    assert_eq!(o.name.as_deref(), Some("foh-send"));
    assert_eq!(o.output.as_deref(), Some("speakers"));
    assert_eq!(o.rate, 96_000);
    assert_eq!(o.budget_ms, Some(20));
    assert_eq!(o.place, Some(Place::Ip(Ipv4Addr::new(192, 0, 2, 7))));
    assert_eq!(
        o.ports,
        Some(Ports {
            arc: 4441,
            cmc: 8801,
            flows: 4456
        })
    );
    assert_eq!(o.process_id, Some(3));
    assert_eq!(o.state, Some(PathBuf::from("/tmp/x.toml")));
    assert_eq!(run("--iface eth1").place, Some(Place::Iface("eth1".into())));
}

#[test]
fn list_help_and_version_are_their_own_commands() {
    assert_eq!(parse(&args("--list-outputs")), Ok(Command::ListOutputs));
    assert_eq!(
        parse(&args("--rate 96000 --list-outputs")),
        Ok(Command::ListOutputs)
    );
    assert_eq!(parse(&args("--help")), Ok(Command::Help));
    assert_eq!(parse(&args("-h --bogus")), Ok(Command::Help));
    assert_eq!(parse(&args("--version")), Ok(Command::Version));
    assert_eq!(parse(&args("-V")), Ok(Command::Version));
}

#[test]
fn bad_command_lines_say_what_is_wrong() {
    assert_eq!(error("--bogus"), "unknown argument \"--bogus\"");
    assert_eq!(error("speakers"), "unknown argument \"speakers\"");
    assert_eq!(error("--output"), "--output needs a value");
    assert_eq!(error("--rate x"), "--rate needs a number, not \"x\"");
    assert_eq!(error("--rate 0"), "--rate needs a rate over 0 Hz");
    assert_eq!(
        error("--budget-ms 0"),
        "--budget-ms needs a budget over 0 ms"
    );
    assert_eq!(
        error("--ip nowhere"),
        "--ip needs an IPv4 address, not \"nowhere\""
    );
    assert_eq!(
        error("--ip 192.0.2.1 --iface eth0"),
        "give one of --ip and --iface, once"
    );
    assert_eq!(
        error("--iface eth0 --ip 192.0.2.1"),
        "give one of --ip and --iface, once"
    );
    assert_eq!(
        error("--process-id x"),
        "--process-id needs a number, not \"x\""
    );
    assert_eq!(
        error("--ports 1,2"),
        "--ports is <arc>,<cmc>,<flows>, not \"1,2\""
    );
    // dlisten's flags are not dsend's.
    assert_eq!(error("--channels 2"), "unknown argument \"--channels\"");
}

#[test]
fn empty_values_are_refused() {
    let one = |flag: &str| parse(&[flag.to_string(), String::new()]).unwrap_err();
    assert_eq!(one("--output"), "--output needs a name");
    assert_eq!(one("--iface"), "--iface needs a name");
    assert_eq!(one("--state"), "--state needs a path");
}

#[test]
fn only_windows_can_capture_and_the_error_says_why() {
    assert_eq!(capture_check(true), Ok(()));
    let e = capture_check(false).unwrap_err();
    assert!(
        e.starts_with("dsend captures an output's sound by WASAPI loopback"),
        "{e}"
    );
    assert!(e.ends_with("run dsend on Windows"), "{e}");
}

#[test]
fn the_default_name_is_the_host_and_dsend() {
    assert_eq!(default_name("FOH-PC"), "FOH-PC-dsend");
    assert_eq!(default_name(""), "pc-dsend");
    assert_eq!(default_name("my_pc"), "my-pc-dsend");
}

#[test]
fn the_state_file_lives_in_dsend_s_folder() {
    let os = |s: &str| Some(OsString::from(s));
    assert_eq!(
        state_path(false, "pc-dsend", None, os("/x"), os("/h"), "/t".into()),
        PathBuf::from("/x/dstack/dsend/pc-dsend.toml")
    );
    assert_eq!(
        state_path(true, "pc-dsend", os("C:\\L"), None, None, "/t".into()),
        PathBuf::from("C:\\L")
            .join("dstack")
            .join("dsend")
            .join("pc-dsend.toml")
    );
}

#[test]
fn the_usage_names_every_flag() {
    for flag in VALUE_FLAGS {
        assert!(USAGE.contains(&format!("  {flag} ")), "{flag}");
    }
    assert!(USAGE.contains("  --list-outputs "));
    assert!(USAGE.starts_with("usage: dsend [options]"));
}

#[test]
fn the_link_config_is_a_tx_only_device_l_and_r_with_the_options() {
    let state = PathBuf::from("/tmp/s.toml");
    let plain = link_config(&run(""), "pc-dsend", state.clone());
    assert_eq!(
        plain,
        Config::new("pc-dsend", &state)
            .tx(["L", "R"])
            .sample_rate(96_000)
            .product("dstack", "dsend")
    );
    let dev = plain.device().unwrap();
    assert!(dev.rx.is_empty(), "TX only");
    assert_eq!(dev.tx, ["L", "R"]);
    assert_eq!(dev.sample_rate, 96_000);
    let o = run("--rate 96000 --budget-ms 20 --ports 4441,8801,4456 --process-id 3");
    assert_eq!(
        link_config(&o, "n", state.clone()),
        Config::new("n", &state)
            .tx(["L", "R"])
            .sample_rate(96_000)
            .product("dstack", "dsend")
            .budget_ns(20_000_000)
            .ports(Ports {
                arc: 4441,
                cmc: 8801,
                flows: 4456
            })
            .process_id(3)
    );
}

#[test]
fn a_run_never_shows_a_box_its_failure_goes_to_the_log() {
    // dstack #225: a logon task starts dsend with no console and no
    // stderr; a failed start stayed on the desktop as a modal box until
    // someone clicked it. A run has no window and logs to its file.
    for (attached, has_stderr) in [(false, false), (true, false), (false, true), (true, true)] {
        assert!(
            !shows_box(true, true, attached, has_stderr),
            "{attached} {has_stderr}"
        );
    }
    // A run whose log file could not be opened leaves no other trace on a
    // start from Explorer or a task: the box tells it.
    assert!(shows_box(true, false, false, false));
    assert!(
        !shows_box(true, false, true, false),
        "a command line's console"
    );
    // A list started from Explorer still tells its failure in a box.
    for logged in [false, true] {
        assert!(shows_box(false, logged, false, false));
        assert!(
            !shows_box(false, logged, true, false),
            "a command line's console"
        );
        assert!(
            !shows_box(false, logged, false, true),
            "a script's redirected stderr"
        );
    }
}
