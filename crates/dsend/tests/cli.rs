// SPDX-License-Identifier: MIT OR Apache-2.0

//! dsend's binary from the outside (dstack #164): what a user types gets
//! the answer it should, with no sound card (CI has none).

use std::process::{Command, Output};

fn dsend(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dsend"))
        .args(args)
        .output()
        .expect("dsend runs")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn help_and_version_answer_on_stdout() {
    let out = dsend(&["--help"]);
    assert!(out.status.success());
    assert!(
        text(&out.stdout).starts_with("usage: dsend [options]"),
        "{out:?}"
    );
    let out = dsend(&["--version"]);
    assert!(out.status.success());
    assert_eq!(
        text(&out.stdout).trim(),
        format!("dsend {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn a_usage_error_exits_2_with_the_reason_and_the_usage() {
    let out = dsend(&["--rate", "0"]);
    assert_eq!(out.status.code(), Some(2));
    let err = text(&out.stderr);
    assert!(
        err.starts_with("dsend: --rate needs a rate over 0 Hz"),
        "{err}"
    );
    assert!(err.contains("usage: dsend"), "{err}");
}

/// Off Windows a run stops at once with the reason, before any device or
/// stream starts (no state folder, no Dante device).
#[cfg(not(windows))]
#[test]
fn a_run_off_windows_exits_1_saying_it_needs_windows() {
    let dir = std::env::temp_dir().join(format!("dsend-cli-{}", std::process::id()));
    let state = dir.join("x.toml");
    let out = dsend(&["--state", state.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let err = text(&out.stderr);
    assert!(
        err.contains("dsend captures an output's sound by WASAPI loopback"),
        "{err}"
    );
    assert!(!dir.exists(), "nothing was started");
}
