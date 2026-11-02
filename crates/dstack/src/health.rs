// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! The device's health as an application reads it: one control-socket
//! status line ([`parse_status`], [`read_status`] on Linux) and the
//! device's own reasons to look from two reads a second apart
//! ([`device_reasons`], pure).
//!
//! An application's box health (its services, its server, how long a
//! clock may stay not ready after a start, its exit codes) is the
//! application's: it builds that verdict on these reasons. Until v0.2.0
//! the stack carried iemmixer's box verdict and the `health` command;
//! they moved to iemmixer (see CHANGELOG.md).

use serde::{Deserialize, Serialize};

/// The status's `clock` as health reads it (`ready` and `holdover` 0/1,
/// as the status writes them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct Clock {
    pub ready: u8,
    /// The last poll of dantesync was in holdover.
    pub holdover: u8,
    /// Runs of polls in holdover, since the device started.
    pub holdovers: u32,
    pub re_anchors: u32,
    /// dantesync's frequency adjustment.
    pub freq_ppb: i32,
}

/// The status's `engine`: the engine's pump (`media` 0/1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct Pump {
    /// On the media clock (1), pacing itself (0).
    pub media: u8,
    pub blocks: u64,
    pub late: u64,
    pub missed: u64,
    pub rebootstraps: u64,
    pub max_process_us: u64,
}

/// What health reads of one control-socket status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Status {
    pub clock: Clock,
    /// Only once an engine hosts the device.
    #[serde(default)]
    pub engine: Option<Pump>,
}

/// One status line (`crate::ctl::status`), `None` for anything else (an
/// `error: …` answer, a line that does not read).
pub fn parse_status(line: &str) -> Option<Status> {
    serde_json::from_str(line).ok()
}

/// The device's own reasons to look, from two status reads a second
/// apart; [`device_reasons`] lists each at most once, in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceReason {
    /// A read gave no status (no socket, no answer, an error answer).
    NoStatus,
    /// The clock is not ready at the second read.
    ClockNotReady,
    /// dantesync was in holdover at either read, or went into it between
    /// them.
    Holdover,
    /// The clock is ready and the engine's pump paces itself.
    PumpFree,
    /// The pump's blocks did not rise between the reads, or a read has no
    /// pump while an engine should host the device.
    PumpStalled,
    /// The pump re-bootstrapped between the reads.
    PumpRebootstrap,
}

/// The device's reasons from the reads `first` and `second` (`None` for a
/// read without a status); with `engine`, the engine's pump between them.
/// A missing read is [`DeviceReason::NoStatus`] alone: nothing else can be
/// judged. Pure.
pub fn device_reasons(
    first: Option<&Status>,
    second: Option<&Status>,
    engine: bool,
) -> Vec<DeviceReason> {
    let (Some(a), Some(b)) = (first, second) else {
        return vec![DeviceReason::NoStatus];
    };
    let mut reasons = Vec::new();
    if b.clock.ready != 1 {
        reasons.push(DeviceReason::ClockNotReady);
    }
    if a.clock.holdover == 1 || b.clock.holdover == 1 || b.clock.holdovers > a.clock.holdovers {
        reasons.push(DeviceReason::Holdover);
    }
    if engine {
        match (a.engine, b.engine) {
            (Some(pa), Some(pb)) => {
                if b.clock.ready == 1 && pb.media != 1 {
                    reasons.push(DeviceReason::PumpFree);
                }
                if pb.blocks <= pa.blocks {
                    reasons.push(DeviceReason::PumpStalled);
                }
                if pb.rebootstraps > pa.rebootstraps {
                    reasons.push(DeviceReason::PumpRebootstrap);
                }
            }
            _ => reasons.push(DeviceReason::PumpStalled),
        }
    }
    reasons
}

#[cfg(target_os = "linux")]
pub use glue::read_status;

/// The Linux glue: one read of the control socket.
#[cfg(target_os = "linux")]
mod glue {
    use std::io::{self, BufRead, BufReader, Read, Write};
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::time::Duration;

    use super::{Status, parse_status};

    /// The longest status line read, bytes.
    pub const MAX_READ: u64 = 1 << 20;

    /// One `status` request to the control socket at `path`, its answer
    /// line read within `wait` a read; `None` without a socket, an answer
    /// or a status.
    pub fn read_status(path: &Path, wait: Duration) -> Option<Status> {
        let exchange = || -> io::Result<String> {
            let mut stream = UnixStream::connect(path)?;
            stream.set_read_timeout(Some(wait))?;
            stream.set_write_timeout(Some(wait))?;
            stream.write_all(b"status\n")?;
            let mut line = String::new();
            BufReader::new(stream.take(MAX_READ)).read_line(&mut line)?;
            Ok(line)
        };
        exchange().ok().and_then(|line| parse_status(&line))
    }
}

#[cfg(test)]
mod tests;
