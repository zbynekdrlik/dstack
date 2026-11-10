// SPDX-License-Identifier: MIT OR Apache-2.0
#![cfg_attr(windows, windows_subsystem = "windows")]

//! `dlisten`: plays Dante audio on a local output (dstack #139), see the
//! library. It has no window and no tray of its own: dtray shows it, and
//! its Output menu sends `app output <n>` (the owner's ROZHODNUTÉ of
//! 6.11.2026). The binary is glue: the command line ([`dlisten::cli`]), the
//! session (`session`: the dlink device and the output), the log (stderr
//! and `<state file>.log`), Ctrl+C, and `--autostart`. On Windows it is a
//! GUI-subsystem program: its output goes to the console it was started
//! from, if any. Exit codes: 0 done, 1 a failure (no output device, the
//! device did not start), 2 a usage error.

use std::fs::File;
use std::process::ExitCode;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use dlisten::cli::{self, Command, Options};
use dlisten::outputs;
use dstack_endpoint::host::{self, init_log};

use session::{Event, Session};

mod audio;
#[cfg(not(windows))]
mod os_linux;
mod session;
#[cfg(windows)]
mod win;

#[cfg(not(windows))]
use os_linux as os;
#[cfg(windows)]
use win as os;

/// How often the main thread wakes (dtray's choices come at once).
const WAKE: Duration = Duration::from_secs(1);
/// Every how many wakes the status is logged (10 s).
const LOG_EVERY: u32 = 10;
/// Every how many wakes the output devices are read again (5 s).
const OUTPUTS_EVERY: u32 = 5;

fn main() -> ExitCode {
    #[cfg(windows)]
    let (attached, has_stderr) = win::attach_console();
    #[cfg(not(windows))]
    let (attached, has_stderr) = (true, true);
    let boxed = dlisten::alert::shows_box(attached, has_stderr);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match cli::parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("dlisten: {e}\n\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    let done = match command {
        Command::Help => {
            println!("{}", cli::USAGE);
            Ok(())
        }
        Command::Version => {
            println!("dlisten {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::ListOutputs => list(),
        Command::Autostart { on, args } => autostart(on, &args),
        Command::Run(o) => run(&o),
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("dlisten: {e}");
            fail(&e, boxed);
            ExitCode::FAILURE
        }
    }
}

/// A failure the user must see: stderr, and a message box when nothing
/// reads stderr (`boxed`: a Windows start from Explorer or the Run value,
/// [`dlisten::alert::shows_box`]).
fn fail(text: &str, boxed: bool) {
    eprintln!("dlisten: {text}");
    #[cfg(windows)]
    if boxed {
        win::message(&format!("dlisten: {text}"));
    }
    #[cfg(not(windows))]
    let _ = boxed;
}

/// `--list-outputs`.
fn list() -> Result<(), String> {
    init_log(None);
    let (_, described) = audio::outputs()?;
    println!("{}", outputs::list_text(&described)?);
    Ok(())
}

/// `--autostart on|off`: the logon start runs with `args`.
fn autostart(on: bool, args: &[String]) -> Result<(), String> {
    init_log(None);
    os::set_autostart(on, args)?;
    if os::autostart_on() != on {
        return Err("the start at logon did not change".into());
    }
    println!("dlisten: start at logon {}", if on { "on" } else { "off" });
    Ok(())
}

fn run(o: &Options) -> Result<(), String> {
    let name = session::name_of(o);
    let state = session::state_of(o, &name);
    let log = state.with_extension("log");
    let file = log
        .parent()
        .and_then(|d| std::fs::create_dir_all(d).ok())
        .and_then(|()| File::create(&log).ok());
    let logged = file.is_some();
    init_log(file);
    if logged {
        tracing::info!(log = %log.display(), "dlisten: the log");
    }
    let (events, inbox) = mpsc::channel();
    let mut session = Session::start(o, events.clone())?;

    host::on_ctrl_c("dlisten", move || {
        let _ = events.send(Event::Stop);
    })?;

    let mut wakes: u32 = 0;
    let result = loop {
        match inbox.recv_timeout(WAKE) {
            Ok(Event::Stop) => {
                tracing::info!("dlisten: Ctrl+C, stopping");
                break Ok(());
            }
            Ok(Event::Output(n)) => {
                if let Err(e) = session.choose_output(n) {
                    tracing::warn!("dlisten: the output {n}: {e}");
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                wakes = wakes.wrapping_add(1);
                if wakes.is_multiple_of(OUTPUTS_EVERY) {
                    session.refresh_outputs();
                }
                if wakes.is_multiple_of(LOG_EVERY) {
                    tracing::info!("dlisten: {}", session.line());
                }
                if let Some(why) = session.ended() {
                    break Err(format!("the Dante device ended: {why}"));
                }
            }
            // The session holds a sender (its command hook): never.
            Err(RecvTimeoutError::Disconnected) => break Ok(()),
        }
    };
    session.stop();
    result
}
