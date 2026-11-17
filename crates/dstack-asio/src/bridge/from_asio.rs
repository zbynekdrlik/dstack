// SPDX-License-Identifier: MIT OR Apache-2.0

//! The ASIO -> Dante side pulled by the device's TX thread (dstack #364,
//! the from-ASIO twin of #307 step 2; the TX pull of #363, the design of
//! #346 approved 17.11.2026).
//!
//! [`FromAsio`] is the from-ASIO [`Player`] in a `dlink::bridge::TxPull`
//! (with the [`Switch`](dlink::bridge::Switch) that hands it each open's
//! player), plus the take of the driver's blocks for the modelled block
//! age ([`BlockAge::taken`], dstack #325): the TX thread fills the pulled
//! TX channels from the from-ASIO FIFO in whole blocks at the pump's due
//! time and stamp, just before it sends, so a late pump no longer leaves
//! sent stamps silent. Made by [`super::Bridge::pull_from_asio`]; started
//! with `dlink::Link::start_pulling_with_tx` (with the to-ASIO pull) or
//! `dlink::Link::start_tx_pulling`.
//!
//! Its `process` allocates nothing, takes no lock and makes no system
//! call but the age's clock read: the pull's (module doc of
//! `dlink::bridge::tx_pull`) and the age's atomics.

use std::sync::Arc;

use dlink::bridge::{Player, TxProcess, TxPull};
use dstack_audio_io::{Block, Process};

use super::BlockAge;

/// The from-ASIO side the TX thread runs (module doc).
pub struct FromAsio {
    pull: TxPull,
    age: Arc<BlockAge>,
}

impl std::fmt::Debug for FromAsio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FromAsio")
            .field("pull", &self.pull)
            .finish_non_exhaustive()
    }
}

impl FromAsio {
    pub(super) fn new(pull: TxPull, age: Arc<BlockAge>) -> Self {
        FromAsio { pull, age }
    }

    /// The player it plays now (the last open's, once the TX thread took
    /// it).
    pub fn player(&self) -> &Player {
        self.pull.player()
    }
}

impl Process for FromAsio {
    #[cfg_attr(iem_rtsan, sanitize(realtime = "nonblocking"))]
    fn process(&mut self, block: &mut Block<'_>) {
        // The take of the driver's blocks (dstack #325), now on the TX
        // thread's fill.
        self.age.taken();
        self.pull.process(block);
    }
}

impl TxProcess for FromAsio {
    fn outputs(&self) -> usize {
        self.pull.outputs()
    }
}

#[cfg(test)]
mod tests;
