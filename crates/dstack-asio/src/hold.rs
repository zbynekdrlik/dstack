// SPDX-License-Identifier: MIT OR Apache-2.0

//! The ASIO drivers this process holds (SongPlayer's `asio_hold.rs`,
//! songplayer #233 review round 1, dstack #243). A rebuilt device's
//! successor may start while the old one still stops, disposes and releases
//! the same driver; a second instance of one driver inside one process is
//! something many ASIO drivers do not expect (one client each). So a device
//! takes the driver's name here BEFORE it loads the driver and gives it back
//! only after the driver is released; a successor that finds it held is
//! refused ([`crate::Reason::Held`]) and tries again after the backoff. A
//! device that PARKED its driver (a callback never returned) keeps the hold
//! for the process's life and marks it parked, so a successor is told so.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// The drivers held in this process.
#[derive(Default)]
pub struct DriverHolds {
    held: Mutex<Vec<String>>,
    /// Held for good: their device parked them.
    parked: Mutex<Vec<String>>,
}

/// One held driver, given back when dropped.
pub struct DriverHold<'a> {
    holds: &'a DriverHolds,
    driver: String,
}

/// A poisoned list is still the list: a panic elsewhere never loses a hold.
fn lock(m: &Mutex<Vec<String>>) -> MutexGuard<'_, Vec<String>> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl DriverHolds {
    /// None held (a `const fn`, for a static).
    pub const fn new() -> Self {
        Self {
            held: Mutex::new(Vec::new()),
            parked: Mutex::new(Vec::new()),
        }
    }

    /// Hold `driver`, or `None` while another holder has it.
    pub fn claim(&self, driver: &str) -> Option<DriverHold<'_>> {
        let mut held = lock(&self.held);
        if held.iter().any(|h| h == driver) {
            return None;
        }
        held.push(driver.to_string());
        Some(DriverHold {
            holds: self,
            driver: driver.to_string(),
        })
    }

    /// Whether `driver` is held now.
    pub fn is_held(&self, driver: &str) -> bool {
        lock(&self.held).iter().any(|h| h == driver)
    }

    /// Whether `driver`'s device parked it (held until the process ends).
    pub fn is_parked(&self, driver: &str) -> bool {
        lock(&self.parked).iter().any(|h| h == driver)
    }
}

impl DriverHold<'_> {
    /// Keep the driver held for the process's life, marked parked.
    pub fn park(self) {
        lock(&self.holds.parked).push(self.driver.clone());
        // Never dropped: its Drop would give the driver back.
        std::mem::forget(self);
    }
}

impl Drop for DriverHold<'_> {
    fn drop(&mut self) {
        let mut held = lock(&self.holds.held);
        if let Some(i) = held.iter().position(|h| *h == self.driver) {
            held.swap_remove(i);
        }
    }
}

#[cfg(test)]
mod tests;
