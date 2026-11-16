// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! Who writes the RX rings (dstack #326, #307): the engine's pump, the
//! app's consumer or the RX thread, claimed once at the device's start
//! ([`RxShared`]'s `ring_writer`); the first claim holds for good.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::{Intake, RxShared};

/// [`RxShared`]'s ring writer: not chosen yet.
pub(super) const WRITER_UNSET: u8 = 0;
/// The RX thread writes the rings ([`Receiver`](super::Receiver) holds
/// the [`Intake`]).
const WRITER_THREAD: u8 = 1;
/// The engine's pump writes them ([`RxShared::pump_intake`]).
const WRITER_PUMP: u8 = 2;
/// The app's consumer writes them ([`RxShared::app_intake`], dstack #307).
const WRITER_APP: u8 = 3;

impl RxShared {
    /// The engine's pump claims the rings' writes (dstack #326, the normal
    /// profile; [`crate::stream::Pump::new`]): the [`Intake`] it drains
    /// before each block, unless the RX thread already writes them (its
    /// [`Receiver`](super::Receiver) was built first; then `None`, and
    /// nothing changes).
    pub fn pump_intake(self: &Arc<Self>) -> Option<Intake> {
        self.ring_writer
            .compare_exchange(
                WRITER_UNSET,
                WRITER_PUMP,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .ok()
            .map(|_| Intake::new(Arc::clone(self)))
    }

    /// Whether the engine's pump writes the rings ([`RxShared::pump_intake`]);
    /// else the RX thread or the app does.
    pub fn pump_writes(&self) -> bool {
        self.ring_writer.load(Ordering::Acquire) == WRITER_PUMP
    }

    /// The app claims the rings' writes (dstack #307): the [`Intake`] the
    /// thread with the deadline (an output callback, dlink's
    /// `bridge::RxPull`) drains just before it reads the rings by
    /// position, so neither the pump's lateness nor a helper thread's
    /// stands between a packet in its socket and the read. Claimed before
    /// the pump and the RX thread (`embed::Embedded::start_pulling`); `None`
    /// when one of them claimed first, and nothing changes.
    pub fn app_intake(self: &Arc<Self>) -> Option<Intake> {
        self.ring_writer
            .compare_exchange(
                WRITER_UNSET,
                WRITER_APP,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .ok()
            .map(|_| Intake::for_app(Arc::clone(self)))
    }

    /// Whether the app writes the rings ([`RxShared::app_intake`]).
    pub fn app_writes(&self) -> bool {
        self.ring_writer.load(Ordering::Acquire) == WRITER_APP
    }

    /// The RX thread claims the rings' writes: `true` when it is the first
    /// claim (the device's one RX thread); a pump's claim or another
    /// receiver's before it leaves this one without writes.
    pub(super) fn thread_writes(&self) -> bool {
        self.ring_writer
            .compare_exchange(
                WRITER_UNSET,
                WRITER_THREAD,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}
