// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! The device binary's command line (`--help`: [`super::usage`]), on
//! Linux and Windows (Windows has no control socket, so `ctl` refuses
//! there), as a library function: Cargo never builds a dependency's binaries,
//! so each application keeps a thin binary that calls [`main`] with its own
//! [`Build`] (dstack's `dante-stack-dev`, iemmixer's `iem-dante-dev`).
//!
//! The device itself is `crate::host`: here it is prepared with no engine
//! channel and started with SIGTERM and SIGINT ending it
//! (`host::Signals::Stop`). Exit codes (`host::exit_code`): 0 after a
//! signal and the goodbyes; 1 when the address is not there (yet), the
//! interface's MAC cannot be read, serving failed or the goodbyes were not
//! confirmed; 2 for the command line, the config, the state file or an
//! interface without a usable MAC. `clock` and `ctl` are
//! described in [`super::usage`].

use std::io::Write;
#[cfg(unix)]
use std::io::{BufRead, BufReader};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::AtomicI32;
use std::time::Duration;

use super::{Build, ClockLines, Command, ctl_succeeded, parse_args, usage};
use crate::clock::{DANTESYNC, MonoClock, POLL, Poller};
use crate::host::{self, Host, Signals};
use tokio::time::MissedTickBehavior;
use tracing::error;
use tracing_subscriber::EnvFilter;

/// How long `ctl` waits for the device's answer.
#[cfg(unix)]
const CTL_WAIT: Duration = Duration::from_secs(5);

pub fn main(build: Build) -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (opts, source) = match parse_args(&args) {
        Ok(Command::Version) => {
            println!("{}", build.line());
            return ExitCode::SUCCESS;
        }
        Ok(Command::Help) => {
            println!("{}", usage(build.name));
            return ExitCode::SUCCESS;
        }
        Ok(Command::Clock { seconds }) => return clock(build.name, seconds),
        Ok(Command::Ctl { socket, request }) => return ctl_request(build.name, &socket, &request),
        Ok(Command::Card(args)) => return card(build.name, &args),
        Ok(Command::Run(opts)) => (opts, host::ClockSource::Dantesync),
        Ok(Command::RunOnSystemClock(opts)) => (opts, host::ClockSource::System),
        Err(msg) => {
            eprintln!("{}: {msg}\n\n{}", build.name, usage(build.name));
            return ExitCode::from(2);
        }
    };
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    // The standalone device: no engine channel, the signals end it.
    let ran = host::prepare_with(&opts, &[], source)
        .and_then(|prepared| prepared.start(Signals::Stop))
        .and_then(Host::wait);
    match ran {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("{e}");
            ExitCode::from(host::exit_code(&e))
        }
    }
}

/// `clock --seconds <n>`: n polls of dantesync's time server, a second
/// apart (the first at once), each one line on stdout; no log.
fn clock(name: &str, seconds: u32) -> ExitCode {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("{name} clock: no runtime: {e}");
            return ExitCode::from(1);
        }
    };
    rt.block_on(async {
        let mut poller = match Poller::connect(DANTESYNC).await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("{name} clock: no socket to dantesync's time server: {e}");
                return ExitCode::from(1);
            }
        };
        let clock = MonoClock::new();
        let freq = AtomicI32::new(0);
        let mut lines = ClockLines::new();
        let mut tick = tokio::time::interval(POLL);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        for t_s in 0..seconds {
            tick.tick().await;
            let polled = poller.once(&clock, &freq).await;
            let line = lines.line(t_s, &polled);
            if let Err(e) = writeln!(std::io::stdout(), "{line}") {
                eprintln!("{name} clock: stdout: {e}");
                return ExitCode::from(1);
            }
        }
        ExitCode::SUCCESS
    })
}

/// `card`: another device's labels and name (see [`super::card`]).
fn card(name: &str, args: &[String]) -> ExitCode {
    use super::card::{self, Verb};
    use crate::arc::client::{REPLY_TIMEOUT, Udp};
    use std::net::{Ipv4Addr, SocketAddrV4};
    use std::sync::atomic::{AtomicBool, AtomicU16};

    let parsed = match card::parse(args) {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("{name} card: {msg}");
            return ExitCode::from(2);
        }
    };
    let changes = match &parsed.verb {
        Verb::Write(_) => {
            let mut text = String::new();
            if let Err(e) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut text) {
                eprintln!("{name} card: stdin: {e}");
                return ExitCode::from(2);
            }
            match card::parse_changes(&text) {
                Ok(c) => c,
                Err(msg) => {
                    eprintln!("{name} card: {msg}");
                    return ExitCode::from(2);
                }
            }
        }
        _ => Vec::new(),
    };
    let seq = AtomicU16::new(1);
    let requests = crate::ctl::Requests::default();
    let stop = AtomicBool::new(false);
    let to = SocketAddrV4::new(parsed.ip, crate::arc::PORT);
    let mut udp = match Udp::open(
        Ipv4Addr::UNSPECIFIED,
        to,
        &seq,
        REPLY_TIMEOUT,
        &requests,
        &stop,
    ) {
        Ok(u) => u,
        Err(e) => {
            eprintln!("{name} card: no socket to the card: {e}");
            return ExitCode::from(1);
        }
    };
    let line = |out: Result<String, serde_json::Error>| {
        out.unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    };
    let out = match &parsed.verb {
        Verb::Read => {
            card::read(&mut udp, &parsed.expected).map(|o| line(serde_json::to_string(&o)))
        }
        Verb::Write(flag) => card::write(&mut udp, &parsed.expected, flag, &changes)
            .map(|o| line(serde_json::to_string(&o))),
        Verb::Rename(new) => {
            card::rename(&mut udp, &parsed.expected, new).map(|o| line(serde_json::to_string(&o)))
        }
    };
    match out {
        Ok(line) => {
            if let Err(e) = writeln!(std::io::stdout(), "{line}") {
                eprintln!("{name} card: stdout: {e}");
                return ExitCode::from(1);
            }
            ExitCode::SUCCESS
        }
        Err(card::Refused(why)) => {
            eprintln!("{name} card: {why}");
            ExitCode::from(1)
        }
    }
}

/// `ctl` on Windows: no control socket there (dasio's tray reads the
/// device's shared memory), exit 1.
#[cfg(windows)]
fn ctl_request(name: &str, _path: &Path, _request: &str) -> ExitCode {
    eprintln!("{name} ctl: Windows has no control socket");
    ExitCode::from(1)
}

/// `ctl`: one request line to the control socket at `path`, its answer
/// line on stdout (exit 0 for `ok` or the status, 1 otherwise).
#[cfg(unix)]
fn ctl_request(name: &str, path: &Path, request: &str) -> ExitCode {
    let exchange = || -> std::io::Result<String> {
        let mut stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(CTL_WAIT))?;
        stream.set_write_timeout(Some(CTL_WAIT))?;
        stream.write_all(format!("{request}\n").as_bytes())?;
        let mut answer = String::new();
        BufReader::new(stream).read_line(&mut answer)?;
        Ok(answer)
    };
    let answer = match exchange() {
        Ok(answer) => answer,
        Err(e) => {
            eprintln!("{name} ctl: the control socket {}: {e}", path.display());
            return ExitCode::from(1);
        }
    };
    let line = answer.trim_end();
    if let Err(e) = writeln!(std::io::stdout(), "{line}") {
        eprintln!("{name} ctl: stdout: {e}");
        return ExitCode::from(1);
    }
    if ctl_succeeded(line) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
