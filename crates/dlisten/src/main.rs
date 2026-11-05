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
use tracing_subscriber::fmt::writer::MakeWriterExt;

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
    win::attach_console();
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
            fail(&e);
            ExitCode::FAILURE
        }
    }
}

/// A failure the user must see: stderr, and on Windows a message box (no
/// console from Explorer or the Run value).
fn fail(text: &str) {
    eprintln!("dlisten: {text}");
    #[cfg(windows)]
    win::message(&format!("dlisten: {text}"));
}

/// The log on stderr and in `file` (when it opens), RUST_LOG as the
/// devices read it, `info` by default.
fn init_log(file: Option<File>) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    match file {
        Some(f) => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(std::io::stderr.and(std::sync::Mutex::new(f)))
            .init(),
        None => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .init(),
    }
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

    // Ctrl+C on a thread of its own (tokio's signal, as dstack-dev's).
    std::thread::Builder::new()
        .name("dlisten-ctrl-c".into())
        .spawn(move || {
            let caught = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
                .and_then(|rt| {
                    rt.block_on(tokio::signal::ctrl_c())
                        .map_err(|e| e.to_string())
                });
            match caught {
                Ok(()) => {
                    let _ = events.send(Event::Stop);
                }
                Err(e) => tracing::warn!("dlisten: Ctrl+C cannot be caught ({e})"),
            }
        })
        .map_err(|e| format!("the Ctrl+C thread: {e}"))?;

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
