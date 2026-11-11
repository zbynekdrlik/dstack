// SPDX-License-Identifier: MIT OR Apache-2.0
#![cfg_attr(windows, windows_subsystem = "windows")]

//! `dtray`: the PC's tray icon for its dstack devices (Windows and Linux),
//! see the library. `dtray --print` runs one discovery pass of
//! [`PRINT_WAIT`] and writes the tooltip and the lines to stdout (every
//! platform: a check without a desktop session; the Linux binary still
//! loads GTK's libraries), the expected devices
//! ([`dtray::expected`]) included. `dtray --autostart on|off` sets or
//! removes the tray's start at the user's logon (the Run value on Windows,
//! the XDG autostart entry on Linux; installers call it).
//! `dtray --set-latency <device> <ms>` runs the same discovery pass, then
//! the tray menu's latency write for the local device of that name
//! (case-insensitive) and value (any in ms inside the range the device
//! states, rounded to whole µs, such as `0.333` or `2`; dstack #213), and
//! prints the latency it reads back
//! ([`dtray::set`]; every platform). Exit codes: 0 done (for
//! `--set-latency`: the latency reads back as asked, or already was), 1 a
//! failure (on Linux also: no desktop to show the tray on; for
//! `--set-latency`: no such device, its settings unreadable, no value or
//! one outside its range, the write refused or a different read-back), 2 a
//! usage error or the tray or autostart on a platform without one. On
//! Windows (a GUI-subsystem program) every mode but the tray attaches to
//! the console of the shell that started it ([`dtray::wants_console`]).
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
use std::time::Duration;

use dstack::arc::control;
use dtray::Mode;
use dtray::discover::Scanner;
use dtray::expected;
use dtray::model::{self, Tracker};
use dtray::switches;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(windows, target_os = "linux"))]
mod tray;
#[cfg(windows)]
mod win;

/// How long `--print` lets discovery find the devices.
const PRINT_WAIT: Duration = Duration::from_secs(3);
/// How long `--print` lets the apps' status hooks answer after its first
/// read: a device calls its hook on its own thread once a status is read,
/// and the read before that has no app part yet (dstack #104 step 5).
const APP_WAIT: Duration = Duration::from_millis(500);

fn main() -> ExitCode {
    let parsed = dtray::mode_of(std::env::args().skip(1));
    #[cfg(windows)]
    if dtray::wants_console(parsed.as_ref().ok()) {
        win::attach_console();
    }
    let mode = match parsed {
        Ok(m) => m,
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };
    match mode {
        Mode::Print => print(),
        Mode::Tray => tray(),
        Mode::Autostart(on) => autostart(on),
        Mode::SetLatency { device, ms } => set_latency(&device, &ms),
        Mode::Switch { label, source } => switch(&label, &source),
        Mode::Version => {
            println!("{}", dtray::version_line());
            ExitCode::SUCCESS
        }
    }
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
    println!("{}", dtray::version_line());
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
/// named `device` ([`dtray::set::latency()`], through the ARC as the menu
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
    let outcome = dtray::set::latency(
        &scanner.devices(),
        device,
        ms,
        |arc| dtray::arc::call(arc, |c| control::settings(c).map_err(|e| e.to_string())),
        |arc, ns| {
            dtray::arc::call(arc, |c| {
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
