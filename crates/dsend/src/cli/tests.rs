// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

use std::net::Ipv4Addr;

use dstack_endpoint::cli::WASAPI_PERIOD_NS;

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
            latency: Latency::default(),
            latency_ns: None,
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
        "--name foh-send --output speakers --rate 96000 --network-latency-ms 0.5 --receive-buffer-ms 9 --ip 192.0.2.7 \
         --ports 4441,8801,4456 --process-id 3 --state /tmp/x.toml",
    );
    assert_eq!(o.name.as_deref(), Some("foh-send"));
    assert_eq!(o.output.as_deref(), Some("speakers"));
    assert_eq!(o.rate, 96_000);
    assert_eq!(
        o.latency,
        Latency {
            network_ns: Some(500_000),
            receive_buffer_ns: Some(9_000_000),
            app_buffer_ns: None,
        }
    );
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
        error("--network-latency-ms 1.x"),
        "--network-latency-ms needs ms with up to 6 decimals, not \"1.x\""
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
    assert!(USAGE.contains(&format!("  {LATENCY_FLAG} ")));
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
            .receive_buffer_ns(9_000_000)
            .app_buffer(WASAPI_PERIOD_NS, None)
    );
    let dev = plain.device().unwrap();
    assert!(dev.rx.is_empty(), "TX only");
    assert_eq!(dev.tx, ["L", "R"]);
    assert_eq!(dev.sample_rate, 96_000);
    let o = run(
        "--rate 96000 --network-latency-ms 0.5 --receive-buffer-ms 9 --ports 4441,8801,4456 --process-id 3",
    );
    assert_eq!(
        link_config(&o, "n", state.clone()),
        Config::new("n", &state)
            .tx(["L", "R"])
            .sample_rate(96_000)
            .product("dstack", "dsend")
            .network_latency_ns(500_000)
            .receive_buffer_ns(9_000_000)
            .ports(Ports {
                arc: 4441,
                cmc: 8801,
                flows: 4456
            })
            .process_id(3)
            .app_buffer(WASAPI_PERIOD_NS, None)
    );
}

