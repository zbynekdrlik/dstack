// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The socket past the qdisc across an interface re-create (dstack #414).

#[cfg(all(test, target_os = "linux"))]
use super::Direct;
#[cfg(test)]
use super::TxShared;

#[cfg(test)]
impl TxShared {
    /// This state with `direct` past the qdisc on the interface `nic`
    /// (tests: a stand-in, [`Direct::stand_in`]).
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
}
