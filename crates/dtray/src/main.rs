// SPDX-License-Identifier: MIT OR Apache-2.0
#![cfg_attr(windows, windows_subsystem = "windows")]

//! `dtray`: the PC's tray icon for its dstack devices (Windows and Linux),
//! see the library. `dtray --print` runs one discovery pass of
//! [`dtray::cli::PRINT_WAIT`] and writes the tooltip and the lines to stdout (every
//! platform: a check without a desktop session; the Linux binary still
//! loads GTK's libraries), the expected devices
//! ([`dtray::expected`]) included. `dtray --autostart on|off` sets or
//! removes the tray's start at the user's logon (the Run value on Windows,
//! the XDG autostart entry on Linux; installers call it).
//! `dtray --set-latency <device> <ms>` runs the same discovery pass, then
//! the tray menu's latency write for the local device of that name
//! (case-insensitive) and value (any in ms inside the range the device
//! states, to whole ns, such as `0.333334` or `2`; dstack #213), and
//! prints the latency it reads back
//! ([`dtray::set`]; every platform). Exit codes: 0 done (for
//! `--set-latency`: the latency reads back as asked, or already was), 1 a
//! failure (on Linux also: no desktop to show the tray on; for
//! `--set-latency`: no such device, its settings unreadable, no value or
//! one outside its range, the write refused or a different read-back), 2 a
//! usage error or the tray or autostart on a platform without one. On
//! Windows (a GUI-subsystem program) every mode but the tray attaches to
//! the console of the shell that started it ([`dtray::wants_console`]):
//! an interactive console shows the lines, but PowerShell's `&` does not
//! wait for a GUI-subsystem program unless it captures the output, so in a
//! shell whose output is a pipe (a remote shell, CI) the lines and the exit
//! code are lost. Scripts and automation call `dtray-cli`, the console
//! program for the same modes (dstack #218).
//! Both programs run the modes by the same code ([`dtray::cli::run`]).
//!
//! `dtray --settings` (Windows) opens the dasio settings window alone, as
//! the tray menu's "dasio settings…" does (dstack #246: one tab per dasio
//! instance; [`dtray::dasio`]).
//!
//! `dtray --switch <label> <source>` runs the same discovery pass, then
//! the tray menu's choice of that source on that switch of the PC's
//! switch file ([`dtray::switches`]: the receiver's listed RX channels
//! subscribed, then read back), and prints the verdict: 0 when it reads
//! back on that source (or already was), 1 otherwise (no such switch or
//! source, the file refused, the receiver not found or not read, a
//! subscription refused, a different read-back). `--print` also prints
//! each switch's current source after the devices (a refused switch file
//! on stderr, and 1).

use std::process::ExitCode;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod settings_win;
#[cfg(any(windows, target_os = "linux"))]
mod tray;
#[cfg(windows)]
mod win;

fn main() -> ExitCode {
    let parsed = dtray::mode_of(std::env::args().skip(1));
    #[cfg(windows)]
    if dtray::wants_console(parsed.as_ref().ok()) {
        win::attach_console();
    }
    #[cfg(windows)]
    if parsed == Ok(dtray::Mode::Settings) {
        return settings();
    }
    dtray::cli::run(parsed.ok(), dtray::USAGE, tray, autostart)
}

#[cfg(windows)]
fn tray() -> ExitCode {
    match win::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            win::message(&format!("dtray: {e}"));
            ExitCode::FAILURE
        }
    }
}

/// `dtray --settings`: the dasio settings window alone (dstack #246); an
/// error in a box, 1.
#[cfg(windows)]
fn settings() -> ExitCode {
    match settings_win::run_standalone() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            win::message(&format!("dtray: {e}"));
            ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "linux")]
fn tray() -> ExitCode {
    match linux::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("dtray: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn tray() -> ExitCode {
    eprintln!("the tray runs on Windows and Linux; use --print");
    ExitCode::from(2)
}

/// Sets or removes the Run value; 1 when the registry refused (no box: an
/// installer runs it without a desktop to answer one).
#[cfg(windows)]
fn autostart(on: bool) -> ExitCode {
    match win::autostart(on) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("dtray: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Writes or removes the XDG autostart entry; 1 when that failed.
#[cfg(target_os = "linux")]
fn autostart(on: bool) -> ExitCode {
    match linux::autostart(on) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("dtray: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn autostart(_on: bool) -> ExitCode {
    eprintln!("--autostart is for Windows and Linux; {}", dtray::USAGE);
    ExitCode::from(2)
}
