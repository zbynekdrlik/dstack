// SPDX-License-Identifier: MIT OR Apache-2.0

//! The command-line modes, one implementation for both of dtray's programs
//! (dstack #218): `dtray` (the tray, a GUI-subsystem program on Windows)
//! and `dtray-cli` (a console program on every platform, for scripts and
//! automation: any shell waits for it and reads its lines and exit code,
//! PowerShell's `&` too). Each program parses its arguments with
//! [`crate::mode_of`] and calls [`run`] with its own usage line, its own
//! tray and its own autostart.
//!
//! - `--print`: one discovery pass of [`PRINT_WAIT`], the tooltip and the
//!   lines on stdout (the expected devices and each switch's source
//!   included); 1 when discovery did not start or the switch file was
//!   refused.
//! - `--version`: [`crate::version_line`] on stdout.
//! - `--set-latency <device> <ms>`: the same discovery pass, then the tray
//!   menu's latency write ([`crate::set::latency()`]).
//! - `--switch <label> <source>`: the same discovery pass, then the tray
//!   menu's choice of that source ([`switches::set_live`]).
//! - `--dasio-apply <instance> <file>`: the settings window's elevated
//!   helper (dstack #246, [`dasio::apply_staged`]): the staged config
//!   written as that dasio instance's, its service restarted. The window
//!   starts it with Windows' "run as administrator"; it is not for scripts.
//!
//! Exit codes: 0 done, 1 a failure, 2 a usage error.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use dstack::arc::control;

use crate::Mode;
use crate::dasio;
use crate::discover::Scanner;
use crate::expected;
use crate::model::{self, Tracker};
use crate::switches;

/// How long `--print`, `--set-latency` and `--switch` let discovery find
/// the devices.
pub const PRINT_WAIT: Duration = Duration::from_secs(3);
/// How long `--print` lets the apps' status hooks answer after its first
/// read: a device calls its hook on its own thread once a status is read,
/// and the read before that has no app part yet (dstack #104 step 5).
pub const APP_WAIT: Duration = Duration::from_millis(500);

/// Why `--settings` does not run here: the window is the tray program's
/// on Windows (`dtray.exe --settings`).
pub const SETTINGS_ELSEWHERE: &str =
    "the dasio settings window opens with `dtray --settings` on Windows (or from the tray's menu)";

/// `dtray-cli`'s usage line: every mode needs an argument (it runs no
/// tray).
pub const CLI_USAGE: &str = "usage: dtray-cli --print | --version | --autostart on|off | --set-latency <device> <ms> | --switch <label> <source> | --dasio-apply <instance> <file>";

/// Runs `mode` (`None`: the arguments were a usage error, `usage` goes to
/// stderr and the exit code is 2). The tray and the autostart are the
/// program's own (`tray`, `autostart`): dtray runs its tray and writes its
/// own start at logon, dtray-cli refuses the tray and has the tray program
/// write the autostart ([`tray_program`]). Every other mode is run here,
/// the same for both programs.
pub fn run(
    mode: Option<Mode>,
    usage: &str,
    tray: impl FnOnce() -> ExitCode,
    autostart: impl FnOnce(bool) -> ExitCode,
) -> ExitCode {
    match mode {
        None => usage_error(usage),
        Some(Mode::Tray) => tray(),
        Some(Mode::Autostart(on)) => autostart(on),
        Some(Mode::Version) => {
            println!("{}", crate::version_line());
            ExitCode::SUCCESS
        }
        Some(Mode::Print) => print(),
        Some(Mode::SetLatency { device, ms }) => set_latency(&device, &ms),
        Some(Mode::Switch { label, source }) => switch(&label, &source),
        Some(Mode::DasioApply { instance, file }) => dasio_apply(&instance, Path::new(&file)),
        Some(Mode::Settings) => {
            eprintln!("dtray: {SETTINGS_ELSEWHERE}");
            ExitCode::from(2)
        }
    }
}

/// The settings window's elevated helper: [`dasio::apply_staged`] on
/// this PC's dasio folder, its verdict printed and left in the instance's
/// folder for the window ([`dasio::result_path`]; a verdict that cannot be
/// left there is said on stderr).
fn dasio_apply(instance: &str, file: &Path) -> ExitCode {
    let root = dasio::root();
    let result = dasio::instance_arg(instance)
        .ok()
        .map(|n| dasio::result_path(&root, n));
    if let Some(r) = &result {
        let _ = std::fs::remove_file(r);
    }
    let outcome = dasio::apply_staged(&root, instance, file, dasio::service::restart);
    if let Some(r) = &result
        && let Err(e) = std::fs::write(r, dasio::result_text(&outcome))
    {
        eprintln!("dtray: the verdict for the window ({}): {e}", r.display());
    }
    verdict(outcome)
}

