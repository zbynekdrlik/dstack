// SPDX-License-Identifier: MIT OR Apache-2.0

//! The RX bridge read by its consumer (dstack #307). Step one: the API
//! only; the pump still writes the FIFO.

use std::sync::Arc;

use super::Fifo;

/// The RX side of a bridge, pulled by the thread with the deadline.
pub struct RxPull {
    _fifo: Arc<Fifo>,
}

impl RxPull {
    pub(crate) fn new(fifo: Arc<Fifo>) -> Self {
        RxPull { _fifo: fifo }
    }

    /// Brings the FIFO up to the media clock's now; returns the frames
    /// written.
    pub fn pull(&mut self) -> usize {
        0
    }
}
