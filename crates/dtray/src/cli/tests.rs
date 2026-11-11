// SPDX-License-Identifier: MIT OR Apache-2.0

use std::path::Path;
use std::process::ExitCode;

use super::*;

fn no_tray() -> ExitCode {
    panic!("the tray is not this mode's")
}

fn no_autostart(_on: bool) -> ExitCode {
    panic!("the autostart is not this mode's")
}

#[test]
fn a_usage_error_exits_2_and_runs_neither_tray_nor_autostart() {
    assert_eq!(
        run(None, CLI_USAGE, no_tray, no_autostart),
        ExitCode::from(2)
    );
    assert_eq!(usage_error(CLI_USAGE), ExitCode::from(2));
}

#[test]
fn the_tray_is_the_program_s_own() {
    assert_eq!(
        run(
            Some(Mode::Tray),
            CLI_USAGE,
            || ExitCode::from(7),
            no_autostart
        ),
        ExitCode::from(7)
    );
}

#[test]
fn the_autostart_is_the_program_s_own_with_its_choice() {
    for on in [true, false] {
        let code = run(Some(Mode::Autostart(on)), CLI_USAGE, no_tray, |got| {
            assert_eq!(got, on);
            ExitCode::from(5)
        });
        assert_eq!(code, ExitCode::from(5));
    }
}

#[test]
fn the_version_runs_here_and_exits_0() {
    assert_eq!(
        run(Some(Mode::Version), CLI_USAGE, no_tray, no_autostart),
        ExitCode::SUCCESS
    );
}

#[test]
fn the_cli_usage_needs_a_mode_and_lists_every_one() {
    assert!(CLI_USAGE.starts_with("usage: dtray-cli "));
    for mode in [
        "--print",
        "--version",
        "--autostart on|off",
        "--set-latency <device> <ms>",
        "--switch <label> <source>",
    ] {
        assert!(CLI_USAGE.contains(mode), "{mode}");
    }
    assert!(!CLI_USAGE.contains('['), "no mode is optional");
}

#[test]
fn the_tray_program_sits_beside_the_cli() {
    let suffix = std::env::consts::EXE_SUFFIX;
    let cli = Path::new("/opt/dstack").join(format!("dtray-cli{suffix}"));
    assert_eq!(
        tray_program(&cli),
        Path::new("/opt/dstack").join(format!("dtray{suffix}"))
    );
}

#[test]
fn the_autostart_arguments_are_dtray_s_own_mode() {
    for on in [true, false] {
        let args = autostart_args(on).map(String::from);
        assert_eq!(crate::mode_of(args), Ok(Mode::Autostart(on)));
    }
}
