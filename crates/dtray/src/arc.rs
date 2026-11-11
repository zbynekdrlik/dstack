// SPDX-License-Identifier: MIT OR Apache-2.0

//! One exchange with a local device's ARC ([`dstack::arc::client::Udp`]),
//! from the PC's address the ARC sits on: the tray menu's settings reads
//! and latency writes, and `dtray --set-latency`'s ([`crate::set`]).

use std::net::SocketAddrV4;
use std::sync::atomic::{AtomicBool, AtomicU16};
use std::time::Duration;

use dstack::arc::client::Udp;
use dstack::ctl::Requests;

/// How long an ARC read or write waits for each reply.
pub const WAIT: Duration = Duration::from_millis(300);

/// `f` on a client of the ARC at `arc`, from the PC's address it sits on
/// (each reply within [`WAIT`]); the reason when no socket opened.
pub fn call<T>(
    arc: SocketAddrV4,
    f: impl FnOnce(&mut Udp<'_>) -> Result<T, String>,
) -> Result<T, String> {
    let seq = AtomicU16::new(1);
    let requests = Requests::default();
    let stop = AtomicBool::new(false);
    let mut udp = Udp::open(*arc.ip(), arc, &seq, WAIT, &requests, &stop)
        .map_err(|e| format!("no socket: {e}"))?;
    f(&mut udp)
}

#[cfg(test)]
mod tests;
