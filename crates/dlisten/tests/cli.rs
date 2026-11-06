// SPDX-License-Identifier: MIT OR Apache-2.0

//! dlisten's binary from the outside (dstack #139): what a user types gets
//! the answer it should, with no sound card (CI has none). A start whose
//! output cannot be found stops before the Dante device starts.

use std::process::{Command, Output};

fn dlisten(args: &[&str], env: &[(&str, &std::path::Path)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_dlisten"));
    c.args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().expect("dlisten runs")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn help_and_version_answer_on_stdout() {
    let out = dlisten(&["--help"], &[]);
    assert!(out.status.success());
    assert!(
        text(&out.stdout).starts_with("usage: dlisten [options]"),
        "{out:?}"
    );
    let out = dlisten(&["--version"], &[]);
    assert!(out.status.success());
    assert_eq!(
        text(&out.stdout).trim(),
        format!("dlisten {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn a_usage_error_exits_2_with_the_reason_and_the_usage() {
    let out = dlisten(&["--channels", "0"], &[]);
    assert_eq!(out.status.code(), Some(2));
    let err = text(&out.stderr);
    assert!(
        err.starts_with("dlisten: --channels is 1 to 64, not 0"),
        "{err}"
    );
    assert!(err.contains("usage: dlisten"), "{err}");
}

/// A failure shows a message box on Windows (no console from Explorer),
/// which a test cannot answer: these run on Linux.
#[cfg(target_os = "linux")]
mod linux {
    use super::*;

    #[test]
    fn listing_outputs_never_panics_without_a_sound_card() {
        let out = dlisten(&["--list-outputs"], &[]);
        match out.status.code() {
            Some(0) => assert!(!out.stdout.is_empty(), "{out:?}"),
            Some(1) => assert!(text(&out.stderr).contains("dlisten: "), "{out:?}"),
            other => panic!("exit {other:?}: {out:?}"),
        }
    }

    #[test]
    fn a_start_whose_output_is_not_there_stops_before_the_device() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("t-dlisten.toml");
        let out = dlisten(
            &[
                "--output",
                "no-such-output-xyzzy",
                "--state",
                state.to_str().unwrap(),
                "--name",
                "t-dlisten",
            ],
            &[],
        );
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        let err = text(&out.stderr);
        assert!(
            err.contains("no output's name holds \"no-such-output-xyzzy\"")
                || err.contains("no audio output device found")
                || err.contains("lists no outputs"),
            "{err}"
        );
        assert!(!err.contains("the Dante device runs"), "{err}");
        // The log file sits next to the state file and holds the failure.
        let log = std::fs::read_to_string(dir.path().join("t-dlisten.log")).unwrap();
        assert!(log.contains("dlisten: the log"), "{log}");
    }

    #[test]
    fn autostart_writes_and_removes_the_entry_with_the_options() {
        let dir = tempfile::tempdir().unwrap();
        let env = [("XDG_CONFIG_HOME", dir.path())];
        let out = dlisten(&["--autostart", "on", "--channels", "1"], &env);
        assert!(out.status.success(), "{out:?}");
        assert_eq!(text(&out.stdout).trim(), "dlisten: start at logon on");
        let entry = dir.path().join("autostart").join("dlisten.desktop");
        let written = std::fs::read_to_string(&entry).unwrap();
        assert!(written.contains("\" \"--channels\" \"1\"\n"), "{written}");
        let out = dlisten(&["--autostart", "off"], &env);
        assert!(out.status.success(), "{out:?}");
        assert!(!entry.exists());
        // Off again: nothing there is done too.
        assert!(dlisten(&["--autostart", "off"], &env).status.success());
    }
}
