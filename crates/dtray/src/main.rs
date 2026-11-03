// SPDX-License-Identifier: MIT OR Apache-2.0
#![cfg_attr(windows, windows_subsystem = "windows")]

//! `dtray`: the PC's tray icon for its dstack devices (Windows), see the
//! library. `dtray --print` runs one discovery pass of [`PRINT_WAIT`] and
//! writes the tooltip and the lines to stdout (every platform: a check
//! without a desktop), the expected devices ([`dtray::expected`]) included.
//! `dtray --autostart on|off` sets or removes the tray's start at the
//! user's logon (Windows; installers call it). Exit codes: 0 done, 1 a
//! failure, 2 a usage error or the tray or autostart on a platform without
//! one.

use std::process::ExitCode;
use std::time::Duration;

use dtray::Mode;
use dtray::discover::Scanner;
use dtray::expected;
use dtray::model::{self, Tracker};

#[cfg(windows)]
mod win;

/// How long `--print` lets discovery find the devices.
const PRINT_WAIT: Duration = Duration::from_secs(3);

fn main() -> ExitCode {
    let mode = match dtray::mode_of(std::env::args().skip(1)) {
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
    }
}

/// One discovery pass, the view on stdout; 1 when discovery did not start.
fn print() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    let mut scanner = Scanner::new();
    let failed = scanner.refresh();
    std::thread::sleep(PRINT_WAIT);
    let expected = expected::expected(&expected::devices_dir());
    let view = Tracker::default().update_with(&scanner.readings(), &expected);
    print!("{}", model::print_text(&view));
    for f in &failed {
        eprintln!("dtray: {f}");
    }
    if failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
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

#[cfg(not(windows))]
fn tray() -> ExitCode {
    eprintln!("the tray runs on Windows; use --print");
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

#[cfg(not(windows))]
fn autostart(_on: bool) -> ExitCode {
    eprintln!("--autostart is for Windows; {}", dtray::USAGE);
    ExitCode::from(2)
}
