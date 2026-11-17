// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The TX pull's watch (dstack #363), the pump watch's twin
//! ([`crate::stream::watch::PumpWatch`]): once a second, from the daemon's
//! tick (never the TX thread), one warning line when the TX pull's fill
//! started again, skipped blocks, wrote a block after some of its stamps
//! were sent, or its processor panicked in that second, with the counts
//! that grew and the panic's message. The TX thread only counts.

use tracing::warn;

use super::{TxFillStats, TxFillView};
use crate::media::late::EVERY_NS;
use crate::media::tx::TxShared;

/// The text of the TX pull watch's warning: the line a consumer greps for.
pub const TX_PULL_WARNING: &str = "TX pull restarted, skipped or faulted this second";

/// The watch's state: the counts at the last look and when the next is
/// due.
#[derive(Debug, Default)]
pub struct FillWatch {
    seen: TxFillView,
    next_ns: Option<u64>,
}

impl FillWatch {
    pub const fn new() -> Self {
        FillWatch {
            seen: TxFillView {
                blocks: 0,
                rebootstraps: 0,
                skipped: 0,
                writes_after_send: 0,
                faults: 0,
            },
            next_ns: None,
        }
    }

    /// At `now_ns`, a look at `view` once [`EVERY_NS`] has passed since
    /// the last (the first only learns): what grew since, when a
    /// restart, a skip, a write after send or a fault did (`blocks` is
    /// the blocks filled since); `None` otherwise.
    pub fn look(&mut self, now_ns: u64, view: TxFillView) -> Option<TxFillView> {
        if self.next_ns.is_some_and(|next| now_ns < next) {
            return None;
        }
        let first = self.next_ns.is_none();
        self.next_ns = Some(now_ns.saturating_add(EVERY_NS));
        let before = std::mem::replace(&mut self.seen, view);
        let grew = TxFillView {
            blocks: view.blocks.saturating_sub(before.blocks),
            rebootstraps: view.rebootstraps.saturating_sub(before.rebootstraps),
            skipped: view.skipped.saturating_sub(before.skipped),
            writes_after_send: view
                .writes_after_send
                .saturating_sub(before.writes_after_send),
            faults: view.faults.saturating_sub(before.faults),
        };
        let lost = grew.rebootstraps + grew.skipped + grew.writes_after_send + grew.faults;
        (!first && lost > 0).then_some(grew)
    }

    /// [`FillWatch::look`] at `tx`'s TX pull (none: nothing), logged.
    pub fn watch(&mut self, now_ns: u64, tx: &TxShared) {
        let Some(stats) = tx.tx_fill_stats() else {
            return;
        };
        self.watch_stats(now_ns, &tx.cfg.name, stats);
    }

    /// [`FillWatch::watch`] on the counts `stats` of the device `device`.
    pub fn watch_stats(&mut self, now_ns: u64, device: &str, stats: &TxFillStats) {
        let Some(grew) = self.look(now_ns, stats.view()) else {
            return;
        };
        let fault = (grew.faults > 0).then(|| stats.fault()).flatten();
        warn!(
            device,
            rebootstraps = grew.rebootstraps,
            skipped = grew.skipped,
            writes_after_send = grew.writes_after_send,
            faults = grew.faults,
            blocks = grew.blocks,
            fault = fault.as_deref().unwrap_or("-"),
            "{TX_PULL_WARNING}"
        );
    }
}

#[cfg(test)]
mod tests;
