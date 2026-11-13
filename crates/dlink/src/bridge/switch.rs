// SPDX-License-Identifier: MIT OR Apache-2.0

//! A player handed over to a running audio thread (dstack #164's dsend
//! switch, made generic for dstack-asio's bridge, dstack #243).
//!
//! A control thread makes the replacement (another output's rate, a
//! reopened driver's) and hands it to the audio thread through a bounded
//! channel of one, which the audio thread reads with `try_recv`; the one it
//! replaces goes back through a second channel, so it is freed on the
//! control thread ([`Switch::collect`]), never on the audio thread.
//! [`Switch::send`] refuses a handover that could leave more than
//! [`RETIRED`] replaced items waiting, so the audio thread's `try_send`
//! back always has room and never frees one.
//!
//! [`Taker::take`] allocates nothing and makes no system call: std's
//! bounded channel takes no lock on `try_send`/`try_recv` while no thread
//! blocks on it, and neither end ever blocks.

use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};

/// [`Switch::send`]'s error while the audio thread has not taken the last
/// handover (it does not run), or the replaced ones still wait to be freed.
pub const NOT_TAKEN: &str = "the Dante device's pump has not taken the last output yet";
/// [`Switch::send`]'s error once the audio thread's end is gone.
pub const GONE: &str = "the Dante device is gone";
/// The room of the channel back to the control thread.
pub const RETIRED: usize = 2;

/// The control thread's end: hands a new item over and frees the ones it
/// replaced.
pub struct Switch<T> {
    outgoing: SyncSender<T>,
    retired: Receiver<T>,
    /// Items handed over whose predecessor was not freed yet: each
    /// handover retires exactly one, so at most this many wait in
    /// `retired` (kept within its room, [`RETIRED`]).
    pending: usize,
}

/// The audio thread's end.
pub struct Taker<T> {
    incoming: Receiver<T>,
    retired: SyncSender<T>,
}

/// A connected pair.
pub fn switch<T>() -> (Taker<T>, Switch<T>) {
    let (outgoing, incoming) = sync_channel(1);
    let (back, retired) = sync_channel(RETIRED);
    (
        Taker {
            incoming,
            retired: back,
        },
        Switch {
            outgoing,
            retired,
            pending: 0,
        },
    )
}

impl<T> Switch<T> {
    /// Hands `next` over; the audio thread uses it from its next
    /// [`Taker::take`]. The items it replaced are freed first. An error,
    /// and `next` freed here, when the last one was not taken yet or the
    /// replaced ones still wait to be freed ([`NOT_TAKEN`]), or the audio
    /// thread's end is gone ([`GONE`]).
    pub fn send(&mut self, next: T) -> Result<(), &'static str> {
        self.collect();
        if self.pending >= RETIRED {
            return Err(NOT_TAKEN);
        }
        self.outgoing.try_send(next).map_err(|e| match e {
            TrySendError::Full(_) => NOT_TAKEN,
            TrySendError::Disconnected(_) => GONE,
        })?;
        self.pending += 1;
        Ok(())
    }

    /// Frees the items the audio thread replaced; returns how many.
    pub fn collect(&mut self) -> usize {
        let mut n = 0;
        while self.retired.try_recv().is_ok() {
            n += 1;
        }
        self.pending = self.pending.saturating_sub(n);
        n
    }

    /// Handovers whose predecessor is not freed yet.
    pub fn pending(&self) -> usize {
        self.pending
    }
}

impl<T> Taker<T> {
    /// Takes an item handed over, if there is one, into `current`; the one
    /// it replaces goes back to be freed. Returns whether one came.
    pub fn take(&self, current: &mut T) -> bool {
        match self.incoming.try_recv() {
            Ok(next) => {
                let old = std::mem::replace(current, next);
                // `send` keeps at most RETIRED handovers unfreed, so the
                // channel has room.
                let _ = self.retired.try_send(old);
                true
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => false,
        }
    }
}

#[cfg(test)]
mod tests;