/// A mode's outcome: its line on stdout and 0, or its reason on stderr
/// and 1.
pub fn verdict(outcome: Result<String, String>) -> ExitCode {
    match outcome {
        Ok(line) => {
            println!("dtray: {line}");
            ExitCode::SUCCESS
        }
        Err(reason) => {
            eprintln!("dtray: {reason}");
            ExitCode::FAILURE
        }
    }
}

/// `usage` on stderr, exit code 2.
pub fn usage_error(usage: &str) -> ExitCode {
    eprintln!("{usage}");
    ExitCode::from(2)
}

/// The tray program beside the program at `exe`: `dtray` (`dtray.exe` on
/// Windows) in the same folder. dtray-cli's `--autostart` runs it, so the
/// start at logon names the tray, never dtray-cli.
pub fn tray_program(exe: &Path) -> PathBuf {
    exe.with_file_name(format!("dtray{}", std::env::consts::EXE_SUFFIX))
}

/// The tray program's arguments for `--autostart on` (`on`) or `off`.
pub fn autostart_args(on: bool) -> [&'static str; 2] {
    ["--autostart", if on { "on" } else { "off" }]
}

/// The log on stderr, `RUST_LOG` as the devices read it (warnings by
/// default).
fn init_log() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
}

/// One discovery pass, the view on stdout; 1 when discovery did not start.
fn print() -> ExitCode {
    init_log();
    let mut scanner = Scanner::new();
    let failed = scanner.refresh();
    std::thread::sleep(PRINT_WAIT);
    let _first = scanner.readings();
    std::thread::sleep(APP_WAIT);
    let expected = expected::expected(&expected::devices_dir());
    let view = Tracker::default().update_with(&scanner.readings(), &expected);
    println!("{}", crate::version_line());
    print!("{}", model::print_text(&view));
    let mut ok = failed.is_empty();
    match switches::load(&switches::path()) {
        Ok(all) => {
            for sw in &all {
                let now = switches::current(sw, &switches::read_live(&scanner, sw));
                println!("{}", switches::line(sw, &now));
            }
        }
        Err(e) => {
            eprintln!("dtray: the switch file: {e}");
            ok = false;
        }
    }
    for f in &failed {
        eprintln!("dtray: {f}");
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// One discovery pass, then the menu's latency write for the local device
/// named `device` ([`crate::set::latency()`], through the ARC as the menu
/// writes): one line on stdout and 0 when the latency reads back as asked
/// (or already was), one line on stderr and 1 otherwise.
fn set_latency(device: &str, ms: &str) -> ExitCode {
    init_log();
    let mut scanner = Scanner::new();
    let failed = scanner.refresh();
    std::thread::sleep(PRINT_WAIT);
    for f in &failed {
        eprintln!("dtray: {f}");
    }
    let outcome = crate::set::latency(
        &scanner.devices(),
        device,
        ms,
        |arc| crate::arc::call(arc, |c| control::settings(c).map_err(|e| e.to_string())),
        |arc, ns| {
            crate::arc::call(arc, |c| {
                control::set_latency(c, ns).map_err(|e| e.to_string())
            })
        },
    );
    match outcome {
        Ok(line) => {
            println!("dtray: {line}");
            ExitCode::SUCCESS
        }
        Err(reason) => {
            eprintln!("dtray: {reason}");
            ExitCode::FAILURE
        }
    }
}

/// One discovery pass, then the menu's choice of the source labelled
/// `source` on the switch labelled `label` ([`switches::set_live`]): one
/// line on stdout and 0 when it reads back on that source (or already
/// was), one line on stderr and 1 otherwise.
fn switch(label: &str, source: &str) -> ExitCode {
    init_log();
    let outcome = switches::load(&switches::path()).and_then(|all| {
        let sw = switches::find(&all, label)?;
        let wanted = switches::find_source(sw, source)?;
        let mut scanner = Scanner::new();
        for f in scanner.refresh() {
            eprintln!("dtray: {f}");
        }
        std::thread::sleep(PRINT_WAIT);
        switches::set_live(&scanner, sw, wanted)
    });
    match outcome {
        Ok(line) => {
            println!("dtray: {line}");
            ExitCode::SUCCESS
        }
        Err(reason) => {
            eprintln!("dtray: {reason}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests;
