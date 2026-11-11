// SPDX-License-Identifier: MIT OR Apache-2.0

//! `dtray-cli` from the outside (dstack #218): the console program for
//! dtray's command-line modes. A capturing host (a script, a remote shell,
//! CI) reads its lines and exit code through the pipes it gave, and it
//! never starts a tray. On Windows it is a console-subsystem program, so
//! PowerShell's `&` waits for it and sets `$LASTEXITCODE`, which it does
//! not for the GUI-subsystem `dtray.exe`.

use std::process::{Command, Output};

fn dtray_cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dtray-cli"))
        .args(args)
        .output()
        .expect("dtray-cli runs")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn the_version_answers_on_stdout_with_0() {
    let out = dtray_cli(&["--version"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        text(&out.stdout).trim(),
        format!("dtray {}", env!("CARGO_PKG_VERSION"))
    );
    assert!(out.stderr.is_empty(), "{out:?}");
}

#[test]
fn no_arguments_is_a_usage_error_never_a_tray() {
    let out = dtray_cli(&[]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(
        text(&out.stderr).starts_with("usage: dtray-cli "),
        "{out:?}"
    );
    assert!(out.stdout.is_empty(), "{out:?}");
}

#[test]
fn a_bad_argument_is_a_usage_error() {
    let out = dtray_cli(&["--set-latency", "dev"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(
        text(&out.stderr).starts_with("usage: dtray-cli "),
        "{out:?}"
    );
    assert!(out.stdout.is_empty(), "{out:?}");
}

/// The case dasio met (#218): a refused `--set-latency` prints its reason
/// to the pipe and exits 1. One discovery pass (3 s, mDNS on this PC's
/// addresses: the only test here that touches the network); no device of
/// that name exists anywhere. A discovery error, if any, comes on stderr
/// before the refusal.
#[test]
fn a_refused_set_latency_prints_why_and_exits_1() {
    let name = "dtray-cli-test-no-such-device";
    let out = dtray_cli(&["--set-latency", name, "1"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let err = text(&out.stderr);
    assert!(
        err.contains(&format!("no dstack device named {name} on this PC")),
        "{out:?}"
    );
    assert!(out.stdout.is_empty(), "{out:?}");
}

/// `dtray-cli --autostart on` registers the tray program (`dtray` beside
/// it), never itself: a logon start of dtray-cli would only print the
/// usage. Linux only: on Windows the same call writes the runner user's
/// Run value, which a test must not leave behind; the Windows Run value is
/// written by the tray program's own code, which this delegation runs.
#[cfg(target_os = "linux")]
#[test]
fn autostart_registers_the_tray_program_beside_it() {
    let config = tempfile::tempdir().expect("a temporary folder");
    let run = |on: &str| {
        Command::new(env!("CARGO_BIN_EXE_dtray-cli"))
            .args(["--autostart", on])
            .env("XDG_CONFIG_HOME", config.path())
            .output()
            .expect("dtray-cli runs")
    };
    let entry = config.path().join("autostart").join("dtray.desktop");

    let out = run("on");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let tray = std::fs::canonicalize(env!("CARGO_BIN_EXE_dtray")).expect("dtray is built");
    let written = std::fs::read_to_string(&entry).expect("the entry is written");
    assert_eq!(
        Some(written),
        dtray::desktop::desktop_entry(&tray),
        "the entry starts the tray program"
    );

    let out = run("off");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(!entry.exists(), "the entry is removed");
}

/// Without the tray program beside it, `dtray-cli --autostart on` writes
/// nothing and exits 1 with the reason.
#[cfg(target_os = "linux")]
#[test]
fn autostart_without_the_tray_program_beside_it_exits_1() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let alone = dir.path().join("dtray-cli");
    std::fs::copy(env!("CARGO_BIN_EXE_dtray-cli"), &alone).expect("a copy of dtray-cli");
    let config = dir.path().join("config");
    let out = Command::new(&alone)
        .args(["--autostart", "on"])
        .env("XDG_CONFIG_HOME", &config)
        .output()
        .expect("dtray-cli runs");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(text(&out.stderr).contains("did not start"), "{out:?}");
    assert!(
        !config.join("autostart").join("dtray.desktop").exists(),
        "nothing is written"
    );
}

/// The PE header's Subsystem field of the program at `exe`: 2 for a GUI
/// program, 3 for a console program.
#[cfg(windows)]
fn subsystem(exe: &str) -> u16 {
    let bytes = std::fs::read(exe).expect("the program is readable");
    let u32_at = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
    let pe = u32_at(0x3C);
    assert_eq!(&bytes[pe..pe + 4], b"PE\0\0", "a PE file");
    // The optional header follows the 4-byte signature and the 20-byte
    // file header; its Subsystem field sits at offset 68 (PE32 and PE32+).
    let at = pe + 4 + 20 + 68;
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

#[cfg(windows)]
#[test]
fn dtray_cli_is_a_console_program_and_dtray_the_gui_tray() {
    assert_eq!(subsystem(env!("CARGO_BIN_EXE_dtray-cli")), 3, "console");
    assert_eq!(subsystem(env!("CARGO_BIN_EXE_dtray")), 2, "GUI");
}

/// PowerShell running `script` with its own output in pipes, as a remote
/// shell or CI runs it: its stdout and stderr.
#[cfg(windows)]
fn powershell(script: &str) -> Output {
    Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .expect("powershell runs")
}

/// The way dasio's automation calls it (#218): PowerShell's call operator,
/// the program's output not captured, in a PowerShell whose own output is
/// a pipe (the remote shell). PowerShell waits for a console program, its
/// lines reach the pipe and `$LASTEXITCODE` is set; it does not wait for
/// the GUI-subsystem dtray.exe, whose lines were lost and `$LASTEXITCODE`
/// left empty there.
#[cfg(windows)]
#[test]
fn powershell_call_operator_uncaptured_gives_the_lines_and_the_exit_code() {
    let exe = env!("CARGO_BIN_EXE_dtray-cli").replace('\'', "''");
    let out = powershell(&format!(
        "& '{exe}' --version; \"code=$LASTEXITCODE\"; \
         & '{exe}' --bogus; \"code=$LASTEXITCODE\"; exit 0"
    ));
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let stdout = text(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    let version = format!("dtray {}", env!("CARGO_PKG_VERSION"));
    assert_eq!(lines, [version.as_str(), "code=0", "code=2"], "{out:?}");
    // PowerShell 5.1 may pass a native program's stderr on as its own
    // error record ("<program> : usage: ..."), so the line is looked for,
    // not taken as the whole text.
    assert!(text(&out.stderr).contains("usage: dtray-cli "), "{out:?}");
}

/// PowerShell's call operator with the output captured into a variable,
/// the other way a script reads it: the lines and `$LASTEXITCODE`.
#[cfg(windows)]
#[test]
fn powershell_call_operator_captured_gives_the_lines_and_the_exit_code() {
    let exe = env!("CARGO_BIN_EXE_dtray-cli").replace('\'', "''");
    // Windows PowerShell 5.1 wraps each line a native program writes to
    // stderr in an ErrorRecord; "$_" is its text alone (Out-String would
    // add the record's position lines).
    let out = powershell(&format!(
        "$v = & '{exe}' --version; \"version=$v\"; \"code=$LASTEXITCODE\"; \
         $u = (& '{exe}' --bogus 2>&1 | ForEach-Object {{ \"$_\" }}) -join ' '; \
         \"usage=$u\"; \"code=$LASTEXITCODE\"; exit 0"
    ));
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let lines: Vec<String> = text(&out.stdout).lines().map(str::to_string).collect();
    assert_eq!(
        lines.first().map(String::as_str),
        Some(format!("version=dtray {}", env!("CARGO_PKG_VERSION")).as_str()),
        "{out:?}"
    );
    assert_eq!(lines.get(1).map(String::as_str), Some("code=0"), "{out:?}");
    assert!(
        lines
            .get(2)
            .is_some_and(|l| l.starts_with("usage=usage: dtray-cli ")),
        "{out:?}"
    );
    assert_eq!(lines.get(3).map(String::as_str), Some("code=2"), "{out:?}");
}
