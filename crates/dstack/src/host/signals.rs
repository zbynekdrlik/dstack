// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The device's signals (dstack #369 moved them out of `host.rs`): what
//! SIGTERM and SIGINT (Ctrl+C on Windows) do while the device serves
//! ([`Signals`], re-exported at `host::`), and the registered watch whose
//! stop ends serving.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(unix)]
use tokio::signal::unix::{Signal, SignalKind, signal};
use tracing::info;

#[cfg(unix)]
use super::run_failed;
use super::{Control, HostError};

/// What SIGTERM and SIGINT do while the device serves.
#[derive(Debug, Clone)]
pub enum Signals {
    /// End serving (the standalone device: the unit's stop).
    Stop,
    /// Only set the flag: the host (the engine) decides when to stop,
    /// through [`Host::stop`](super::Host::stop).
    Report(Arc<AtomicBool>),
    /// No handler at all: an app that hosts the device in its own process
    /// (an OBS plugin) owns its signals and stops the device through
    /// [`Host::stop`](super::Host::stop). A registered handler would
    /// outlive the device and keep SIGTERM from ending the app
    /// ([`crate::embed`]).
    Host,
}

/// The signals as registered (in the device's runtime).
#[cfg(unix)]
pub(super) enum Watch {
    Stop {
        term: Signal,
        int: Signal,
    },
    Report {
        term: Signal,
        int: Signal,
        flag: Arc<AtomicBool>,
    },
    /// No handler: only the host's stop.
    Host,
}

#[cfg(unix)]
impl Watch {
    /// Registers SIGTERM and SIGINT (inside the runtime), except for
    /// [`Signals::Host`], which registers nothing.
    pub(super) fn new(signals: Signals) -> Result<Watch, HostError> {
        let both = || -> Result<(Signal, Signal), HostError> {
            let term = signal(SignalKind::terminate()).map_err(run_failed)?;
            let int = signal(SignalKind::interrupt()).map_err(run_failed)?;
            Ok((term, int))
        };
        Ok(match signals {
            Signals::Stop => {
                let (term, int) = both()?;
                Watch::Stop { term, int }
            }
            Signals::Report(flag) => {
                let (term, int) = both()?;
                Watch::Report { term, int, flag }
            }
            Signals::Host => Watch::Host,
        })
    }

    /// The future that ends serving: a signal (for [`Signals::Stop`]) or
    /// the host's stop. For [`Signals::Report`] a task sets the flag at
    /// each signal while the device serves. Glue: tokio's signals.
    pub(super) async fn stop(self, control: &Control) {
        match self {
            Watch::Stop { mut term, mut int } => {
                tokio::select! {
                    _ = term.recv() => info!("SIGTERM: stopping"),
                    _ = int.recv() => info!("SIGINT: stopping"),
                    () = control.stop.notified() => info!("the device's stop was asked"),
                }
            }
            Watch::Report { term, int, flag } => {
                tokio::spawn(report(term, int, flag));
                control.stop.notified().await;
                info!("the device's stop was asked");
            }
            Watch::Host => {
                control.stop.notified().await;
                info!("the device's stop was asked");
            }
        }
    }
}

/// Sets `flag` at every SIGTERM or SIGINT (the host stops the device).
#[cfg(unix)]
async fn report(mut term: Signal, mut int: Signal, flag: Arc<AtomicBool>) {
    loop {
        let which = tokio::select! {
            Some(()) = term.recv() => "SIGTERM",
            Some(()) = int.recv() => "SIGINT",
            else => return,
        };
        info!(
            signal = which,
            "a stop was asked: the host stops the device"
        );
        flag.store(true, Ordering::Release);
    }
}

/// On Windows: a console Ctrl+C (or the console's close) in place of the
/// signals.
#[cfg(windows)]
pub(super) enum Watch {
    Stop,
    Report(Arc<AtomicBool>),
    Host,
}

#[cfg(windows)]
impl Watch {
    pub(super) fn new(signals: Signals) -> Result<Watch, HostError> {
        Ok(match signals {
            Signals::Stop => Watch::Stop,
            Signals::Report(flag) => Watch::Report(flag),
            Signals::Host => Watch::Host,
        })
    }

    /// The future that ends serving: Ctrl+C (for [`Signals::Stop`]) or the
    /// host's stop; for [`Signals::Report`] Ctrl+C only sets the flag.
    pub(super) async fn stop(self, control: &Control) {
        match self {
            Watch::Stop => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => info!("Ctrl+C: stopping"),
                    () = control.stop.notified() => info!("the device's stop was asked"),
                }
            }
            Watch::Report(flag) => {
                tokio::spawn(async move {
                    while tokio::signal::ctrl_c().await.is_ok() {
                        info!("Ctrl+C: the host stops the device");
                        flag.store(true, Ordering::Release);
                    }
                });
                control.stop.notified().await;
                info!("the device's stop was asked");
            }
            Watch::Host => {
                control.stop.notified().await;
                info!("the device's stop was asked");
            }
        }
    }
}

// libc (raise) is a Linux dependency only.
#[cfg(all(test, target_os = "linux"))]
mod tests;
