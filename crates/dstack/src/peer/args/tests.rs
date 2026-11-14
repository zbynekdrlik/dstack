// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

fn args(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_string).collect()
}

fn run(line: &str) -> PeerArgs {
    match parse(&args(line)) {
        Ok(Parsed::Run(a)) => *a,
        other => panic!("{line}: {other:?}"),
    }
}

fn refused(line: &str) -> String {
    match parse(&args(line)) {
        Err(why) => why,
        other => panic!("{line}: {other:?}"),
    }
}

#[test]
fn the_design_s_command_line_parses() {
    let a = run("--iface dante0 --name ci-peer --tx 2 --tone-hz 440,1000 \
                 --rx-from obs-test:01,02 --test-clock-ppm -6 --status /tmp/p.json");
    assert_eq!(
        a,
        PeerArgs {
            place: Place::Iface("dante0".into()),
            name: "ci-peer".into(),
            tones: vec![440, 1000],
            rx_from: Some(("obs-test".into(), vec!["01".into(), "02".into()])),
            test_clock_ppm: Some(TestClockPpm(-6.0)),
            rate: 48_000,
            latency_ns: 10_000_000,
            ports: None,
            process_id: 0,
            state: PathBuf::from("/tmp/p.json.state.toml"),
            status: PathBuf::from("/tmp/p.json"),
        }
    );
}

#[test]
fn every_option_is_taken() {
    let a = run(
        "--ip 10.70.0.2 --name p --tone-hz 997 --rate 96000 --latency-ms 4.5 \
                 --ports 4441,8801,4456 --process-id 3 --state /s.toml --status /st.json",
    );
    assert_eq!(a.place, Place::Ip(Ipv4Addr::new(10, 70, 0, 2)));
    assert_eq!(
        (a.tones.clone(), a.rate, a.latency_ns),
        (vec![997], 96_000, 4_500_000)
    );
    assert_eq!((a.ports, a.process_id), (Some((4441, 8801, 4456)), 3));
    assert_eq!(a.state, PathBuf::from("/s.toml"));
    assert_eq!((a.rx_from, a.test_clock_ppm), (None, None));
}

#[test]
fn help_is_help() {
    assert_eq!(parse(&args("--help")), Ok(Parsed::Help));
    assert_eq!(parse(&args("--name x -h")), Ok(Parsed::Help));
}

#[test]
fn each_refusal_names_its_flag() {
    let base = "--ip 10.0.0.2 --name p --status /s";
    for (extra, why) in [
        ("--tone-hz", "--tone-hz: a value is missing"),
        ("--ip x", "--ip x: an IPv4 address"),
        ("--tx two", "--tx two: a count"),
        (
            "--tone-hz 440,abc",
            "--tone-hz 440,abc: whole Hz, comma separated",
        ),
        ("--tone-hz 0", "--tone-hz 0: whole Hz, comma separated"),
        ("--rx-from obs", "--rx-from obs: <device>:<channel,...>"),
        ("--rx-from :01", "--rx-from :01: <device>:<channel,...>"),
        ("--rx-from obs:", "--rx-from obs:: <device>:<channel,...>"),
        ("--test-clock-ppm six", "--test-clock-ppm six: a number"),
        (
            "--test-clock-ppm 1001",
            "test_clock_ppm: a number from -1000 to 1000",
        ),
        ("--rate fast", "--rate fast: Hz"),
        (
            "--latency-ms 0",
            "--latency-ms 0: ms, over 0 and up to 1000",
        ),
        (
            "--latency-ms 1000.5",
            "--latency-ms 1000.5: ms, over 0 and up to 1000",
        ),
        ("--ports 1,2", "--ports 1,2: <arc>,<cmc>,<flows>"),
        ("--ports 1,2,x", "--ports 1,2,x: <arc>,<cmc>,<flows>"),
        ("--process-id -1", "--process-id -1: a number"),
        ("--bogus 1", "--bogus: not a peer option"),
        (
            "--tx 3 --tone-hz 440,880",
            "--tx 3: --tone-hz gives 2 tones: one for every channel, or one each",
        ),
        (
            "--tx 1",
            "--tx 1: --tone-hz gives 0 tones: one for every channel, or one each",
        ),
        (
            "--tone-hz 24000",
            "--tone-hz: each under half the rate (48000 Hz)",
        ),
    ] {
        assert_eq!(refused(&format!("{base} {extra}")), why, "{extra}");
    }
    assert_eq!(
        refused("--name p --status /s"),
        "--iface or --ip is missing"
    );
    assert_eq!(refused("--ip 10.0.0.2 --status /s"), "--name is missing");
    assert_eq!(refused("--ip 10.0.0.2 --name p"), "--status is missing");
}

#[test]
fn the_tones_under_half_the_rate_and_their_count_agree() {
    let a = run("--ip 10.0.0.2 --name p --status /s --tx 1 --tone-hz 23999");
    assert_eq!(a.tones, [23_999]);
    let a = run("--ip 10.0.0.2 --name p --status /s --tx 0");
    assert!(a.tones.is_empty());
    // One tone for every channel.
    let a = run("--ip 10.0.0.2 --name p --status /s --tx 3 --tone-hz 997");
    assert_eq!(a.tones, [997, 997, 997]);
    let a = run("--ip 10.0.0.2 --name p --status /s --tx 2 --tone-hz 440,880");
    assert_eq!(a.tones, [440, 880]);
    let a = run("--ip 10.0.0.2 --name p --status /s --tone-hz 30000 --rate 96000");
    assert_eq!(a.tones, [30_000]);
}
