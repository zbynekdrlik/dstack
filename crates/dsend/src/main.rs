// SPDX-License-Identifier: MIT OR Apache-2.0
#![cfg_attr(windows, windows_subsystem = "windows")]

//! `dsend`: sends a PC's sound into Dante (dstack #164), see the library.
//! It has no window and no tray of its own: dtray shows it, and its Output
//! menu sends `app output <n>`. The binary is glue: the command line
//! ([`dsend::cli`]), the session (`session`: the capture and the dlink
//! device), the log (stderr and `<state file>.log`) and Ctrl+C. On
//! Windows it is a GUI-subsystem program: its output goes to the console
//! it was started from, if any. Exit codes: 0 done, 1 a failure (not
//! Windows, no output device, the device did not start), 2 a usage error.

use std::fs::File;
use std::process::ExitCode;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use dsend::cli::{self, Command, Options};
use dstack_endpoint::host::{self, init_log};
use dstack_endpoint::outputs;

use session::{Event, Session};

mod audio;
mod session;

/// How often the main thread wakes (dtray's choices come at once).
const WAKE: Duration = Duration::from_secs(1);
/// Every how many wakes the status is logged (10 s).
const LOG_EVERY: u32 = 10;
/// Every how many wakes the output devices are read again (5 s).
const OUTPUTS_EVERY: u32 = 5;

fn main() -> ExitCode {
    #[cfg(windows)]
    let (attached, has_stderr) = dstack_endpoint::win::attach_console();
    #[cfg(not(windows))]
    let (attached, has_stderr) = (true, true);
    let boxed = dstack_endpoint::alert::shows_box(attached, has_stderr);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match cli::parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("dsend: {e}\n\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
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
            fail(&e, boxed);
            ExitCode::FAILURE
        }
    }
}

/// A failure the user must see: stderr, and a message box when nothing
/// reads stderr (a Windows start from Explorer).
fn fail(text: &str, boxed: bool) {
    eprintln!("dsend: {text}");
    #[cfg(windows)]
    if boxed {
        dstack_endpoint::win::message("dsend", &format!("dsend: {text}"));
    }
    #[cfg(not(windows))]
    let _ = boxed;
}

/// `--list-outputs`: only where dsend can capture them.
fn list() -> Result<(), String> {
    cli::capture_check(cfg!(windows))?;
    init_log(None);
    let (_, described) = audio::outputs()?;
    println!("{}", outputs::list_text(&described)?);
    Ok(())
}

fn run(o: &Options) -> Result<(), String> {
    // Before anything starts: only Windows can capture an output.
    cli::capture_check(cfg!(windows))?;
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
        tracing::info!(log = %log.display(), "dsend: the log");
    }
    let (events, inbox) = mpsc::channel();
    let mut session = Session::start(o, events.clone())?;
    host::on_ctrl_c("dsend", move || {
        let _ = events.send(Event::Stop);
    })?;

    let mut wakes: u32 = 0;
    let result = loop {
        match inbox.recv_timeout(WAKE) {
            Ok(Event::Stop) => {
                tracing::info!("dsend: Ctrl+C, stopping");
                break Ok(());
            }
            Ok(Event::Output(n)) => {
                if let Err(e) = session.choose_output(n) {
                    tracing::warn!("dsend: the output {n}: {e}");
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                wakes = wakes.wrapping_add(1);
                session.watch();
                if wakes.is_multiple_of(OUTPUTS_EVERY) {
                    session.refresh_outputs();
                }
                if wakes.is_multiple_of(LOG_EVERY) {
                    tracing::info!("dsend: {}", session.line());
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
