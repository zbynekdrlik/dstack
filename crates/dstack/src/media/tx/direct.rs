// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The socket past the qdisc across an interface re-create (dstack #414).
//! It names its interface's index in every send ([`Direct::send`]);
//! after the NIC is destroyed and created again under a new index (a USB
//! reset: same name, same address) every direct send would fail with
//! ENXIO, and the slot never goes back to its UDP socket, since its
//! receiver's MAC comes back on the same interface name. The TX interface
//! watch ([`TxShared::keep_send_interface`], from the daemon's tick) moves
//! the socket to the new index ([`Direct::follow`]): one atomic word, so
//! the TX thread keeps reading it without a lock or an allocation.
//!
//! [`Direct::send`]: crate::media::bypass::Direct::send
//! [`Direct::follow`]: crate::media::bypass::Direct::follow

use std::io;

use tracing::info;

use super::TxShared;
#[cfg(all(test, target_os = "linux"))]
use crate::media::bypass::Direct;

/// The INFO line after the direct socket moved to a re-created
/// interface's index (CI's re-create test reads it).
pub(crate) const FOLLOWED: &str = "the direct socket sends on the interface's new index";

/// What a failed move names: the act's warning is the TX watch's, worded
/// for the flow sockets it also names again.
pub(crate) const DIRECT_SOCKET: &str = "the direct socket (audio TX past the qdisc)";

impl TxShared {
    /// Moves the socket past the qdisc to its interface's index now
    /// ([`Direct::follow`](crate::media::bypass::Direct::follow), dstack
    /// #414), logged once when it moved; an error naming the direct socket
    /// when its interface is not there (the watch warns it beside the flow
    /// sockets' naming and asks again at its next look). Nothing without
    /// the socket.
    pub(crate) fn follow_direct(&self) -> io::Result<()> {
        let Some((direct, nic)) = &self.direct else {
            return Ok(());
        };
        let moved = direct
            .follow(nic)
            .map_err(|e| io::Error::new(e.kind(), format!("{DIRECT_SOCKET} on {nic}: {e}")))?;
        if let Some((old, new)) = moved {
            info!(nic = %nic, old_index = old, new_index = new, "{FOLLOWED}");
        }
        Ok(())
    }
}

#[cfg(test)]
impl TxShared {
    /// This state with `direct` past the qdisc on the interface `nic`
    /// (tests: a stand-in, `Direct::stand_in`).
    #[cfg(target_os = "linux")]
    pub(crate) fn with_direct(mut self, direct: Direct, nic: &str) -> Self {
        self.direct = Some((direct, nic.to_string()));
        self
    }

    /// The interface index the direct socket sends on now; `None` without
    /// one.
    #[cfg(target_os = "linux")]
    pub(crate) fn direct_index(&self) -> Option<i32> {
        self.direct.as_ref().map(|(d, _)| d.index())
    }

    /// This state without the socket past the qdisc (a test run as root
    /// opens a real one on `lo`).
    #[cfg(target_os = "linux")]
    pub(crate) fn without_direct(mut self) -> Self {
        self.direct = None;
        self
    }
}
