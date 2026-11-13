// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The info server's log of the requests it does not answer (dstack #273).
//! Bitfocus Companion's Dante module asks every device opcode 0x100a once
//! a second; no Audinate device answers it, and neither do we. One INFO
//! line per request was ~86 000 lines a day per device. So the first
//! unanswered request of each (sender address, opcode) is logged once at
//! INFO, naming the sender, its peer id and the whole opcode; every request
//! is counted in [`Gaps`] (per peer id and opcode) and the counts are
//! logged as one INFO line once a [`WINDOW`] ([`Unanswered::flush`]), as
//! ARC's gaps are, with the address of each peer it names. At most
//! [`MAX_TOLD`] pairs get a line of their own; once that many were told,
//! the set is cleared at the next minute's report, so a new asker is named
//! again within a minute or two even after a burst of senders. A pair is
//! the sender's address and the request's opcode bytes 26–27
//! ([`info_op`]): requests that differ only in the opcode's other bytes
//! are one pair, its first line showing the first one's whole opcode. A
//! log only: what the server answers never depends on it.

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::time::Instant;

use tracing::info;

use crate::arc::gaps::{Gap, Gaps, Report, WINDOW};
use crate::ctl::info_op;

/// The (sender, opcode) pairs logged on their own line at most.
pub(crate) const MAX_TOLD: usize = 256;

/// The unanswered requests seen and counted.
#[derive(Debug)]
pub(crate) struct Unanswered {
    gaps: Gaps,
    told: BTreeSet<(IpAddr, u16)>,
}

impl Unanswered {
    pub fn new() -> Self {
        Self {
            gaps: Gaps::new(WINDOW),
            told: BTreeSet::new(),
        }
    }

    /// Counts one unanswered request `datagram` from `peer` at `now`: the
    /// first of its (sender, opcode) is logged at INFO; the window before
    /// it, when it has run its length, is logged as its report.
    pub fn record(&mut self, now: Instant, peer: IpAddr, datagram: &[u8]) {
        let op = info_op(datagram);
        let report = self.gaps.record(now, peer, op, Gap::Unanswered);
        self.log(report);
        if self.told.len() < MAX_TOLD && self.told.insert((peer, op)) {
            let opcode = datagram.get(24..32).map_or_else(String::new, |o| {
                o.iter().map(|b| format!("{b:02x}")).collect()
            });
            info!(
                op = %opcode,
                peer = self.gaps.peer_id(peer),
                from = %peer,
                "info request not answered (told once per sender and opcode, then counted a minute)"
            );
        }
    }

    /// Logs the window's report once it has run its length.
    pub fn flush(&mut self, now: Instant) {
        let report = self.gaps.flush(now);
        self.log(report);
    }

    /// A window's report as one INFO line, naming the address of each peer
    /// in it; a full told set is cleared with it.
    fn log(&mut self, report: Option<Report>) {
        let Some(report) = report else {
            return;
        };
        if self.told.len() >= MAX_TOLD {
            self.told.clear();
        }
        let mut ids: Vec<usize> = report.entries.iter().map(|e| e.peer).collect();
        ids.sort_unstable();
        ids.dedup();
        let peers: Vec<String> = ids
            .into_iter()
            .filter_map(|id| Some(format!("{id}={}", self.gaps.peer(id)?)))
            .collect();
        info!(
            requests = report.requests(),
            counts = %report,
            peers = %peers.join(","),
            "info requests not answered in a minute (peer id, opcode)"
        );
    }
}

impl Default for Unanswered {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
