// SPDX-License-Identifier: MIT OR Apache-2.0

//! dtray's binary from the outside (dstack #213): a command-line mode's
//! lines and exit code reach the program that started it, through the
//! pipes it gave, on every platform. On Windows dtray.exe is a
//! GUI-subsystem program that attaches to its parent's console in these
//! modes; the pipes must still win. Neither case runs a discovery pass, so
//! no port is bound.

use std::process::{Command, Output};

fn dtray(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dtray"))
        .args(args)
        .output()
        .expect("dtray runs")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn the_version_answers_on_stdout() {
    let out = dtray(&["--version"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        text(&out.stdout).trim(),
        format!("dtray {}", env!("CARGO_PKG_VERSION"))
    );
    assert!(out.stderr.is_empty(), "{out:?}");
}

#[test]
fn a_usage_error_exits_2_with_the_usage_on_stderr() {
    let out = dtray(&["--set-latency", "dev"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert_eq!(text(&out.stderr).trim(), dtray::USAGE);
    assert!(out.stdout.is_empty(), "{out:?}");
}
