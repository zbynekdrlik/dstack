// SPDX-License-Identifier: MIT OR Apache-2.0

//! The binaries' process glue dlisten and dsend share: the PC's name, the
//! default route's address, the log and Ctrl+C. Thin calls to the OS and
//! the runtime, run by the binaries (not per mutant: they read the
//! process's environment and install process-wide handlers).

use std::fs::File;
use std::io::Write;
use std::path::Path;

use dlink::Place;
use tracing_subscriber::fmt::writer::MakeWriterExt;

use crate::logfile;

/// This PC's name (Windows `COMPUTERNAME`, Linux the kernel's hostname).
pub fn host_name() -> String {
    std::env::var("COMPUTERNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/proc/sys/kernel/hostname").ok())
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// The address of the default route, waited for while the PC has no
/// default route yet (at boot or logon; [`dlink::default_route_place`],
/// dstack #230).
pub fn routed_ipv4() -> Result<Place, String> {
    dlink::default_route_place().map_err(|e| e.to_string())
}

/// The log on stderr and in `file` (when it opens), RUST_LOG as the
/// devices read it, `info` by default.
pub fn init_log(file: Option<File>) {
    init_log_to(file);
}

/// [`init_log`] into any writer (the run's [`logfile::Rotating`] log).
fn init_log_to<W: Write + Send + 'static>(file: Option<W>) {
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

/// A run's log: on stderr and in the file `log` ([`logfile::open`]: the
/// run before kept as `<name>.log.1`, dstack #470; rotated by size, the
/// older logs up to `.log.3`, dstack #506), with the INFO line
/// that names it (and the kept previous log, when one was kept). Whether
/// the file opened: without it the log is on stderr only and the app
/// still runs.
pub fn start_log(app: &str, log: &Path) -> bool {
    match logfile::open(log) {
        Ok(opened) => {
            init_log_to(Some(opened.file));
            match logfile::start_line(app, log, &opened.previous) {
                (true, line) => tracing::warn!("{line}"),
                (false, line) => tracing::info!("{line}"),
            }
            true
        }
        Err(e) => {
            init_log_to(None::<File>);
            tracing::warn!(log = %log.display(), "{app}: no log file ({e})");
            false
        }
    }
}

/// Runs `caught` on a thread of its own when Ctrl+C comes (tokio's signal,
/// as dstack-dev's); `app` names the thread and the log line when it
/// cannot be caught.
pub fn on_ctrl_c(app: &str, caught: impl FnOnce() + Send + 'static) -> Result<(), String> {
    let name = app.to_string();
    std::thread::Builder::new()
        .name(format!("{app}-ctrl-c"))
        .spawn(move || {
            let waited = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
                .and_then(|rt| {
                    rt.block_on(tokio::signal::ctrl_c())
                        .map_err(|e| e.to_string())
                });
            match waited {
                Ok(()) => caught(),
                Err(e) => tracing::warn!("{name}: Ctrl+C cannot be caught ({e})"),
            }
        })
        .map(|_| ())
        .map_err(|e| format!("the Ctrl+C thread: {e}"))
}
