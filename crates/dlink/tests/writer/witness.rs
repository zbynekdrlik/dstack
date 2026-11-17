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
//! off, 0 TX holes, 0 writes after send). The writer keeps an absolute
//! schedule, so how far each write landed past its due time, less its
//! least lateness since its start (the timer's steady oversleep: the
//! player keeps the depth on the writes' real line), is how much depth
//! the FIFO lost to the writer; a write later than that floor by more
//! than the bridge's room ([`room_ns`]) is a stall, late enough to run
//! the FIFO dry. The writer
//! reads the bridge's underruns after each write under the witness's lock,
//! and a window's underruns are read there too at its open and close, so
//! the record and the count are of one moment.
//!
//! The rule is `dstack-asio/tests/to_asio_pull.rs`'s (#307): a judged
//! window in which the writer stalled (or was still stalled at its close)
//! is set aside (kept for the message) and the next one is judged, up to
//! [`WINDOWS`]; the last is judged as it is, so a run whose every window
//! stalled still fails. A window set aside loses only its audio: a writer
//! stall never makes TX holes, writes after send or a fill restart, so the
//! tests assert those over every window, set aside or judged. Between
//! windows [`GUARD`] passes unjudged, so a set-aside window's silence
//! still on its way to the receiver never counts in the next. A failure
//! prints [`verdict`]: `RUNNER` when every bridge underrun follows a writer
//! stall within [`CAUSE_WRITES`] and the stack's own counts are 0, else
//! `OURS`.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use dlink::bridge::{AppBuffer, Stats};

/// The judged windows a run may need: one whose writer never stalled is
/// judged, a stalled one is set aside.
pub(crate) const WINDOWS: usize = 4;

/// The unjudged time between two windows: the path to the receiver (its
/// latency, about 10 ms here) and the player's re-priming after an
/// underrun, with room.
pub(crate) const GUARD: Duration = Duration::from_millis(100);

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
    /// The bridge's underruns between the window's open and close.
    pub(crate) underruns: u64,
    /// The latest write past its due time, µs, and the writer's least
    /// lateness since its start at the close (its timer's steady
    /// oversleep, which costs no depth: the player keeps the depth on the
    /// writes' real line).
    pub(crate) latest_us: u64,
    pub(crate) floor_us: u64,
    /// Every write later than the floor by more than the room, with how
    /// late it was (µs, past its due time); a stall still running at the
    /// close is the write after the last.
    pub(crate) stalls: Vec<(u64, u64)>,
    /// The write after which the underruns rose, and by how many.
    pub(crate) underrun_at: Vec<(u64, u64)>,
}

impl Window {
    /// Whether the writer stalled in the window (it is then set aside).
    pub(crate) fn stalled(&self) -> bool {
        !self.stalls.is_empty()
    }

    /// The stalls at most [`CAUSE_WRITES`] before write `at`.
    fn causes(&self, at: u64) -> impl Iterator<Item = &(u64, u64)> {
        self.stalls
            .iter()
            .filter(move |&&(end, _)| end <= at && at - end <= CAUSE_WRITES)
    }

    /// Whether the window's loss is the runner's: the bridge underran, the
    /// writer read every underrun (or the ones it did not read yet wait on
    /// a write still stalled at the close), each after a stall of its own,
    /// and the stack's own counts (`ours`) are all 0. Samples off with no
    /// underrun are never the writer's.
    pub(crate) fn runner(&self, ours: &[(&str, u64)]) -> bool {
        let read: u64 = self.underrun_at.iter().map(|&(_, n)| n).sum();
        let pending = self
            .stalls
            .last()
            .is_some_and(|&(at, _)| at == self.writes + 1);
        self.underruns > 0
            && ours.iter().all(|&(_, n)| n == 0)
            && (read >= self.underruns || pending)
            && self
                .underrun_at
                .iter()
                .all(|&(at, _)| self.causes(at).next().is_some())
    }
}

/// The verdict of a judged window that lost audio (module doc).
pub(crate) fn verdict(window: &Window, ours: &[(&str, u64)]) -> String {
    let causes: Vec<String> = window
        .underrun_at
        .iter()
        .map(|&(at, n)| {
            let before: Vec<String> = window
                .causes(at)
                .map(|&(end, us)| format!("{us} us late, {} writes before", at - end))
                .collect();
            format!("{n} underruns read at write {at}: writer stalls {before:?}")
        })
        .collect();
    let who = if window.runner(ours) {
        "RUNNER (the writer thread was not run)"
    } else {
        "OURS"
    };
    format!(
        "verdict {who}: {} underruns {causes:?}, the stack's counts {ours:?}",
        window.underruns
    )
}

