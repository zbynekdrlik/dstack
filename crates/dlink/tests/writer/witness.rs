// SPDX-License-Identifier: MIT OR Apache-2.0

//! The writer thread's witness (dstack #376): which side lost a TX bridge's
//! audio, the test's own writer thread or the stack. Shared by
//! `tests/tx_pull.rs`, `tests/tx_pull_counts.rs` and dstack-asio's
//! `tests/from_asio_pull.rs` through `#[path]`.
//!
//! In those tests a thread of the test writes the bridge's FIFO on its own
//! clock (dsend's capture, OBS's mix, an ASIO driver's buffer switch) and
//! the stack's TX thread takes it. When the box does not run the writer,
//! the FIFO runs dry and the player counts an underrun: the runner's, not
//! the stack's (v0.6.166 main CI on windows-2025: 1 underrun, 390 samples
//! off, 0 TX holes, 0 writes after send). The writer records its own
//! scheduling gaps on the app's clock (`dstack::clock::app_monotonic_ns`):
//! a gap over its period plus the bridge's room ([`room_ns`]) is a stall,
//! a write late enough to run the FIFO dry.
//!
//! The rule is `dstack-asio/tests/to_asio_pull.rs`'s (#307): a judged
//! window in which the writer stalled is set aside (kept for the message)
//! and the next one is judged, up to [`WINDOWS`]; the last is judged as it
//! is, so a run whose every window stalled still fails. The asserts on the
//! judged window are the test's own and unchanged (the stack's counts, TX
//! holes and writes after send, stay strict). A failure prints
//! [`verdict`]: `RUNNER` when every bridge underrun follows a writer stall
//! within [`CAUSE_WRITES`] and the stack's own counts are 0, else `OURS`.

use std::sync::Mutex;
use std::time::Duration;

use dlink::bridge::AppBuffer;

/// The judged windows a run may need: one whose writer never stalled is
/// judged, a stalled one is set aside.
pub(crate) const WINDOWS: usize = 4;

/// How many writes after a writer stall an underrun is its own: the late
/// write itself (the writer reads the count after its write) and the
/// writes it then makes back to back to keep its schedule.
pub(crate) const CAUSE_WRITES: u64 = 4;

/// How late a write may land before the FIFO can run dry, ns: the player
/// keeps the depth at the target, a writer of `chunk_ns` swings it half a
/// chunk under it, and the TX thread takes a block at once
/// (`AppBuffer::short`'s least): 5 ms for a 10 ms writer at a 12.667 ms
/// target (block 128 at 48 kHz), 2.667 ms for a 256-frame ASIO switch at
/// 8 ms.
pub(crate) fn room_ns(buffer: &AppBuffer) -> u64 {
    buffer
        .clamped_ns()
        .saturating_sub(buffer.chunk_ns() / 2 + buffer.block_ns())
}

/// What the writer saw over one judged window.
#[derive(Debug, Default)]
pub(crate) struct Window {
    /// The writes in the window.
    pub(crate) writes: u64,
    /// The largest gap between two writes, µs of the app's clock.
    pub(crate) largest_gap_us: u64,
    /// Every gap over the stall bound, with the write it ended at.
    pub(crate) stalls: Vec<(u64, u64)>,
    /// The write after which each rise of the bridge's underruns was read.
    pub(crate) underrun_at: Vec<u64>,
}

impl Window {
    /// Whether the writer stalled in the window (it is then set aside).
    pub(crate) fn stalled(&self) -> bool {
        !self.stalls.is_empty()
    }

    /// The stalls that end at most [`CAUSE_WRITES`] before write `at`.
    fn causes(&self, at: u64) -> impl Iterator<Item = &(u64, u64)> {
        self.stalls
            .iter()
            .filter(move |&&(end, _)| end <= at && at - end <= CAUSE_WRITES)
    }

    /// Whether the window's loss is the runner's: the bridge underran
    /// (`underruns`, its count over the window), the writer read every
    /// underrun, each after a stall of its own, and the stack's own counts
    /// (`ours`) are all 0. Samples off with no underrun are never the
    /// writer's.
    pub(crate) fn runner(&self, underruns: u64, ours: &[(&str, u64)]) -> bool {
        underruns > 0
            && ours.iter().all(|&(_, n)| n == 0)
            && self.underrun_at.len() as u64 >= underruns
            && self
                .underrun_at
                .iter()
                .all(|&at| self.causes(at).next().is_some())
    }
}

/// The verdict of a judged window that lost audio (module doc).
pub(crate) fn verdict(window: &Window, underruns: u64, ours: &[(&str, u64)]) -> String {
    let causes: Vec<String> = window
        .underrun_at
        .iter()
        .map(|&at| {
            let before: Vec<String> = window
                .causes(at)
                .map(|&(end, us)| format!("{us} us, {} writes before", at - end))
                .collect();
            format!("underrun at write {at}: writer stalls {before:?}")
        })
        .collect();
    let who = if window.runner(underruns, ours) {
        "RUNNER (the writer thread was not run)"
    } else {
        "OURS"
    };
    format!(
        "verdict {who}: {underruns} underruns, {} read by the writer {causes:?}, the stack's counts {ours:?}",
        window.underrun_at.len()
    )
}

/// The record the writer keeps.
#[derive(Debug, Default)]
struct State {
    /// The last write's time, ns of the app's clock.
    last_ns: Option<i64>,
    /// The underruns the last write read.
    seen: Option<u64>,
    /// Whether a window is open, and its record.
    open: bool,
    window: Window,
}

/// The writer's witness: the writer calls [`Witness::wrote`] after each
/// write, [`judge`] opens and closes the windows.
#[derive(Debug)]
pub(crate) struct Witness {
    stall_us: u64,
    state: Mutex<State>,
}