/// dstack #247: dsend's FIFO target is WASAPI's 10 ms period and one pump
/// block at the network's rate (11.333 ms at 96 kHz, was a fixed 40 ms),
/// and `--app-buffer-ms` sets it as given.
#[test]
fn the_app_buffer_is_the_wasapi_period_and_a_block_or_the_flag() {
    let state = PathBuf::from("/tmp/s.toml");
    let plain = link_config(&run(""), "pc-dsend", state.clone());
    let b = plain.bridge_buffer().unwrap();
    assert_eq!((b.chunk_ns(), b.block_ns()), (10_000_000, 1_333_333));
    assert_eq!(b.target_ns(), 11_333_333);
    let at_48k = link_config(&run("--rate 48000"), "pc-dsend", state.clone());
    assert_eq!(at_48k.bridge_buffer().unwrap().target_ns(), 12_666_667);
    let set = link_config(&run("--app-buffer-ms 40"), "pc-dsend", state);
    assert_eq!(set.bridge_buffer().unwrap().target_ns(), 40_000_000);
    assert_eq!(
        run("--app-buffer-ms 40").latency.app_buffer_ns,
        Some(40_000_000)
    );
    assert!(parse(&args("--app-buffer-ms 200")).is_err());
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

/// The smallest maximum latency a hardware Dante receiver on the site
/// states in its ARC settings (0x1100 key 0x8302), ns: a Dante card
/// (`arc_settings_card.json`) and the wireless receiver read 10156250, a
/// second card 10666667 (dstack's catalog fixtures). A transmitter whose
/// latency is past a receiver's maximum leaves that receiver's subscription
/// in "Incorrect channel latencies" (dstack #480: a Yamaha AIC128-D on
/// dsend's 40 ms).
const HARDWARE_RECEIVER_MAX_NS: u64 = 10_156_250;

/// dstack #480 (regression of #461): dsend's default Dante latency is 10
/// ms, the largest value common hardware receivers accept. #461's 40 ms put
/// the FOH card's subscription in "Incorrect channel latencies" (the card
/// ran 9 ms before). As much margin as the receivers allow (owner, 20.11:
/// dsend is a rare fallback whose latency may go up), never past them.
#[test]
fn dsend_s_default_latency_fits_common_hardware_receivers() {
    let state = PathBuf::from("/tmp/s.toml");
    for (flags, network_ns) in [("", 1_000_000), ("--network-latency-ms 0.5", 500_000)] {
        for rate in ["96000", "48000", "44100"] {
            let o = run(&format!("{flags} --rate {rate}"));
            let split = link_config(&o, "pc-dsend", state.clone()).split().unwrap();
            assert!(
                split.latency_ns <= HARDWARE_RECEIVER_MAX_NS,
                "{flags} {rate}: {split:?} is past what hardware receivers accept"
            );
            assert_eq!(split.latency_ns, 10_000_000, "{flags} {rate}: {split:?}");
            assert_eq!(split.network_latency_ns, network_ns, "{flags} {rate}");
            assert_eq!(split.receive_buffer_ns, 10_000_000 - network_ns);
        }
    }
    // (b) set: as given, (a) + (b).
    let set = link_config(&run("--receive-buffer-ms 9"), "pc-dsend", state);
    assert_eq!(set.split().unwrap().latency_ns, 10_000_000);
    assert_eq!(LATENCY_NS, 10_000_000);
}

/// dstack #480: `--latency-ms` sets dsend's whole Dante latency, (b) =
/// it less (a); it never comes with `--receive-buffer-ms`, and never under
/// (a). `--receive-buffer-ms` alone keeps working.
#[test]
fn the_latency_flag_sets_the_dante_latency() {
    let state = PathBuf::from("/tmp/s.toml");
    let o = run("--latency-ms 5");
    assert_eq!(o.latency_ns, Some(5_000_000));
    assert_eq!(o.latency, Latency::default());
    let split = link_config(&o, "pc-dsend", state.clone()).split().unwrap();
    assert_eq!(
        (
            split.latency_ns,
            split.network_latency_ns,
            split.receive_buffer_ns
        ),
        (5_000_000, 1_000_000, 4_000_000)
    );
    // With (a) set: the total stays, (b) follows.
    let o = run("--network-latency-ms 0.5 --latency-ms 20");
    let split = link_config(&o, "pc-dsend", state).split().unwrap();
    assert_eq!(
        (split.latency_ns, split.receive_buffer_ns),
        (20_000_000, 19_500_000)
    );
    // A total equal to (a) is taken: (b) 0 (dlink then refuses what is
    // under its TX floor).
    assert_eq!(run("--latency-ms 1").latency_ns, Some(1_000_000));
    // A whole latency with (b), or under (a), is a usage error.
    let both = "give one of --latency-ms (the whole latency) and --receive-buffer-ms (b)";
    assert_eq!(error("--latency-ms 10 --receive-buffer-ms 9"), both);
    assert_eq!(error("--receive-buffer-ms 9 --latency-ms 10"), both);
    assert_eq!(
        error("--latency-ms 0.999999"),
        "--latency-ms 0.999999 is under the network latency (a) 1 ms it holds"
    );
    assert_eq!(
        error("--network-latency-ms 2 --latency-ms 1.5"),
        "--latency-ms 1.5 is under the network latency (a) 2 ms it holds"
    );
    assert!(error("--latency-ms ten").starts_with("--latency-ms needs ms"));
    assert_eq!(error("--latency-ms"), "--latency-ms needs a value");
    // `--list-outputs` checks the options too.
    assert!(parse(&args("--list-outputs --latency-ms 0.5")).is_err());
    assert_eq!(
        parse(&args("--list-outputs --latency-ms 30")),
        Ok(Command::ListOutputs)
    );
    // latency_of: (b) given wins; a total under (a) never wraps.
    let given = Latency {
        network_ns: Some(2_000_000),
        receive_buffer_ns: None,
        app_buffer_ns: Some(5_000_000),
    };
    assert_eq!(
        latency_of(given, None),
        Latency {
            receive_buffer_ns: Some(8_000_000),
            ..given
        }
    );
    assert_eq!(
        latency_of(given, Some(40_000_000)).receive_buffer_ns,
        Some(38_000_000)
    );
    let b = Latency {
        receive_buffer_ns: Some(3_000_000),
        ..given
    };
    assert_eq!(latency_of(b, Some(40_000_000)), b);
    assert_eq!(
        latency_of(given, Some(1_000_000)).receive_buffer_ns,
        Some(0)
    );
    assert_eq!(latency_check(given, None), Ok(()));
    assert_eq!(latency_check(b, None), Ok(()));
    assert_eq!(latency_check(given, Some(2_000_000)), Ok(()));
}