/// The record the writer keeps.
#[derive(Debug, Default)]
struct State {
    /// The last write's due time.
    due: Option<Instant>,
    /// The writer's least lateness since its start, µs.
    floor_us: Option<u64>,
    /// The underruns the last write read, and at the window's open.
    seen: u64,
    opened: u64,
    /// Whether a window is open, and its record.
    open: bool,
    window: Window,
}

/// The writer's witness: the writer calls [`Witness::wrote`] after each
/// write, [`judge`] opens and closes the windows.
#[derive(Debug)]
pub(crate) struct Witness {
    period: Duration,
    stall_us: u64,
    stats: Arc<Stats>,
    state: Mutex<State>,
}

impl Witness {
    /// The witness of a writer every `period` into a bridge FIFO of
    /// `buffer` whose player counts in `stats`: a stall is a write later
    /// than [`room_ns`].
    pub(crate) fn new(period: Duration, buffer: &AppBuffer, stats: Arc<Stats>) -> Self {
        Witness {
            period,
            stall_us: room_ns(buffer) / 1_000,
            state: Mutex::new(State {
                seen: stats.underruns(),
                ..State::default()
            }),
            stats,
        }
    }

    /// The stall bound, µs: a write later than it is a stall.
    pub(crate) fn stall_us(&self) -> u64 {
        self.stall_us
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// One write done, due at `due` on the writer's schedule.
    pub(crate) fn wrote(&self, due: Instant) {
        let late_us = u64::try_from(due.elapsed().as_micros()).unwrap_or(u64::MAX);
        let mut state = self.state();
        let underruns = self.stats.underruns();
        let risen = underruns.saturating_sub(state.seen);
        state.seen = underruns;
        state.due = Some(due);
        let floor_us = state.floor_us.map_or(late_us, |f| f.min(late_us));
        state.floor_us = Some(floor_us);
        if !state.open {
            return;
        }
        let stall_us = self.stall_us;
        let window = &mut state.window;
        window.writes += 1;
        let at = window.writes;
        window.latest_us = window.latest_us.max(late_us);
        if late_us - floor_us > stall_us {
            window.stalls.push((at, late_us));
        }
        if risen > 0 {
            window.underrun_at.push((at, risen));
        }
    }

    fn open(&self) {
        let mut state = self.state();
        state.window = Window::default();
        state.opened = self.stats.underruns();
        state.seen = state.opened;
        state.open = true;
    }

    fn close(&self) -> Window {
        let mut state = self.state();
        state.open = false;
        let mut window = std::mem::take(&mut state.window);
        window.underruns = self.stats.underruns().saturating_sub(state.opened);
        let floor_us = state.floor_us.unwrap_or(0);
        window.floor_us = floor_us;
        // The next write, not landed yet, already later than the room.
        if let Some(late_us) = state
            .due
            .and_then(|due| Instant::now().checked_duration_since(due + self.period))
            .map(|late| u64::try_from(late.as_micros()).unwrap_or(u64::MAX))
            .filter(|&us| us.saturating_sub(floor_us) > self.stall_us)
        {
            window.stalls.push((window.writes + 1, late_us));
        }
        window
    }
}

/// Runs judged windows (`run` measures one and returns its counts) until
/// one whose writer never stalled, at most [`WINDOWS`], with [`GUARD`]
/// between them: the judged one, its writer's record, and the windows set
/// aside.
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
        std::thread::sleep(GUARD);
    }
}

#[cfg(test)]
mod witness_tests {
    use super::*;

    fn window(underruns: u64, stalls: &[(u64, u64)], underrun_at: &[(u64, u64)]) -> Window {
        Window {
            writes: 100,
            underruns,
            latest_us: stalls.iter().map(|&(_, us)| us).max().unwrap_or(0),
            floor_us: 0,
            stalls: stalls.to_vec(),
            underrun_at: underrun_at.to_vec(),
        }
    }

    const CLEAN: &[(&str, u64)] = &[("TX holes", 0), ("writes after send", 0)];
    const STALL: &[(u64, u64)] = &[(40, 30_000)];

    #[test]
    fn an_underrun_after_a_writer_stall_is_the_runner_s() {
        let w = window(1, STALL, &[(40, 1)]);
        assert!(w.runner(CLEAN));
        assert!(verdict(&w, CLEAN).starts_with("verdict RUNNER"));
        // Two underruns read in one rise.
        assert!(window(2, STALL, &[(40, 2)]).runner(CLEAN));
        // The last write the stall still explains, and the first it does not.
        assert!(window(1, STALL, &[(40 + CAUSE_WRITES, 1)]).runner(CLEAN));
        assert!(!window(1, STALL, &[(41 + CAUSE_WRITES, 1)]).runner(CLEAN));
        // An underrun read before the stall is not its.
        assert!(!window(1, STALL, &[(39, 1)]).runner(CLEAN));
    }