impl Witness {
    /// The witness of a writer every `period` into a bridge FIFO of
    /// `buffer`: a stall is a gap over the period and [`room_ns`].
    pub(crate) fn new(period: Duration, buffer: &AppBuffer) -> Self {
        let period_us = u64::try_from(period.as_micros()).unwrap_or(u64::MAX);
        Witness {
            stall_us: period_us + room_ns(buffer) / 1_000,
            state: Mutex::default(),
        }
    }

    /// The stall bound, µs: a gap over it is a stall.
    pub(crate) fn stall_us(&self) -> u64 {
        self.stall_us
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// One write done; `underruns` is the bridge's count read after it.
    pub(crate) fn wrote(&self, underruns: u64) {
        let now = dstack::clock::app_monotonic_ns();
        let mut state = self.state();
        let gap_us = state
            .last_ns
            .map(|last| u64::try_from(now - last).unwrap_or(0) / 1_000);
        state.last_ns = Some(now);
        let risen = state.seen.is_some_and(|seen| underruns > seen);
        state.seen = Some(underruns);
        if !state.open {
            return;
        }
        let stall_us = self.stall_us;
        let window = &mut state.window;
        window.writes += 1;
        let at = window.writes;
        if let Some(gap) = gap_us {
            window.largest_gap_us = window.largest_gap_us.max(gap);
            if gap > stall_us {
                window.stalls.push((at, gap));
            }
        }
        if risen {
            window.underrun_at.push(at);
        }
    }

    fn open(&self) {
        let mut state = self.state();
        state.window = Window::default();
        state.open = true;
    }

    fn close(&self) -> Window {
        let mut state = self.state();
        state.open = false;
        std::mem::take(&mut state.window)
    }
}

/// Runs judged windows (`run` measures one and returns its counts) until
/// one whose writer never stalled, at most [`WINDOWS`]: the judged one, its
/// writer's record, and the windows set aside.
pub(crate) fn judge<T>(
    witness: &Witness,
    mut run: impl FnMut() -> T,
) -> (T, Window, Vec<(T, Window)>) {
    let mut aside = Vec::new();
    loop {
        witness.open();
        let judged = run();
        let window = witness.close();
        if !window.stalled() || aside.len() + 1 == WINDOWS {
            return (judged, window, aside);
        }
        aside.push((judged, window));
    }
}

#[cfg(test)]
mod witness_tests {
    use super::*;

    fn window(stalls: &[(u64, u64)], underrun_at: &[u64]) -> Window {
        Window {
            writes: 100,
            largest_gap_us: stalls.iter().map(|&(_, us)| us).max().unwrap_or(0),
            stalls: stalls.to_vec(),
            underrun_at: underrun_at.to_vec(),
        }
    }

    const CLEAN: &[(&str, u64)] = &[("TX holes", 0), ("writes after send", 0)];

    #[test]
    fn an_underrun_after_a_writer_stall_is_the_runner_s() {
        let w = window(&[(40, 26_000)], &[40]);
        assert!(w.runner(1, CLEAN));
        assert!(verdict(&w, 1, CLEAN).starts_with("verdict RUNNER"));
        // The last write the stall still explains, and the first it does not.
        assert!(window(&[(40, 26_000)], &[40 + CAUSE_WRITES]).runner(1, CLEAN));
        assert!(!window(&[(40, 26_000)], &[41 + CAUSE_WRITES]).runner(1, CLEAN));
        // An underrun read before the stall ended is not its.
        assert!(!window(&[(40, 26_000)], &[39]).runner(1, CLEAN));
    }

    #[test]
    fn a_loss_with_the_writer_on_time_is_ours() {
        let w = window(&[], &[40]);
        assert!(!w.runner(1, CLEAN));
        assert!(verdict(&w, 1, CLEAN).starts_with("verdict OURS"));
        // Samples off with no underrun: no writer explains them.
        assert!(!window(&[(40, 26_000)], &[]).runner(0, CLEAN));
        // An underrun the writer never read (counted after its last write).
        assert!(!window(&[(40, 26_000)], &[40]).runner(2, CLEAN));
        // One underrun of two without a stall before it.
        assert!(!window(&[(40, 26_000)], &[40, 90]).runner(2, CLEAN));
        // A stack count is never the writer's.
        assert!(!window(&[(40, 26_000)], &[40]).runner(1, &[("TX holes", 1)]));
    }

    #[test]
    fn the_room_is_the_target_less_half_a_chunk_and_a_block() {
        // dsend's 10 ms writer on a 48 kHz device (block 128: 2.667 ms).
        let buffer = AppBuffer::new(10_000_000, 2_666_667);
        assert_eq!(room_ns(&buffer), 5_000_000);
        let witness = Witness::new(Duration::from_millis(10), &buffer);
        assert_eq!(witness.stall_us(), 15_000);
    }

    #[test]
    fn a_stalled_window_is_set_aside_and_the_last_judged() {
        let witness = Witness::new(Duration::from_millis(10), &AppBuffer::new(10_000_000, 0));
        // Every window stalls (a write after a 20 ms gap): all but the
        // last set aside.
        let mut n = 0;
        let (judged, window, aside) = judge(&witness, || {
            witness.wrote(0);
            std::thread::sleep(Duration::from_millis(20));
            witness.wrote(n);
            n += 1;
            n
        });
        assert_eq!((judged, aside.len()), (WINDOWS as u64, WINDOWS - 1));
        assert!(window.stalled() && aside.iter().all(|(_, w)| w.stalled()));
        // A window with no stall is judged at once.
        let (_, window, aside) = judge(&witness, || witness.wrote(n));
        assert_eq!(
            (window.stalled(), aside.len(), window.writes),
            (false, 0, 1)
        );
    }
}
