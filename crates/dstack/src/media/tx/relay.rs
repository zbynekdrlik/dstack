// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The escape relay (dstack #141, `DeviceConfig::spin_escape_ms`, Windows):
//! while the TX thread blocks its bounded escape (one timer tick, up to
//! [`crate::clock::ESCAPE_MAX_NS`]), a second thread runs its passes, so no
//! packet waits through the block and none is sent ahead of its time.
//!
//! [`Relay`] hands one value (the [`super::Transmitter`]) between the two
//! threads: exactly one of them holds it at a time. The TX thread offers it
//! ([`Relay::hand_over`]), the relay thread takes it and runs until it is
//! asked back ([`Relay::serve`]), the TX thread takes it back after its
//! block ([`Relay::take_back`]). The hot path takes no lock: the value moves
//! only at an escape, once a period. Between escapes the relay thread
//! sends the TX pass's socket packets on a frame-sender host
//! ([`Relay::serve_with`], `super::outbox`, dstack #485), and parks when
//! there are none.

use std::sync::atomic::{AtomicU8, Ordering, fence};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::thread::Thread;

/// Nobody covers: the TX thread holds the value.
const IDLE: u8 = 0;
/// The TX thread put the value in the slot for the relay to take.
const OFFERED: u8 = 1;
/// The relay thread took it and runs the passes.
const COVERING: u8 = 2;
/// The TX thread's block is over: the relay hands the value back.
const RETURN: u8 = 3;
/// The relay thread ends.
const STOPPED: u8 = 4;

/// The handoff of one value between the TX thread and its relay thread.
#[derive(Debug)]
pub struct Relay<T> {
    slot: Mutex<Option<T>>,
    state: AtomicU8,
    /// The relay thread, once it serves: an offer and a stop unpark it.
    server: OnceLock<Thread>,
}

impl<T> Default for Relay<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Relay<T> {
    pub const fn new() -> Self {
        Relay {
            slot: Mutex::new(None),
            state: AtomicU8::new(IDLE),
            server: OnceLock::new(),
        }
    }

    fn put(&self, value: T) {
        *self.slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(value);
    }

    fn take(&self) -> Option<T> {
        self.slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// The TX thread offers `value` and waits while `wait` says to (it is
    /// called between looks: a bounded spin) for the relay thread to take
    /// it. `Ok`: the relay covers now. `Err(value)`: it did not take it in
    /// time (or has stopped), and the value is back with the caller.
    pub fn hand_over(&self, value: T, mut wait: impl FnMut() -> bool) -> Result<(), T> {
        if self.state.load(Ordering::Acquire) == STOPPED {
            return Err(value);
        }
        self.put(value);
        self.state.store(OFFERED, Ordering::Release);
        self.wake();
        loop {
            if self.state.load(Ordering::Acquire) == COVERING {
                return Ok(());
            }
            if !wait() {
                break;
            }
        }
        // Not taken in time: withdraw the offer, unless the relay took it
        // meanwhile.
        match self
            .state
            .compare_exchange(OFFERED, IDLE, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => Err(self.take().expect("an offered value is in the slot")),
            Err(_) => Ok(()),
        }
    }

    /// The TX thread's block is over: asks the value back and spins until
    /// the relay thread returns it (after its current pass).
    pub fn take_back(&self) -> T {
        self.state.store(RETURN, Ordering::Release);
        while self.state.load(Ordering::Acquire) != IDLE {
            std::hint::spin_loop();
        }
        self.take().expect("a returned value is in the slot")
    }

    /// The relay thread's loop: blocks until offered a value, then runs
    /// `cover` on it with a check that says when the TX thread wants it
    /// back (`cover` returns then); hands it back and blocks again. Ends
    /// once [`Relay::stop`] was called.
    pub fn serve(&self, cover: impl FnMut(&mut T, &dyn Fn() -> bool)) {
        self.serve_with(cover, || false);
    }

    /// [`Relay::serve`] with other work between offers: `idle` runs while
    /// nothing is offered and says whether it did something (then it runs
    /// again at once); the thread parks only when it did nothing, woken by
    /// an offer, a [`Relay::stop`] or another `unpark` of it (dstack #485:
    /// the queue of the TX pass's socket sends).
    pub fn serve_with(
        &self,
        mut cover: impl FnMut(&mut T, &dyn Fn() -> bool),
        mut idle: impl FnMut() -> bool,
    ) {
        // Set before the first look at the state, read by an offer after
        // it set the state: one of the two sees the other.
        let first = self.server.set(std::thread::current()).is_ok();
        debug_assert!(first, "one relay thread serves a relay");
        fence(Ordering::SeqCst);
        loop {
            match self.state.load(Ordering::Acquire) {
                STOPPED => return,
                OFFERED => {}
                _ => {
                    if !idle() {
                        std::thread::park();
                    }
                    continue;
                }
            }
            if self
                .state
                .compare_exchange(OFFERED, COVERING, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                // Withdrawn meanwhile (or stopped): wait for the next offer.
                continue;
            }
            let mut value = self.take().expect("an offered value is in the slot");
            let back = || self.state.load(Ordering::Acquire) == RETURN;
            cover(&mut value, &back);
            while !back() {
                std::hint::spin_loop();
            }
            self.put(value);
            self.state.store(IDLE, Ordering::Release);
        }
    }

    /// Wakes the relay thread (an offer, a stop) once it serves.
    fn wake(&self) {
        fence(Ordering::SeqCst);
        if let Some(server) = self.server.get() {
            server.unpark();
        }
    }

    /// Ends the relay thread's [`Relay::serve`] once it is not covering
    /// (the TX thread calls it when it stops, holding the value).
    pub fn stop(&self) {
        self.state.store(STOPPED, Ordering::Release);
        self.wake();
    }
}

#[cfg(test)]
mod tests;
