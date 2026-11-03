// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The DSCP of our audio on Windows (dstack #1, 3.11.2026). Dante hardware
//! marks its audio Expedited Forwarding ([`super::tx::DSCP_EF`]); switches
//! and NIC filters that rank traffic put it ahead of the rest. Linux marks
//! a flow socket with `IP_TOS` when it opens it. Windows ignores `IP_TOS`
//! from a program, so our audio left with DSCP 0 unless the PC had a QoS
//! policy: dasio's NDIS filter on the FOH PC deferred such a flow behind
//! TCP (peaks 4–7.5 ms at a receiver), while the card's EF flow went
//! through.
//!
//! On Windows the control side adds each TX flow socket to a QoS2 flow per
//! destination (`QOSAddSocketToFlow`, audio/video, not adaptive) and sets
//! its DSCP to EF (`QOSSetFlow`, `QOSSetOutgoingDSCPValue`). Windows lets
//! only an administrator process set the DSCP (a service is one); another
//! process gets one warning naming the QoS policy that does it instead.
//! [`Marks`] follows the table's destinations; [`changes`] and [`verdict`]
//! hold the decisions, the platform glue only makes the calls.

use std::net::{SocketAddrV4, UdpSocket};

use super::tx::MAX_FLOWS;

#[cfg(windows)]
mod windows;

/// The slots whose destination moved from `marked`: (slot, the new one,
/// or none when the flow ended).
pub fn changes(
    marked: &[Option<SocketAddrV4>],
    dests: &[Option<SocketAddrV4>],
) -> Vec<(usize, Option<SocketAddrV4>)> {
    marked
        .iter()
        .zip(dests)
        .enumerate()
        .filter(|(_, (m, d))| m != d)
        .map(|(slot, (_, d))| (slot, *d))
        .collect()
}

/// How setting the DSCP went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The flow sends EF.
    Set,
    /// Windows refused it: the process is not an administrator.
    NotAdmin,
    /// Another failure: the Windows error code.
    Failed(u32),
}

/// Windows' `ERROR_ACCESS_DENIED`.
pub const ERROR_ACCESS_DENIED: u32 = 5;

/// Windows' `ERROR_NOT_FOUND`: what `QOSAddSocketToFlow` returns for a
/// destination on this PC (QoS2 has no route to it).
pub const ERROR_NOT_FOUND: u32 = 1168;

/// Whether a failed `QOSAddSocketToFlow` with `code` only means a
/// destination on this PC (another device of ours on the same address):
/// nothing leaves the NIC to mark, so it is no fault.
pub fn local_destination(code: u32) -> bool {
    code == ERROR_NOT_FOUND
}

/// The verdict of a `QOSSetFlow` that returned `error` (none: it worked).
pub fn verdict(error: Option<u32>) -> Verdict {
    match error {
        None => Verdict::Set,
        Some(ERROR_ACCESS_DENIED) => Verdict::NotAdmin,
        Some(code) => Verdict::Failed(code),
    }
}

/// The marks of the TX flow sockets: each slot's destination as last
/// marked, and the platform's QoS state.
#[derive(Debug)]
pub struct Marks {
    marked: [Option<SocketAddrV4>; MAX_FLOWS],
    #[cfg(windows)]
    qos: windows::Qos,
}

impl Default for Marks {
    fn default() -> Self {
        Self::new()
    }
}

impl Marks {
    /// No slot marked yet.
    pub fn new() -> Self {
        Marks {
            marked: [None; MAX_FLOWS],
            #[cfg(windows)]
            qos: windows::Qos::new(),
        }
    }

    /// Each slot's destination as last marked.
    pub fn marked(&self) -> &[Option<SocketAddrV4>] {
        &self.marked
    }

    /// Follows the slots' destinations `dests`: a slot whose destination
    /// changed is marked for its new one (Windows; elsewhere the socket was
    /// marked when it opened).
    pub fn follow(&mut self, sockets: &[UdpSocket], dests: &[Option<SocketAddrV4>]) {
        for (slot, dest) in changes(&self.marked, dests) {
            #[cfg(windows)]
            if let Some(socket) = sockets.get(slot) {
                self.qos.mark(slot, socket, dest);
            }
            #[cfg(not(windows))]
            let _ = sockets;
            self.marked[slot] = dest;
        }
    }
}

#[cfg(test)]
mod tests;
