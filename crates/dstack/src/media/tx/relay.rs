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
//! only at an escape, once a period.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Condvar, Mutex, PoisonError};

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
    wake: Condvar,
    state: AtomicU8,
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
            wake: Condvar::new(),
            state: AtomicU8::new(IDLE),
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
        self.wake.notify_one();
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
    pub fn serve(&self, mut cover: impl FnMut(&mut T, &dyn Fn() -> bool)) {
        loop {
            {
                let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
                loop {
                    match self.state.load(Ordering::Acquire) {
                        STOPPED => return,
                        OFFERED => break,
                        _ => {
                            slot = self.wake.wait(slot).unwrap_or_else(PoisonError::into_inner);
                        }
                    }
                }
                drop(slot);
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

    /// Ends the relay thread's [`Relay::serve`] once it is not covering
    /// (the TX thread calls it when it stops, holding the value).
    pub fn stop(&self) {
        let _slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        self.state.store(STOPPED, Ordering::Release);
        self.wake.notify_all();
    }
}

#[cfg(test)]
mod tests;
