// SPDX-License-Identifier: MIT OR Apache-2.0

//! dsend's run on Windows (dstack #164): `--list-outputs`, and the main
//! thread of a run (the session, the log, Ctrl+C, dtray's choices, the
//! watch and the refresh). Glue, run by hand on Windows.

use std::fs::File;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use dsend::cli::Options;
use dstack_endpoint::host::{self, init_log};
use dstack_endpoint::outputs;

use crate::audio;
use crate::session::{self, Event, Session};

/// How often the main thread wakes (dtray's choices come at once).
const WAKE: Duration = Duration::from_secs(1);
/// Every how many wakes the status is logged (10 s).
const LOG_EVERY: u32 = 10;
/// Every how many wakes the output devices are read again (5 s).
const OUTPUTS_EVERY: u32 = 5;

/// `--list-outputs`: only where dsend can capture them.
pub fn list() -> Result<(), String> {
    init_log(None);
    let (_, described) = audio::outputs()?;
    println!("{}", outputs::list_text(&described)?);
    Ok(())
}

pub fn run(o: &Options) -> Result<(), String> {
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
