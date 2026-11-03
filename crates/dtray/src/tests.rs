// SPDX-License-Identifier: MIT OR Apache-2.0

use std::path::Path;

use super::{Mode, USAGE, mode_of, run_value};

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

#[test]
fn no_argument_is_the_tray_and_print_prints() {
    assert_eq!(mode_of(args(&[])), Ok(Mode::Tray));
    assert_eq!(mode_of(args(&["--print"])), Ok(Mode::Print));
}

#[test]
fn autostart_takes_on_or_off() {
    assert_eq!(
        mode_of(args(&["--autostart", "on"])),
        Ok(Mode::Autostart(true))
    );
    assert_eq!(
        mode_of(args(&["--autostart", "off"])),
        Ok(Mode::Autostart(false))
    );
}

#[test]
fn anything_else_is_the_usage() {
    for a in [
        &["--prin"][..],
        &["print"],
        &["--print", "--print"],
        &["-h"],
        &["--autostart"],
        &["--autostart", "yes"],
        &["--autostart", "ON"],
        &["--autostart", "on", "off"],
        &["on"],
    ] {
        assert_eq!(mode_of(args(a)), Err(USAGE), "{a:?}");
    }
}

#[test]
fn the_run_value_is_the_exe_path_quoted() {
    assert_eq!(
        run_value(Path::new(r"C:\Program Files\dstack\dtray.exe")),
        r#""C:\Program Files\dstack\dtray.exe""#
    );
    assert_eq!(run_value(Path::new("dtray")), "\"dtray\"");
}
