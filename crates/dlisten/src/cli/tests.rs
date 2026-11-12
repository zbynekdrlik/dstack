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
    let o = run("");
    assert_eq!(
        o,
        Options {
            name: None,
            channels: 2,
            output: None,
            subscribe: vec![],
            rate: 48_000,
            latency: Latency::default(),
            place: None,
            ports: None,
            process_id: None,
            state: None,
            mono: false,
        }
    );
}

#[test]
fn every_option_is_read() {
    let o = run(
        "--name foh-listen --channels 4 --output speakers --subscribe 1=Out@tx-a \
         --subscribe 4=Mix@tx-b --rate 96000 --network-latency-ms 0.5 --receive-buffer-ms 9 --ip 192.0.2.7 \
         --ports 4441,8801,4456 --process-id 3 --state /tmp/x.toml",
    );
    assert_eq!(o.name.as_deref(), Some("foh-listen"));
    assert_eq!(o.channels, 4);
    assert_eq!(o.output.as_deref(), Some("speakers"));
    assert_eq!(
        o.subscribe,
        [
            Subscribe {
                rx: 1,
                tx_channel: "Out".into(),
                tx_device: "tx-a".into()
            },
            Subscribe {
                rx: 4,
                tx_channel: "Mix".into(),
                tx_device: "tx-b".into()
            }
        ]
    );
    assert_eq!(o.rate, 96_000);
    assert_eq!(
        o.latency,
        Latency {
            network_ns: Some(500_000),
            receive_buffer_ns: Some(9_000_000),
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
        parse(&args("--channels 1 --list-outputs")),
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
    assert_eq!(error("--channels 0"), "--channels is 1 to 64, not 0");
    assert_eq!(error("--channels 65"), "--channels is 1 to 64, not 65");
    assert_eq!(run("--channels 64").channels, 64);
    assert_eq!(run("--channels 1").channels, 1);
    assert_eq!(
        error("--channels two"),
        "--channels needs a number, not \"two\""
    );
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
        error("--subscribe 3=Out@tx"),
        "--subscribe 3: the device has 2 RX channels"
    );
    assert_eq!(run("--subscribe 2=Out@tx").subscribe[0].rx, 2);
}

#[test]
fn empty_values_are_refused() {
    let empty = |flag: &str| parse(&[flag.to_string(), String::new()]);
    assert_eq!(empty("--output"), Err("--output needs a name".into()));
    assert_eq!(empty("--iface"), Err("--iface needs a name".into()));
    assert_eq!(empty("--state"), Err("--state needs a path".into()));
}

#[test]
fn a_subscription_splits_at_the_first_equals_and_the_last_at() {
    assert_eq!(
        parse_subscribe("2=Ch@1@dev-b"),
        Ok(Subscribe {
            rx: 2,
            tx_channel: "Ch@1".into(),
            tx_device: "dev-b".into()
        })
    );
    assert_eq!(parse_subscribe(" 1 =A=B@d").unwrap().tx_channel, "A=B");
    for bad in [
        "1Out@dev",
        "1=Out",
        "0=Out@dev",
        "x=Out@dev",
        "1=@dev",
        "1=Out@",
    ] {
        assert_eq!(
            parse_subscribe(bad),
            Err(format!(
                "--subscribe is <rx>=<channel>@<device>, not {bad:?}"
            )),
            "{bad}"
        );
    }
}

#[test]
fn ports_are_three_ports_over_zero() {
    assert_eq!(
        parse_ports("1,2,3"),
        Ok(Ports {
            arc: 1,
            cmc: 2,
            flows: 3
        })
    );
    for bad in ["1,2", "1,2,3,4", "0,2,3", "1,x,3", "", "1,2,70000"] {
        assert_eq!(
            parse_ports(bad),
            Err(format!("--ports is <arc>,<cmc>,<flows>, not {bad:?}")),
            "{bad}"
        );
    }
}

#[test]
fn the_default_name_is_a_dante_name_of_at_most_31() {
    assert_eq!(default_name("STUDIO-PC"), "STUDIO-PC-dlisten");
    assert_eq!(default_name("my_pc.local"), "my-pc-local-dlisten");
    assert_eq!(default_name("-x-"), "x-dlisten");
    assert_eq!(default_name(""), "pc-dlisten");
    assert_eq!(default_name("__"), "pc-dlisten");
    let long = default_name("abcdefghijklmnopqrstuvwxyz0123");
    assert_eq!(long, "abcdefghijklmnopqrstuvw-dlisten");
    assert_eq!(long.len(), NAME_MAX);
    // A cut that ends on '-' drops it.
    assert_eq!(
        default_name("abcdefghijklmnopqrstuv-wxyz"),
        "abcdefghijklmnopqrstuv-dlisten"
    );
}

#[test]
fn two_channels_are_l_and_r_others_are_numbered() {
    assert_eq!(rx_names(2), ["L", "R"]);
    assert_eq!(rx_names(1), ["In 1"]);
    assert_eq!(rx_names(3), ["In 1", "In 2", "In 3"]);
}

#[test]
fn the_state_file_lives_in_the_user_s_state_folder() {
    let os = |s: &str| Some(OsString::from(s));
    let temp = PathBuf::from("/tmp");
    let file = |base: &str| {
        PathBuf::from(base)
            .join("dstack")
            .join("dlisten")
            .join("pc-dlisten.toml")
    };
    assert_eq!(
        state_path(
            true,
            "pc-dlisten",
            os("C:\\Users\\u\\AppData\\Local"),
            os("/x"),
            os("/h"),
            temp.clone()
        ),
        file("C:\\Users\\u\\AppData\\Local")
    );
    assert_eq!(
        state_path(true, "pc-dlisten", None, os("/x"), os("/h"), temp.clone()),
        file("/tmp")
    );
    assert_eq!(
        state_path(
            false,
            "pc-dlisten",
            os("/l"),
            os("/x"),
            os("/h"),
            temp.clone()
        ),
        file("/x")
    );
    assert_eq!(
        state_path(false, "pc-dlisten", None, os(""), os("/h"), temp.clone()),
        file("/h/.local/state")
    );
    assert_eq!(
        state_path(false, "pc-dlisten", None, None, os(""), temp.clone()),
        file("/tmp")
    );
}

#[test]
fn the_usage_names_every_flag() {
    for flag in VALUE_FLAGS {
        assert!(USAGE.contains(&format!("  {flag} ")), "{flag}");
    }
    assert!(USAGE.contains("  --list-outputs "));
    assert!(USAGE.contains("  --autostart on|off "));
}

#[test]
fn the_link_config_is_an_rx_only_device_with_the_options() {
    let state = PathBuf::from("/tmp/s.toml");
    let plain = link_config(&run(""), "pc-dlisten", state.clone());
    assert_eq!(
        plain,
        Config::new("pc-dlisten", &state)
            .rx(["L", "R"])
            .sample_rate(48_000)
            .product("dstack", "dlisten")
    );
    let dev = plain.device().unwrap();
    assert!(dev.tx.is_empty(), "RX only");
    assert_eq!(dev.sample_rate, 48_000);
    assert_eq!(
        plain.split().unwrap(),
        Config::new("x", &state).rx(["L", "R"]).split().unwrap()
    );

    let o = run(
        "--channels 1 --rate 96000 --network-latency-ms 0.5 --receive-buffer-ms 9 --ports 4441,8801,4456 \
         --process-id 3 --subscribe 1=Out@tx-a",
    );
    let full = link_config(&o, "n", state.clone());
    assert_eq!(
        full,
        Config::new("n", &state)
            .rx(["In 1"])
            .sample_rate(96_000)
            .product("dstack", "dlisten")
            .network_latency_ns(500_000)
            .receive_buffer_ns(9_000_000)
            .ports(Ports {
                arc: 4441,
                cmc: 8801,
                flows: 4456
            })
            .process_id(3)
            .subscribe(1, "Out", "tx-a")
    );
}

#[test]
fn autostart_keeps_the_other_options_for_the_logon_start() {
    assert_eq!(
        parse(&args("--ip 192.0.2.7 --autostart on --channels 1")),
        Ok(Command::Autostart {
            on: true,
            args: args("--ip 192.0.2.7 --channels 1")
        })
    );
    assert_eq!(
        parse(&args("--autostart off")),
        Ok(Command::Autostart {
            on: false,
            args: vec![]
        })
    );
    assert_eq!(error("--autostart"), "--autostart is on or off");
    assert_eq!(error("--autostart yes"), "--autostart is on or off");
    assert_eq!(
        error("--autostart on --channels 0"),
        "--channels is 1 to 64, not 0"
    );
    assert_eq!(
        error("--autostart on --list-outputs"),
        "--autostart takes run options only"
    );
    assert_eq!(
        error("--autostart on --help"),
        "--autostart takes run options only"
    );
}

#[test]
fn mono_is_read_and_kept_for_the_logon_start() {
    assert!(run("--mono --rate 96000").mono);
    assert!(!run("--rate 96000").mono);
    let args: Vec<String> = ["--mono", "--autostart", "on"].map(String::from).to_vec();
    match parse(&args).unwrap() {
        Command::Autostart { on, args } => {
            assert!(on);
            assert_eq!(args, ["--mono"]);
        }
        other => panic!("{other:?}"),
    }
}
