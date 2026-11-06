// SPDX-License-Identifier: MIT OR Apache-2.0

//! TX's send ahead before a bounded escape (dstack #1,
//! `DeviceConfig::spin_escape_ms`, a Windows trial): right after a pass, the
//! packets due within [`clock::PRESEND_NS`] go out now, each flow in order
//! up to its first packet whose samples are not there yet, so the one-tick
//! block that follows holds back nothing a receiver needs meanwhile.

use std::time::Instant;

use super::{Plan, Ring, Source, Transmitter, clock, send_clock};

/// The rounds [`Transmitter::presend_all`] makes at most: each covers the
/// send time of the one before.
pub const PRESEND_ROUNDS: usize = 4;

/// What a send ahead did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Presend {
    /// Packets sent ahead.
    pub sent: u64,
    /// Every packet due within the window went out: none waits for its
    /// samples, and the clock did not re-anchor since the pass.
    pub covered: bool,
}

impl Transmitter {
    /// Before a bounded escape (right after a [`Transmitter::step`] at the
    /// clock's `epoch`): every packet due within [`clock::PRESEND_NS`] of
    /// PTP `now_ns`, the longest the escape's block lasts, sent now, each
    /// flow in order up to its first packet whose samples are not there yet
    /// ([`Source::ready`]); counted in the lanes' `early`. Nothing is sent
    /// when the clock is not ready or re-anchored since the pass (the next
    /// pass realigns first): not covered.
    pub fn presend(&mut self, now_ns: Option<u64>, epoch: u32) -> Presend {
        let pass = Instant::now();
        let (Some(now_ns), true) = (now_ns, self.epoch == Some(epoch)) else {
            return Presend {
                sent: 0,
                covered: false,
            };
        };
        let now = send_clock(now_ns, self.rate, self.margin_ns);
        let horizon = send_clock(
            now_ns.saturating_add(clock::PRESEND_NS),
            self.rate,
            self.margin_ns,
        );
        self.send_due(now, now_ns, pass, Some(horizon))
    }

    /// [`Transmitter::presend`] from a fresh clock read (`read`: PTP ns and
    /// the clock's epoch, read in that order) again while it sends anything,
    /// at most [`PRESEND_ROUNDS`] times, so the window also covers the send
    /// ahead's own time (all but the last round's, when every round sent):
    /// `true` when no round left a packet due within it. `false`: put the
    /// escape off (`clock::defer_escape`), a packet held through the block
    /// could pass its latency and restart its flow; also when the clock
    /// re-anchored since the pass.
    pub fn presend_all(&mut self, mut read: impl FnMut() -> (Option<u64>, u32)) -> bool {
        for _ in 0..PRESEND_ROUNDS {
            let (now_ns, epoch) = read();
            let done = self.presend(now_ns, epoch);
            if !done.covered {
                return false;
            }
            if done.sent == 0 {
                break;
            }
        }
        true
    }

    /// Before the TX thread's wait (after a pass): when `ask` says the wait
    /// will escape (`clock::ask_escape`), the send ahead
    /// ([`Transmitter::presend_all`] on the clock's `read`); when that left
    /// a packet due in the window, or the clock re-anchored, `defer` puts
    /// the escape off (`clock::defer_escape`, `false` when it is overdue
    /// and escapes anyway). Whether the wait escapes.
    pub fn before_wait(
        &mut self,
        read: impl FnMut() -> (Option<u64>, u32),
        ask: impl FnOnce() -> bool,
        defer: impl FnOnce() -> bool,
    ) -> bool {
        ask() && (self.presend_all(read) || !defer())
    }
}

/// Whether every channel of `plan`'s packet stamped `at` has its samples
/// there ([`Source::ready`] with the TX channels' `sources`, their `engine`
/// rings, read on their line with the engine shift `engine_shift`, and
/// the RX `rings`); a channel mapped to no TX channel sends silence and
/// waits for nothing.
pub(super) fn packet_ready(
    plan: &Plan,
    sources: &[Source],
    engine: &[Option<Ring>],
    rings: &[Ring],
    at: u64,
    engine_shift: i64,
) -> bool {
    let fpp = usize::from(plan.fpp);
    plan.channels.iter().take(usize::from(plan.nch)).all(|&id| {
        usize::from(id).checked_sub(1).is_none_or(|i| {
            let own = engine.get(i).and_then(Option::as_ref);
            sources.get(i).is_none_or(|source| {
                let at = super::shift::source_position(source, at, engine_shift);
                source.ready(at, fpp, rings, own)
            })
        })
    })
}
