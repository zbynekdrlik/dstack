// SPDX-License-Identifier: MIT OR Apache-2.0

use std::path::Path;

use super::{Mode, USAGE, mode_of, run_value, version_line, wants_console};

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

#[test]
fn no_argument_is_the_tray_and_print_prints() {
    assert_eq!(mode_of(args(&[])), Ok(Mode::Tray));
    assert_eq!(mode_of(args(&["--print"])), Ok(Mode::Print));
    assert_eq!(mode_of(args(&["--version"])), Ok(Mode::Version));
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
fn set_latency_takes_a_device_and_a_value() {
    assert_eq!(
        mode_of(args(&["--set-latency", "Test Dev", "0.5"])),
        Ok(Mode::SetLatency {
            device: "Test Dev".to_string(),
            ms: "0.5".to_string()
        })
    );
    // The value is checked against the device's offer, not here.
    assert_eq!(
        mode_of(args(&["--set-latency", "x", "3"])),
        Ok(Mode::SetLatency {
            device: "x".to_string(),
            ms: "3".to_string()
        })
    );
    assert!(USAGE.contains("--set-latency <device> <ms>"));
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
        &["--set-latency"],
        &["--set-latency", "x"],
        &["--set-latency", "x", "1", "2"],
        &["--print", "--set-latency", "x", "1"],
        &["--switch"],
        &["--switch", "Solo"],
        &["--switch", "Solo", "band", "master"],
        &["--print", "--switch", "Solo", "band"],
    ] {
        assert_eq!(mode_of(args(a)), Err(USAGE), "{a:?}");
    }
}

#[test]
fn switch_takes_a_label_and_a_source() {
    assert_eq!(
        mode_of(args(&["--switch", "Solo", "master"])),
        Ok(Mode::Switch {
            label: "Solo".to_string(),
            source: "master".to_string()
        })
    );
    assert!(USAGE.contains("--switch <label> <source>"));
}

#[test]
fn the_run_value_is_the_exe_path_quoted() {
    assert_eq!(
        run_value(Path::new(r"C:\Program Files\dstack\dtray.exe")).as_deref(),
        Some(r#""C:\Program Files\dstack\dtray.exe""#)
    );
    assert_eq!(run_value(Path::new("dtray")).as_deref(), Some("\"dtray\""));
}

#[cfg(unix)]
#[test]
fn a_path_that_is_no_unicode_has_no_run_value() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    assert_eq!(
        run_value(Path::new(OsStr::from_bytes(b"/opt/\xff/dtray"))),
        None
    );
}

/// The owner (9.11.2026): every PC's dtray can be checked for its version.
#[test]
fn the_version_line_names_the_dstack_release() {
    assert_eq!(
        version_line(),
        format!("dtray {}", env!("CARGO_PKG_VERSION"))
    );
    assert!(USAGE.contains("--version"));
}

#[test]
fn every_command_line_mode_takes_the_shell_s_console_the_tray_none() {
    // dstack #213: on Windows dtray.exe is a GUI-subsystem program; its
    // command-line modes attach to the console of the shell that started
    // them, so their lines reach it. The tray opens no console.
    let wants = |a: &[&str]| wants_console(mode_of(args(a)).as_ref().ok());
    assert!(!wants(&[]));
    assert!(wants(&["--print"]));
    assert!(wants(&["--version"]));
    assert!(wants(&["--autostart", "on"]));
    assert!(wants(&["--autostart", "off"]));
    assert!(wants(&["--set-latency", "dev", "0.333"]));
    assert!(wants(&["--switch", "Solo", "band"]));
    // A usage error is printed too.
    assert!(wants(&["--bogus"]));
    assert!(!wants_console(Some(&Mode::Tray)));
    assert!(wants_console(None));
}
