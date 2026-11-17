// SPDX-License-Identifier: MIT OR Apache-2.0

//! The soak's TX pull cases (dstack #363): who fills the TX block, the TX
//! thread's one stall, the pulled path's own checks and its gate cases.
//! The path itself is `harness.rs`'s (`simulated_path(true)`: the app's
//! `TxSide` behind its own `Rated`, no RX side, filled by the TX thread's
//! `TxFill`), driven by [`Soak::run`].

use super::*;

/// Who fills the TX block (dstack #363).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tx {
    /// The pump, inside the app's processor (every app before #363).
    Pumped,
    /// The TX thread (`Link::start_tx_pulling`), on its own schedule, its
    /// wakes as late as the pump's (the field's: all inside the fill's
    /// catch-up reach).
    Pulled,
    /// [`Tx::Pulled`], one of whose passes comes [`TX_STALL_NS`] late (past
    /// W and the flows' lag bound: the fill starts again).
    PulledTxStall,
}

/// The TX thread's one stall in a [`Tx::PulledTxStall`] case, ns, and when
/// (after the bridges settled, before the 5 minutes whose rate is judged).
pub(crate) const TX_STALL_NS: u64 = 40_000_000;
pub(crate) const TX_STALL_AT_NS: u64 = 200 * SECOND;

impl Soak {
    /// The pulled path's own checks (dstack #363), from the fill's counts
    /// (status `tx_pull`) and the TX tone: on [`Tx::Pulled`] the pump's late
    /// wakes and its fault never reach the TX (no restart, no skip, no
    /// write after send, no fault, no tone break: the pump's path breaks
    /// the tone and resyncs at its fault); on [`Tx::PulledTxStall`] the
    /// fill starts again once, at the stall, and every tone break is
    /// there.
    pub(crate) fn check_tx_pull(&self, label: &str) {
        let Some(v) = self.fill_view else {
            assert_eq!(self.tx, Tx::Pumped, "{label}: no fill on a pulled path");
            return;
        };
        match self.tx {
            Tx::Pulled => {
                assert_eq!(
                    (v.rebootstraps, v.skipped, v.writes_after_send, v.faults),
                    (0, 0, 0, 0),
                    "{label}: the fill restarted, skipped, wrote after send or faulted: {v:?}"
                );
                assert!(
                    self.tone_break_at.is_empty(),
                    "{label}: the pulled TX broke at {:?} s",
                    self.gap_seconds(&self.tone_break_at)
                );
            }
            Tx::PulledTxStall => {
                assert_eq!(
                    (v.rebootstraps, v.writes_after_send, v.faults),
                    (1, 0, 0),
                    "{label}: one restart, at the stall: {v:?}"
                );
                assert!(v.skipped > 0, "{label}: the stall skipped no block: {v:?}");
                let before = self.before_tx_stall.expect("the TX thread stalled");
                let (from, to) = (before.at_ns, before.at_ns + TX_STALL_NS + TONE_GRACE_NS);
                assert!(
                    !self.tone_break_at.is_empty()
                        && self.tone_break_at.iter().all(|&at| at >= from && at <= to),
                    "{label}: the TX tone broke at {:?} s, the stall at {:.3} s",
                    self.gap_seconds(&self.tone_break_at),
                    (from - T0) as f64 / 1e9
                );
            }
            Tx::Pumped => panic!("{label}: a fill on the pump's path"),
        }
    }
}

/// The pump's late wakes (the field's, all inside W) and one wake past W
/// (8 blocks skipped), the TX side pulled: the TX tone never breaks, the
/// TX bridge never resyncs and the fill never restarts; the RX side as on
/// the pump's path.
#[test]
fn gate_ten_minutes_of_a_tx_pull_with_a_pump_rebootstrap_app_fast() {
    Soak::with_tx(6.0, true, Wakes::Field, Writer::Ticks, Tx::Pulled)
        .run(0x363_0001)
        .check();
}

#[test]
fn gate_ten_minutes_of_a_tx_pull_with_a_pump_rebootstrap_app_slow() {
    Soak::with_tx(-6.0, true, Wakes::Field, Writer::Ticks, Tx::Pulled)
        .run(0x363_0002)
        .check();
}

/// The TX thread itself stalls once past W and the flows' lag bound
/// (40 ms): the fill starts again once, the TX bridge resyncs once, and
/// the tone breaks only there, each break that resync's.
#[test]
fn gate_ten_minutes_of_a_tx_pull_with_a_tx_thread_stall() {
    Soak::with_tx(6.0, false, Wakes::Field, Writer::Ticks, Tx::PulledTxStall)
        .run(0x363_0003)
        .check();
}