    #[test]
    fn a_loss_with_the_writer_on_time_is_ours() {
        let w = window(1, &[], &[(40, 1)]);
        assert!(!w.runner(CLEAN));
        assert!(verdict(&w, CLEAN).starts_with("verdict OURS"));
        // Samples off with no underrun: no writer explains them.
        assert!(!window(0, STALL, &[]).runner(CLEAN));
        // An underrun the writer never read.
        assert!(!window(2, STALL, &[(40, 1)]).runner(CLEAN));
        // One underrun of two without a stall before it.
        assert!(!window(2, STALL, &[(40, 1), (90, 1)]).runner(CLEAN));
        // An unread underrun waits on a write still stalled at the close
        // (write 101 of 100): the runner's; one stalled earlier is no cover.
        assert!(window(2, &[(40, 30_000), (101, 20_000)], &[(40, 1)]).runner(CLEAN));
        assert!(!window(2, &[(40, 30_000), (100, 20_000)], &[(40, 1)]).runner(CLEAN));
        // A stack count is never the writer's.
        assert!(!window(1, STALL, &[(40, 1)]).runner(&[("TX holes", 1)]));
    }

    #[test]
    fn the_room_is_the_target_less_half_a_chunk_and_a_block() {
        // dsend's 10 ms writer on a 48 kHz device (block 128: 2.667 ms).
        let buffer = AppBuffer::new(10_000_000, 2_666_667);
        assert_eq!(room_ns(&buffer), 5_000_000);
        let witness = Witness::new(
            Duration::from_millis(10),
            &buffer,
            Arc::new(Stats::default()),
        );
        assert_eq!(witness.stall_us(), 5_000);
    }

    /// A witness of a 10 ms writer with a 5 ms room, its floor set by a
    /// write on time before any window.
    fn witness() -> Witness {
        let witness = Witness::new(
            Duration::from_millis(10),
            &AppBuffer::new(10_000_000, 0),
            Arc::new(Stats::default()),
        );
        witness.wrote(Instant::now());
        witness
    }

    #[test]
    fn a_stalled_window_is_set_aside_and_the_last_judged() {
        // Every window has a write 20 ms past its due time: all but the
        // last set aside.
        let late = witness();
        let mut n = 0;
        let (judged, window, aside) = judge(&late, || {
            late.wrote(Instant::now() - Duration::from_millis(20));
            n += 1;
            n
        });
        assert_eq!((judged, aside.len()), (WINDOWS, WINDOWS - 1));
        assert!(window.stalled() && aside.iter().all(|(_, w)| w.stalled()));
        assert_eq!(window.stalls[0].0, 1);
        // A window whose write is on time is judged at once (a fresh
        // witness: no earlier write whose next is due).
        let on_time = witness();
        let (_, window, aside) = judge(&on_time, || {
            on_time.wrote(Instant::now() + Duration::from_secs(1))
        });
        assert_eq!(
            (window.stalled(), aside.len(), window.writes),
            (false, 0, 1)
        );
    }

    #[test]
    fn a_timer_s_steady_oversleep_is_no_stall() {
        // A writer whose every wake is 3 ms late: 7 ms late is 4 ms past
        // its floor (within the 5 ms room), 9 ms late is 6 ms past it.
        let witness = Witness::new(
            Duration::from_millis(10),
            &AppBuffer::new(10_000_000, 0),
            Arc::new(Stats::default()),
        );
        let ago = |ms| Instant::now() - Duration::from_millis(ms);
        witness.wrote(ago(3));
        let (_, window, aside) = judge(&witness, || witness.wrote(ago(7)));
        assert!(!window.stalled() && aside.is_empty(), "{window:?}");
        assert!(window.floor_us >= 3_000, "{window:?}");
        let (_, window, _) = judge(&witness, || witness.wrote(ago(9)));
        assert_eq!(window.stalls.first().map(|s| s.0), Some(1), "{window:?}");
    }

    #[test]
    fn a_write_still_late_at_the_close_is_a_stall() {
        // The last write was due 20 ms ago: the next, due 10 ms ago, is
        // 10 ms late at the close and not landed.
        let late = witness();
        let (_, window, _) = judge(&late, || {
            late.wrote(Instant::now() - Duration::from_millis(20));
        });
        assert_eq!(window.stalls.len(), 2, "{window:?}");
        assert_eq!(window.stalls[1].0, 2, "{window:?}");
    }
}
