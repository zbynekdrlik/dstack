// SPDX-License-Identifier: MIT OR Apache-2.0
#![cfg_attr(windows, windows_subsystem = "windows")]

//! `dsend`: sends a PC's sound into Dante (dstack #164), see the library.
//! It has no window and no tray of its own: dtray shows it, and its Output
//! menu sends `app output <n>`. The binary is glue: the command line
//! ([`dsend::cli`]), and on Windows the run (`run`), the session
//! (`session`: the capture and the dlink device), the log (stderr and `<state file>.log`) and Ctrl+C. On
//! Windows it is a GUI-subsystem program: its output goes to the console
//! it was started from, if any. Exit codes: 0 done, 1 a failure (not
//! Windows, no output device, the device did not start), 2 a usage error.

use std::process::ExitCode;

use dsend::cli::{self, Command};

// The capture and the session are Windows only (WASAPI loopback): on
// another OS a run or a list stops at once with the reason
// (`cli::capture_check`), and they are not built.
#[cfg(windows)]
mod audio;
#[cfg(windows)]
mod run;
#[cfg(windows)]
mod session;

#[cfg(windows)]
use run::{list, run};

/// `--list-outputs` off Windows: no output dsend could capture.
#[cfg(not(windows))]
fn list() -> Result<(), String> {
    cli::capture_check(false)
}

/// A run off Windows: the reason it cannot capture.
#[cfg(not(windows))]
fn run(_: &cli::Options) -> Result<(), String> {
    cli::capture_check(false)
}

fn main() -> ExitCode {
    #[cfg(windows)]
    let (attached, has_stderr) = dstack_endpoint::win::attach_console();
    #[cfg(not(windows))]
    let (attached, has_stderr) = (true, true);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match cli::parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("dsend: {e}\n\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    let is_run = matches!(command, Command::Run(_));
    let done = match command {
        Command::Help => {
            println!("{}", cli::USAGE);
            Ok(())
        }
        Command::Version => {
            println!("dsend {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::ListOutputs => list(),
        Command::Run(o) => run(&o),
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("dsend: {e}");
            #[cfg(windows)]
            let logged = run::log_open();
            #[cfg(not(windows))]
            let logged = false;
            fail(&e, cli::shows_box(is_run, logged, attached, has_stderr));
            ExitCode::FAILURE
        }
    }
}

/// A failure the user must see: stderr (and the log, `tracing::error!`
/// above), and a message box when [`cli::shows_box`] says so: never for a
/// run with its log open, which is started headless (a logon task, dstack
/// #225); else when nothing reads stderr (a start from Explorer).
fn fail(text: &str, boxed: bool) {
    eprintln!("dsend: {text}");
    #[cfg(windows)]
    if boxed {
        dstack_endpoint::win::message("dsend", &format!("dsend: {text}"));
    }
    #[cfg(not(windows))]
    let _ = boxed;
}
