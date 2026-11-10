// SPDX-License-Identifier: MIT OR Apache-2.0

//! The binaries' process glue dlisten and dsend share: the PC's name, the
//! default route's address, the log and Ctrl+C. Thin calls to the OS and
//! the runtime, run by the binaries (not per mutant: they read the
//! process's environment and install process-wide handlers).

use std::fs::File;

use dlink::Place;
use tracing_subscriber::fmt::writer::MakeWriterExt;

/// This PC's name (Windows `COMPUTERNAME`, Linux the kernel's hostname).
pub fn host_name() -> String {
    std::env::var("COMPUTERNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/proc/sys/kernel/hostname").ok())
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// The address of the default route (connecting a UDP socket picks the
/// route and sends nothing).
pub fn routed_ipv4() -> Result<Place, String> {
    let probe = std::net::UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    probe.connect("192.0.2.1:9").map_err(|e| e.to_string())?;
    match probe.local_addr().map_err(|e| e.to_string())? {
        std::net::SocketAddr::V4(a) => Ok(Place::Ip(*a.ip())),
        other => Err(format!("no IPv4 address on the default route ({other})")),
    }
}

/// The log on stderr and in `file` (when it opens), RUST_LOG as the
/// devices read it, `info` by default.
pub fn init_log(file: Option<File>) {
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
