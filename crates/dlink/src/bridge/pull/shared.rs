// SPDX-License-Identifier: MIT OR Apache-2.0

//! One [`RxPull`] shared by the threads that may run it (dstack #307):
//! the consumer's callbacks (an output switch, an ASIO driver's reopen:
//! an old stream's last callback can meet the new one's) and the thread
//! that watches it and drains while nothing pulls ([`IdleWatch`]). The
//! callback never waits: [`SharedPull::pull`] only tries the lock. dlisten
//! and dstack-asio's to-ASIO side run it.

use std::sync::{Arc, Mutex, TryLockError};

use super::{Fifo, IdleWatch, Look, PullStats, RxPull};

/// A pull behind a lock the callback only tries (module doc).
#[derive(Debug)]
pub struct SharedPull {
    pub(super) pull: Mutex<RxPull>,
    stats: Arc<PullStats>,
    fifo: Arc<Fifo>,
    channels: usize,
}

impl SharedPull {
    /// Shares `pull`.
    pub fn new(pull: RxPull) -> Self {
        SharedPull {
            stats: pull.stats(),
            fifo: Arc::clone(pull.fifo()),
            channels: pull.channels().len(),
            pull: Mutex::new(pull),
        }
    }

    /// The consumer's pull at the top of its callback ([`RxPull::pull`]),
    /// returns the frames written. Never waits: when another thread holds
    /// the lock (an earlier stream's last callback, the idle drain), this
    /// call pulls nothing (0) and the callback plays what the FIFO holds,
    /// once maybe a little emptier. A lock a panicking pull poisoned still
    /// pulls: its state is read positions ([`SharedPull::clear_poison`]
    /// reports it off the callback). No allocation.
    pub fn pull(&self) -> usize {
        match self.pull.try_lock() {
            Ok(mut p) => p.pull(),
            Err(TryLockError::Poisoned(e)) => e.into_inner().pull(),
            Err(TryLockError::WouldBlock) => 0,
        }
    }

    /// The watching thread's look, on a steady period (about every
    /// 250 ms; never the callback): `watch` sees whether the consumer
    /// still pulls ([`IdleWatch::look`]), and while it does not, the
    /// sockets are drained here ([`RxPull::drain_idle`]) so the flow
    /// stays heard. Only under the lock, never waiting for it, and only
    /// while no callback pulled since the look (one that resumed in
    /// between keeps its read position).
    pub fn keep(&self, watch: &mut IdleWatch) -> Look {
        let pulls = self.stats.pulls();
        let look = watch.look(pulls, self.stats.restarts());
        if look.idle
            && let Ok(mut p) = self.pull.try_lock()
            && self.stats.pulls() == pulls
        {
            p.drain_idle();
        }
        look
    }

    /// Clears a lock a panicking pull poisoned (its message went to
    /// stderr); `true` when it was, for the watching thread's log.
    pub fn clear_poison(&self) -> bool {
        let poisoned = self.pull.is_poisoned();
        if poisoned {
            self.pull.clear_poison();
        }
        poisoned
    }

    /// The pull's counts.
    pub fn stats(&self) -> Arc<PullStats> {
        Arc::clone(&self.stats)
    }

    /// The bridge FIFO the pull writes.
    pub fn fifo(&self) -> &Arc<Fifo> {
        &self.fifo
    }

    /// The RX channels the pull reads (one a FIFO channel).
    pub fn channels(&self) -> usize {
        self.channels
    }
}
