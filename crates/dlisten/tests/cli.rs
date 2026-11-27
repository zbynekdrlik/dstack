// SPDX-License-Identifier: MIT OR Apache-2.0

//! dlisten's binary from the outside (dstack #139): what a user types gets
//! the answer it should, with no sound card (CI has none). A start whose
//! `--output` matches nothing waits for it, the Dante device running
//! (dstack #506).

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

    /// dstack #506: a start whose `--output` names no output there (the
    /// headphones out at boot) does not stop: the Dante device runs, no
    /// output plays, the log says it waits (WARN), and it never falls back
    /// to the default. Only a PC whose audio system lists nothing at all
    /// (a CI runner without ALSA devices) still stops, before the device;
    /// there the `follow` unit tests alone carry the proof (dev1 lists
    /// ALSA's `null` output, so it waits there).
    #[test]
    fn a_start_whose_named_output_is_not_there_waits_for_it() {
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("t-wait-dlisten.toml");
        let log_path = dir.path().join("t-wait-dlisten.log");
        let mut child = Command::new(env!("CARGO_BIN_EXE_dlisten"))
            .args([
                "--output",
                "no-such-output-xyzzy",
                "--ip",
                "127.0.0.1",
                "--state",
                state.to_str().unwrap(),
                "--name",
                "t-wait-dlisten",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("dlisten starts");
        let start = Instant::now();
        let waited = loop {
            if let Some(status) = child.try_wait().expect("the child is read") {
                let mut err = String::new();
                std::io::Read::read_to_string(child.stderr.as_mut().unwrap(), &mut err)
                    .expect("its stderr");
                assert_eq!(status.code(), Some(1), "{err}");
                assert!(err.contains("lists no outputs"), "{err}");
                break None;
            }
            let log = std::fs::read_to_string(&log_path).unwrap_or_default();
            if log.contains("the Dante device runs") && log.contains("no output holding") {
                break Some(log);
            }
            if start.elapsed() > Duration::from_secs(30) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("no wait in 30 s; the log:\n{log}");
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let _ = child.kill();
        let _ = child.wait();
        if let Some(log) = waited {
            assert!(
                log.contains(
                    "no output holding \"no-such-output-xyzzy\" is there (--output): playing \
                     nothing until it comes, never on another output"
                ),
                "{log}"
            );
            assert!(log.contains(" WARN "), "{log}");
            assert!(!log.contains("playing on the output"), "{log}");
        }
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
