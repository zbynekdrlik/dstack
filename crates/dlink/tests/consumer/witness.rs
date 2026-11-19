// SPDX-License-Identifier: MIT OR Apache-2.0

//! The consumer thread's witness (dstack #396): which side lost an RX
//! bridge's audio, the test's own consumer thread, the test's sender, or
//! the bridge. Shared by `tests/rx_pull.rs` (dlisten's output callback)
//! and dstack-asio's `tests/to_asio_pull.rs` (an ASIO driver's buffer
//! switch) through `#[path]`, beside `tests/tx_restart/restarts.rs`
//! (declared by each as `mod restarts`).
//!
//! In those tests a thread of the test pulls the bridge on an absolute
//! schedule (one period after the last due time) and plays what the
//! player gives. When the box does not run it, its late callback's pull
//! brings the stall's audio, the player skips the excess (by design), and
//! the callbacks it then makes back to back to keep its schedule run the
//! FIFO dry: the runner's, as a real card glitches there too (dstack
//! #307/#395: 2 underruns, ~2580 off, no ring hole, a clean sender). The
//! consumer loses that depth by its LATENESS against its schedule, which
//! can creep over several callbacks whose gaps each look short (the
//! writer witness's rule, `tests/writer/witness.rs`, dstack #376): a
//! callback later than its least lateness since the start (the timer's
//! steady oversleep) by more than a period is a stall. Before #396 a stall
//! was a gap over two periods, the same bound for a callback after an
//! on-time one.
//!
//! Samples off with no underrun are ring holes never written: packets
//! the test's sender never sent (`.claude/rules/tests.md`), most often its
//! TX schedule starting again after the scheduler did not run its TX
//! thread (dstack #401, PR #406's windows-2025 run 44293030766: 520 off,
//! 0 underruns, ring holes 512 never written, `verdict OURS`). The window
//! reads the ring's never-written holes and the sender's TX restarts with
//! their own lines' oversleep ([`super::restarts`]).
//!
//! Samples off with no underrun and no ring hole can be the sender's
//! pump: a pump restart after a wake more than W late leaves its engine
//! ring's positions unwritten, and its TX thread sends them as silence,
//! whole packets no RX counter sees (dstack #436, `tests/rx_pull_held.rs`:
//! plain at dev1 load ~24 that read `OURS` 8 runs of 9). The window also
//! reads the sender's pump restarts (all and late) and the samples its TX
//! lanes sent as silence (their `holes`), as `rx_pull_held`'s verdict does.
//!
//! [`verdict`]: `RUNNER` when every underrun follows a consumer stall
//! within the cause count, `SENDER` when the loss is never-written holes
//! or the silence of the sender's late pump restarts, every TX restart of
//! the sender in the window slept the flow's latency late, the holes are
//! no more than those restarts can leave unsent, every pump restart of
//! the sender was late and, with no underrun and that silence counted, the
//! samples off are within the holes, the silence and [`GAP_EDGES`] a gap;
//! both named when both hold, else `OURS`. A judged window is set aside
//! (kept for the message) only when it lost audio and its verdict is not
//! `OURS`, and the next one is judged after [`GUARD`], up to [`WINDOWS`];
//! the last is judged as it is. A window in which the consumer stalled
//! without a loss is judged, so the run stays short. A stall in the
//! settling starts the guard too. A window counts its underruns and holes
//! from before its first callback; its TX restarts from that callback's
//! end, so a restart just before the open whose holes land inside it reads
//! `OURS`. Its pump counts open [`SILENCE_DELAY`] before its first
//! callback: the silence is counted when the sender sends it and played
//! that much later, so a pump restart just before the open whose silence
//! plays inside it is counted. Two edges err the other way: the silence
//! sent within [`SILENCE_DELAY`] before the open that played before it is
//! counted too (at most that much silence), and a hole read before the
//! open and written late inside lowers the holes never written (at most
//! one restart's). The silence counted before the close that plays after
//! it only leaves the bound room.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dstack::media::tx::{MAX_FLOWS, TxShared};
use dstack::stream::PumpStats;

use super::restarts::{self, Restarts};

/// The judged windows a run may need: a window that lost audio to the
/// runner is set aside, any other is judged.
pub(crate) const WINDOWS: usize = 4;

/// The unjudged time after a window set aside: the callbacks a stall at
/// its close still makes back to back, and the player's re-priming after
/// their underrun, with room (dstack #395: without it that underrun
/// counted in the next window). A stall inside it starts it again.
pub(crate) const GUARD: Duration = Duration::from_millis(100);

/// The samples off the level a sender's gap may cost past its own samples
/// in a window with the silence of a late pump restart (dstack #436): the
/// player's converter rings at both edges of it. Measured under `chrt -f
/// 10` on dev1 (19.11.2026) at most 14 a pump restart (`rx_pull` 10340 off
/// for 10 restarts' 10240 samples of silence, `to_asio_pull` 10380;
/// `rx_pull_held` 10), so 32 is room, not a second cause. Only a gap that
/// left evidence has edges: a pump restart that sent silence, a TX restart
/// line beside holes never written. A window with holes and no pump
/// silence keeps #401's rule (the holes bounded by the restarts, the
/// samples off not): one 12 ms TX sleep's 640 holes cost 677 to 907 off.
pub(crate) const GAP_EDGES: u64 = 32;

/// How long after the sender sends a sample the consumer plays it, at
/// most: the receiver's latency and the FIFO's wait. Measured in
/// `rx_pull` (dstack #436, dev1 under `chrt -f 10`, 19.11.2026): the
/// receiver plays at 592 samples (12.3 ms) and the FIFO's mean wait is
/// ~14.3 ms, ~27 ms in all; `to_asio_pull`'s FIFO waits less (an 8 ms
/// target). With the counts taken at the first callback, a sender pump
/// restart ~25 ms before the open read 626 samples off past the window's
/// silence (9842 off, 9 late restarts, 9216 silence) and `OURS` in 2 runs
/// of 3. A window's pump counts open this long before its first callback.
pub(crate) const SILENCE_DELAY: Duration = Duration::from_millis(50);

/// The consumer's words in a verdict: its thread and its call, one and
/// several.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Names {
    pub(crate) thread: &'static str,
    pub(crate) call: &'static str,
    pub(crate) calls: &'static str,
}

/// What one callback played: samples at the level and off it, and the
/// FIFO's depth over `looks` looks, frames.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Played {
    pub(crate) on: u64,
    pub(crate) off: u64,
    pub(crate) depth: u64,
    pub(crate) looks: u64,
}

/// What the consumer saw over one judged window.
#[derive(Debug, Default)]
pub(crate) struct Window {
    /// Samples at the level and off it.
    pub(crate) on: u64,
    pub(crate) off: u64,
    /// The bridge's underruns between the window's open and close.
    pub(crate) underruns: u64,
    /// The FIFO's depth, frames, summed over the looks.
    pub(crate) depth: u64,
    pub(crate) looks: u64,
    /// The callbacks in the window.
    pub(crate) calls: u64,
    /// The latest callback past its due time, µs, and the consumer's least
    /// lateness since its start at the close (its timer's steady
    /// oversleep, which costs no depth).
    pub(crate) latest_us: u64,
    pub(crate) floor_us: u64,
    /// Every callback later than the floor by more than a period, with how
    /// late it was (µs past its due time).
    pub(crate) stalls: Vec<(u64, u64)>,
    /// The callbacks after which the underruns rose.
    pub(crate) underrun_at: Vec<u64>,
    /// The ring's holes never written in the window (holes less late).
    pub(crate) holes: u64,
    /// The sender's TX schedule restarts in the window.
    pub(crate) sender: Restarts,
    /// The sender's pump restarts in the window (dstack #436).
    pub(crate) pump: Pump,
}

/// The sender's pump in a judged window (dstack #436).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Pump {
    /// Its restarts, and those after a wake more than W late
    /// (`rebootstraps_late`).
    pub(crate) restarts: u64,
    pub(crate) late: u64,
    /// The samples its TX thread sent as silence: engine ring positions
    /// its pump left unwritten (the TX lanes' `holes`), which reach the
    /// receiver as whole packets, so no RX counter sees them.
    pub(crate) silence: u64,
}

/// The sending device a witness reads: its TX side, its pump and its rate.
pub(crate) struct Sender {
    pub(crate) tx: Arc<TxShared>,
    pub(crate) pump: Arc<PumpStats>,
    pub(crate) rate: u32,
}

/// The sender's counts at a window's open.
struct SenderAt {
    opened: restarts::Opened,
    pump: Pump,
}

impl Sender {
    /// Its pump's restarts and the silence its TX lanes sent, since its
    /// start (atomics and seqlocks, no lock).
    fn pump_now(&self) -> Pump {
        let view = self.pump.view();
        Pump {
            restarts: view.rebootstraps,
            late: view.rebootstraps_late,
            silence: (0..MAX_FLOWS).map(|s| self.tx.lane(s).holes).sum(),
        }
    }

    /// The counts at a window's open, its pump's as of `pump`
    /// ([`SILENCE_DELAY`] before).
    fn open(&self, pump: Pump) -> SenderAt {
        SenderAt {
            opened: restarts::open(&self.tx),
            pump,
        }
    }

    /// The TX and pump restarts since the window opened `at`.
    fn since(&self, at: &SenderAt) -> (Restarts, Pump) {
        let now = self.pump_now();
        let pump = Pump {
            restarts: now.restarts.saturating_sub(at.pump.restarts),
            late: now.late.saturating_sub(at.pump.late),
            silence: now.silence.saturating_sub(at.pump.silence),
        };
        (at.opened.close(&self.tx, self.rate), pump)
    }
}

/// Who lost a window's audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Who {
    /// The consumer thread was not run.
    Runner,
    /// The sender's TX thread was not run.
    Sender,
    /// Both.
    Both,
    /// The stack (or nothing explains it).
    Ours,
}

impl Window {
    /// Whether the window lost audio.
    pub(crate) fn lost(&self) -> bool {
        self.underruns > 0 || self.off > 0
    }

    /// The FIFO's mean depth over the window, ms, at `rate` Hz.
    pub(crate) fn mean_depth_ms(&self, rate: u32) -> f64 {
        self.depth as f64 / self.looks.max(1) as f64 * 1e3 / f64::from(rate)
    }

    /// The stalls at most `cause` callbacks before callback `at`.
    fn causes(&self, at: u64, cause: u64) -> impl Iterator<Item = &(u64, u64)> {
        self.stalls
            .iter()
            .filter(move |&&(end, _)| end <= at && at - end <= cause)
    }

    /// Who lost the window's audio: every underrun after a consumer stall
    /// within `cause` callbacks is the runner's; never-written ring holes
    /// are the sender's when every TX restart of its window slept the
    /// flow's latency late and they are no more than those restarts can
    /// leave unsent ([`Restarts::explain`]); the silence the sender's pump
    /// restarts sent is the sender's when every one of them was late
    /// (dstack #436). With that silence and no underrun the samples off
    /// must be within the holes and the silence, and [`GAP_EDGES`] a gap
    /// with evidence (holes alone keep #401's rule: the holes bounded, the
    /// samples off not). Samples off with neither, more off than the
    /// silence explains, a TX restart
    /// without that evidence or a pump restart that was not late, are
    /// ours.
    pub(crate) fn who(&self, cause: u64) -> Who {
        let consumer = self.underruns > 0;
        let pump = &self.pump;
        // The pump's silence counts only with a pump restart in the window
        // (silence with none is a block that ended past W: no evidence).
        let silence = if pump.restarts > 0 { pump.silence } else { 0 };
        let sender = self.holes > 0 || silence > 0;
        let underruns_explained = !consumer
            || (!self.underrun_at.is_empty()
                && self
                    .underrun_at
                    .iter()
                    .all(|&at| self.causes(at, cause).next().is_some()));
        let pump_gaps = if silence > 0 { pump.restarts } else { 0 };
        let tx_gaps = if self.holes > 0 {
            self.sender.lines.len() as u64
        } else {
            0
        };
        let edges = GAP_EDGES.saturating_mul(pump_gaps + tx_gaps);
        // The pump's silence bounds the samples off (holes alone keep
        // #401's rule, GAP_EDGES).
        let off_explained = consumer
            || silence == 0
            || self.off <= silence.saturating_add(self.holes).saturating_add(edges);
        let sender_explained = (self.holes == 0 || self.sender.explain(self.holes))
            && (!self.sender.any() || self.sender.runner())
            && pump.late == pump.restarts
            && off_explained;
        if !self.lost() || !(consumer || sender) || !underruns_explained || !sender_explained {
            return Who::Ours;
        }
        match (consumer, sender) {
            (true, true) => Who::Both,
            (true, false) => Who::Runner,
            _ => Who::Sender,
        }
    }
}

/// The verdict of a judged window that lost audio (module doc).
pub(crate) fn verdict(window: &Window, cause: u64, names: Names) -> String {
    let Names {
        thread,
        call,
        calls,
    } = names;
    let causes: Vec<String> = window
        .underrun_at
        .iter()
        .map(|&at| {
            let before: Vec<String> = window
                .causes(at, cause)
                .map(|&(end, us)| format!("{us} us late, {} {calls} before", at - end))
                .collect();
            format!("underrun at {call} {at}: {thread} stalls {before:?}")
        })
        .collect();
    let who = match window.who(cause) {
        Who::Runner => format!("RUNNER (the {thread} was not run)"),
        Who::Sender => "SENDER (the sender's pump or TX thread was not run)".to_string(),
        Who::Both => {
            format!("RUNNER (the {thread} was not run; the sender's pump or TX thread neither)")
        }
        Who::Ours => format!("OURS{}", window.sender.ours()),
    };
    let pump = &window.pump;
    format!(
        "verdict {who}: {causes:?}, {} samples off, ring holes never written {}, the sender's \
         pump {} restarts ({} late), {} samples sent as silence, the sender's {:?}",
        window.off, window.holes, pump.restarts, pump.late, pump.silence, window.sender
    )
}

/// How late a callback due at `due` woke, µs: read right after its sleep.
pub(crate) fn late_us(due: Instant) -> u64 {
    u64::try_from(Instant::now().saturating_duration_since(due).as_micros()).unwrap_or(u64::MAX)
}

/// Drops the counts of `seen` (oldest first) older than the latest at
/// least `delay` before `now`, so the first is that one (or the oldest,
/// while none is that old).
fn keep_delayed<T>(seen: &mut VecDeque<(Instant, T)>, now: Instant, delay: Duration) {
    let Some(cutoff) = now.checked_sub(delay) else {
        return;
    };
    while seen.get(1).is_some_and(|&(at, _)| at <= cutoff) {
        seen.pop_front();
    }
}

/// An open window: when, and the counts at its open.
struct Open {
    start: Instant,
    underruns: u64,
    holes: u64,
    sender: Option<SenderAt>,
}

/// The consumer's witness: the consumer thread calls [`Witness::called`]
/// after each callback; it opens, judges and sets aside the windows.
pub(crate) struct Witness<H> {
    stall_us: u64,
    cause: u64,
    judged: Duration,
    /// The ring's never-written holes so far, and as of the last callback.
    holes: H,
    holes_seen: u64,
    /// The sending device's TX side, pump and rate.
    sender: Option<Sender>,
    /// The sender's pump counts at the recent callbacks, oldest first: the
    /// first is the latest at least [`SILENCE_DELAY`] old (or the oldest).
    pumps: VecDeque<(Instant, Pump)>,
    /// The consumer's least lateness since its start, µs.
    floor_us: Option<u64>,
    /// The underruns as of the last callback, and as last recorded.
    counted: u64,
    seen: u64,
    open: Option<Open>,
    guard_until: Option<Instant>,
    window: Window,
    aside: Vec<Window>,
}

impl<H: FnMut() -> u64> Witness<H> {
    /// The witness of a consumer every `period` whose underrun is its
    /// stall's within `cause` callbacks, judging windows of `judged`; the
    /// ring's never-written holes from `holes`, the sender's TX and pump
    /// restarts from `sender` (`None`: no sender read). The bridge has
    /// counted `underruns` so far.
    pub(crate) fn new(
        period: Duration,
        cause: u64,
        judged: Duration,
        underruns: u64,
        mut holes: H,
        sender: Option<Sender>,
    ) -> Self {
        Witness {
            stall_us: u64::try_from(period.as_micros()).unwrap_or(u64::MAX),
            cause,
            judged,
            holes_seen: holes(),
            holes,
            sender,
            pumps: VecDeque::new(),
            floor_us: None,
            counted: underruns,
            seen: underruns,
            open: None,
            guard_until: None,
            window: Window::default(),
            aside: Vec::new(),
        }
    }

    /// Notes the sender's pump counts at a callback done at `now`, and
    /// keeps the latest at least [`SILENCE_DELAY`] old as the first.
    fn note_pump(&mut self, now: Instant) {
        let Some(sender) = &self.sender else {
            return;
        };
        self.pumps.push_back((now, sender.pump_now()));
        keep_delayed(&mut self.pumps, now, SILENCE_DELAY);
    }

    /// The stall bound past the floor, µs: a period.
    pub(crate) fn stall_us(&self) -> u64 {
        self.stall_us
    }

    /// One callback done at `now`: it woke `late_us` past its due time,
    /// the bridge has counted `underruns` after it, and it `played` this;
    /// `judging` once the run's settling is over. The judged window and
    /// the windows set aside once the run is judged.
    pub(crate) fn called(
        &mut self,
        now: Instant,
        late_us: u64,
        underruns: u64,
        judging: bool,
        played: Played,
    ) -> Option<(Window, Vec<Window>)> {
        let floor_us = self.floor_us.map_or(late_us, |f| f.min(late_us));
        self.floor_us = Some(floor_us);
        self.note_pump(now);
        let stalled = late_us - floor_us > self.stall_us;
        // The underruns and the ring's never-written holes after this
        // callback, and before it.
        let prior = std::mem::replace(&mut self.counted, underruns);
        let holes = (self.holes)();
        let holes_prior = std::mem::replace(&mut self.holes_seen, holes);
        if !judging {
            // A stall in the settling: its catch-up underrun must not land
            // in the first window.
            if stalled {
                self.guard_until = Some(now + GUARD);
            }
            return None;
        }
        if self.guard_until.is_some_and(|until| now < until) {
            if stalled {
                self.guard_until = Some(now + GUARD);
            }
            return None;
        }
        if self.open.is_none() {
            // A window starts: its underruns and holes are counted from
            // before this callback, whose samples it counts.
            self.seen = prior;
            self.window = Window::default();
            self.open = Some(Open {
                start: now,
                underruns: prior,
                holes: holes_prior,
                sender: self
                    .sender
                    .as_ref()
                    .zip(self.pumps.front())
                    .map(|(s, &(_, p))| s.open(p)),
            });
        }
        let window = &mut self.window;
        window.calls += 1;
        let at = window.calls;
        window.latest_us = window.latest_us.max(late_us);
        if stalled {
            window.stalls.push((at, late_us));
        }
        if underruns > self.seen {
            window.underrun_at.push(at);
            self.seen = underruns;
        }
        let open = self.open.as_ref().expect("a window is open");
        if now.saturating_duration_since(open.start) <= self.judged {
            window.on += played.on;
            window.off += played.off;
            window.depth += played.depth;
            window.looks += played.looks;
            return None;
        }
        let open = self.open.take().expect("a window is open");
        let mut window = std::mem::take(&mut self.window);
        window.underruns = underruns - open.underruns;
        window.floor_us = floor_us;
        window.holes = holes.saturating_sub(open.holes);
        if let (Some(at), Some(sender)) = (open.sender, self.sender.as_ref()) {
            (window.sender, window.pump) = sender.since(&at);
        }
        if !window.lost() || window.who(self.cause) == Who::Ours || self.aside.len() + 1 == WINDOWS
        {
            return Some((window, std::mem::take(&mut self.aside)));
        }
        // The runner lost this window's audio: set aside.
        self.aside.push(window);
        self.guard_until = Some(now + GUARD);
        None
    }
}

#[cfg(test)]
mod consumer_tests {
    use super::*;
    use crate::restarts::Restart;

    const NAMES: Names = Names {
        thread: "output thread",
        call: "callback",
        calls: "callbacks",
    };
    const CAUSE: u64 = 4;
    const STALL: &[(u64, u64)] = &[(40, 30_000)];

    fn window(underruns: u64, off: u64, stalls: &[(u64, u64)], underrun_at: &[u64]) -> Window {
        Window {
            off,
            underruns,
            stalls: stalls.to_vec(),
            underrun_at: underrun_at.to_vec(),
            sender: Restarts {
                rate: 48_000,
                ..Restarts::default()
            },
            ..Window::default()
        }
    }

    /// `counted` TX restarts with lines of these oversleeps (µs), each 470
    /// samples late on a 9 ms latency (432 samples at 48 kHz): up to 902
    /// samples unsent each.
    fn sender(counted: u64, oversleeps: &[Option<u64>]) -> Restarts {
        let mut restarts = crate::restarts::fixture(counted, oversleeps);
        for line in &mut restarts.lines {
            *line = Restart { lag: 470, ..*line };
        }
        restarts
    }

    fn says(w: &Window) -> String {
        verdict(w, CAUSE, NAMES)
    }

    #[test]
    fn an_underrun_after_a_consumer_stall_is_the_runner_s() {
        assert!(says(&window(1, 2578, STALL, &[40])).starts_with("verdict RUNNER (the output"));
        // The last callback the stall still explains, and the first it
        // does not; an underrun before the stall, or with none.
        assert_eq!(window(1, 1, STALL, &[40 + CAUSE]).who(CAUSE), Who::Runner);
        assert_eq!(window(1, 1, STALL, &[41 + CAUSE]).who(CAUSE), Who::Ours);
        assert_eq!(window(1, 1, STALL, &[39]).who(CAUSE), Who::Ours);
        assert_eq!(window(1, 1, &[], &[40]).who(CAUSE), Who::Ours);
        // One underrun of two without a stall before it.
        assert_eq!(window(2, 1, STALL, &[40, 90]).who(CAUSE), Who::Ours);
        // Each after a stall of its own.
        let two = &[(40, 30_000), (90, 25_000)];
        assert_eq!(window(2, 1, two, &[41, 92]).who(CAUSE), Who::Runner);
        assert_eq!(window(2, 1, two, &[41, 95]).who(CAUSE), Who::Ours);
        // No loss at all is never a verdict to set aside.
        assert_eq!(window(0, 0, STALL, &[]).who(CAUSE), Who::Ours);
    }

    #[test]
    fn never_written_holes_after_a_late_tx_sleep_are_the_sender_s() {
        // PR #406's run 44293030766: 520 off, no underrun, 512 holes.
        let mut w = window(0, 520, &[], &[]);
        w.holes = 512;
        assert_eq!(w.who(CAUSE), Who::Ours, "holes with no TX restart");
        w.sender = sender(1, &[Some(9_282)]);
        assert!(says(&w).starts_with("verdict SENDER"), "{}", says(&w));
        // A restart whose sleep was on time, unmeasured, or a counted one
        // without a line (a re-anchor): ours.
        for s in [
            sender(1, &[Some(500)]),
            sender(1, &[None]),
            sender(2, &[Some(9_282)]),
        ] {
            w.sender = s;
            assert_eq!(w.who(CAUSE), Who::Ours, "{:?}", w.sender);
        }
        // Samples off with neither holes nor an underrun.
        let mut w = window(0, 520, &[], &[]);
        w.sender = sender(1, &[Some(9_282)]);
        assert_eq!(w.who(CAUSE), Who::Ours);
        // Both: an underrun after a stall and holes after a late sleep.
        let mut w = window(1, 3000, STALL, &[41]);
        w.holes = 512;
        w.sender = sender(1, &[Some(9_282)]);
        assert_eq!(w.who(CAUSE), Who::Both);
        // A late-sleep restart never covers an underrun on time.
        let mut w = window(1, 3000, &[], &[41]);
        w.holes = 512;
        w.sender = sender(1, &[Some(9_282)]);
        assert_eq!(w.who(CAUSE), Who::Ours);
        // A TX restart without a late sleep spoils a consumer stall's case.
        let mut w = window(1, 3000, STALL, &[41]);
        w.sender = sender(1, &[Some(500)]);
        assert_eq!(w.who(CAUSE), Who::Ours);
        // More holes than the late restarts may cost (902 each): packets
        // dropped beside them.
        let mut w = window(0, 1804, &[], &[]);
        w.sender = sender(2, &[Some(9_282), Some(9_282)]);
        w.holes = 1804;
        assert_eq!(w.who(CAUSE), Who::Sender);
        w.holes = 1805;
        assert_eq!(w.who(CAUSE), Who::Ours);
        // Samples off with a consumer stall but no underrun and no hole.
        assert_eq!(window(0, 520, STALL, &[]).who(CAUSE), Who::Ours);
    }

    /// dstack #426: never-written holes after a sender restart bound under
    /// its receiver's latency are the stack's (the sender gave up packets
    /// the receiver still plays), however late the sleep, and named so.
    #[test]
    fn holes_after_a_restart_under_the_receiver_s_latency_are_ours() {
        let mut w = window(0, 520, &[], &[]);
        w.holes = 512;
        w.sender = sender(1, &[Some(9_282)]);
        w.sender.receiver_lag = vec![433];
        assert_eq!(w.who(CAUSE), Who::Ours, "{:?}", w.sender);
        assert!(
            says(&w)
                .starts_with("verdict OURS (a TX restart bound under the receiver's latency): "),
            "{}",
            says(&w)
        );
        w.sender.receiver_lag = vec![432];
        assert_eq!(w.who(CAUSE), Who::Sender);
    }

    /// `restarts` pump restarts of the sender, `late` of them late,
    /// `silence` samples its TX lanes sent as silence.
    fn pump(restarts: u64, late: u64, silence: u64) -> Pump {
        Pump {
            restarts,
            late,
            silence,
        }
    }

    /// dstack #436: samples off with no underrun and no ring hole after
    /// the sender's late pump restarts are the sender's, within the
    /// silence they sent.
    #[test]
    fn samples_off_within_a_late_sender_pump_restart_s_silence_are_the_sender_s() {
        // A 20 ms sender pump stall every 300 blocks: 10 late restarts,
        // 10240 samples sent as silence, no ring hole.
        let mut w = window(0, 10_340, &[], &[]);
        assert_eq!(w.who(CAUSE), Who::Ours, "no sender evidence");
        w.pump = pump(10, 10, 10_240);
        assert_eq!(w.who(CAUSE), Who::Sender);
        assert!(
            says(&w).starts_with("verdict SENDER (the sender's pump or TX thread was not run): "),
            "{}",
            says(&w)
        );
        assert!(
            says(&w).contains("pump 10 restarts (10 late), 10240 samples sent as silence"),
            "{}",
            says(&w)
        );
        // The silence and GAP_EDGES a restart, and one sample past them.
        w.off = 10_560;
        assert_eq!(w.who(CAUSE), Who::Sender);
        w.off = 10_561;
        assert_eq!(w.who(CAUSE), Who::Ours);
        // One restart of the ten not late (a clock step back, a
        // re-anchor).
        w.off = 10_340;
        w.pump = pump(10, 9, 10_240);
        assert_eq!(w.who(CAUSE), Who::Ours);
        // Silence with no pump restart (a block that ended past W), and a
        // late restart that sent none: no gap, no edges.
        w.pump = pump(0, 0, 10_240);
        assert_eq!(w.who(CAUSE), Who::Ours);
        let mut one = window(0, 1, &[], &[]);
        one.pump = pump(1, 1, 0);
        assert_eq!(one.who(CAUSE), Who::Ours);
        one.pump = pump(1, 1, 1_024);
        one.off = 1_056;
        assert_eq!(one.who(CAUSE), Who::Sender);
        one.off = 1_057;
        assert_eq!(one.who(CAUSE), Who::Ours);
        // An underrun on time beside the pump's silence: ours; after a
        // consumer stall: both; a pump restart that was not late spoils
        // the stall's case.
        let mut w = window(1, 3000, &[], &[41]);
        w.pump = pump(1, 1, 1_024);
        assert_eq!(w.who(CAUSE), Who::Ours);
        let mut w = window(1, 3000, STALL, &[41]);
        w.pump = pump(1, 1, 1_024);
        assert_eq!(w.who(CAUSE), Who::Both);
        assert!(
            says(&w).starts_with(
                "verdict RUNNER (the output thread was not run; the sender's pump or TX thread neither)"
            ),
            "{}",
            says(&w)
        );
        w.pump = pump(1, 0, 1_024);
        assert_eq!(w.who(CAUSE), Who::Ours);
        // A late pump restart beside a TX restart without a late sleep.
        let mut w = window(0, 1_024, &[], &[]);
        w.pump = pump(1, 1, 1_024);
        w.sender = sender(1, &[Some(500)]);
        assert_eq!(w.who(CAUSE), Who::Ours);
    }

    /// With no underrun and a late pump restart's silence, samples off past
    /// the sender's holes, that silence and their edges have another cause
    /// beside them (dstack #436, as `rx_pull_held`'s rule); holes alone
    /// keep #401's rule.
    #[test]
    fn samples_off_past_the_sender_s_gaps_and_their_edges_are_ours() {
        // 640 holes never written after one 12 ms TX sleep: 907 off read
        // SENDER (measured, rx_pull under `chrt -f 10`, 19.11.2026).
        let mut w = window(0, 907, &[], &[]);
        w.holes = 640;
        w.sender = sender(1, &[Some(12_010)]);
        assert_eq!(w.who(CAUSE), Who::Sender);
        // Holes and a late pump restart's silence together: 512 + 1024
        // and an edge for each gap.
        w.holes = 512;
        w.pump = pump(1, 1, 1_024);
        w.off = 1_600;
        assert_eq!(w.who(CAUSE), Who::Sender);
        w.off = 1_601;
        assert_eq!(w.who(CAUSE), Who::Ours);
        // With an underrun after a stall, the runner's loss is not
        // bounded by the sender's gaps.
        let mut w = window(1, 30_000, STALL, &[41]);
        w.holes = 512;
        w.sender = sender(1, &[Some(9_282)]);
        assert_eq!(w.who(CAUSE), Who::Both);
    }

    #[test]
    fn a_window_s_pump_counts_open_the_silence_s_delay_before_it() {
        // A callback every 10 ms: at 100 ms the first kept is the one at
        // 50 ms (exactly the delay old), at 105 ms still that one (the next,
        // at 60 ms, is 45 ms old), at 110 ms the one at 60 ms.
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut seen = VecDeque::new();
        for ms in (0..=100).step_by(10) {
            seen.push_back((at(ms), ms));
            keep_delayed(&mut seen, at(ms), SILENCE_DELAY);
        }
        assert_eq!(seen.front().map(|&(_, ms)| ms), Some(50), "{seen:?}");
        assert_eq!(seen.len(), 6, "{seen:?}");
        keep_delayed(&mut seen, at(105), SILENCE_DELAY);
        assert_eq!(seen.front().map(|&(_, ms)| ms), Some(50), "{seen:?}");
        seen.push_back((at(110), 110));
        keep_delayed(&mut seen, at(110), SILENCE_DELAY);
        assert_eq!(seen.front().map(|&(_, ms)| ms), Some(60), "{seen:?}");
        // While none is that old, the oldest stays first.
        let mut young = VecDeque::from([(at(0), 0), (at(10), 10)]);
        keep_delayed(&mut young, at(20), SILENCE_DELAY);
        assert_eq!(young.front().map(|&(_, ms)| ms), Some(0), "{young:?}");
    }

    /// A witness of a 10 ms consumer judging 95 ms windows (a window opens
    /// at a callback and closes at the 11th, 100 ms on), with no ring or
    /// sender read.
    fn witness() -> Witness<impl FnMut() -> u64> {
        Witness::new(
            Duration::from_millis(10),
            CAUSE,
            Duration::from_millis(95),
            0,
            || 0,
            None,
        )
    }

    /// Runs callback `i` at `t0` + 10 ms * i + its lateness, each a
    /// (lateness µs, underruns after it) pair, the list padded with copies
    /// of its last callback to `len`, until the witness returns.
    fn run(calls: &[(u64, u64)], len: usize) -> Option<(Window, Vec<Window>)> {
        let mut w = witness();
        let t0 = Instant::now();
        let last = calls.last().copied().unwrap_or_default();
        let played = Played {
            on: 480,
            ..Played::default()
        };
        let padded = calls
            .iter()
            .copied()
            .chain(std::iter::repeat(last))
            .take(len);
        padded.enumerate().find_map(|(i, (late, underruns))| {
            let now = t0 + Duration::from_millis(10 * i as u64) + Duration::from_micros(late);
            w.called(now, late, underruns, true, played)
        })
    }

    #[test]
    fn creeping_lateness_under_two_periods_a_gap_is_a_stall() {
        // Every callback 8 ms later than the last: each gap 18 ms (under
        // the old bound of two periods), the lateness 8, 16, 24 ms past a
        // floor of 0; the underrun right after it reads RUNNER and the
        // window is set aside, the next (after the guard) judged.
        let creeping: [(u64, u64); 5] = [(0, 0), (8_000, 0), (16_000, 0), (24_000, 0), (0, 1)];
        let gaps: Vec<u64> = creeping
            .windows(2)
            .map(|p| (10_000 + p[1].0).saturating_sub(p[0].0))
            .collect();
        assert!(gaps.iter().all(|&g| g < 20_000), "{gaps:?}");
        let (judged, aside) = run(&creeping, 40).expect("judged");
        assert_eq!(aside.len(), 1, "{aside:?}");
        let first = &aside[0];
        assert_eq!(first.stalls, [(3, 16_000), (4, 24_000)], "{first:?}");
        assert_eq!(first.underrun_at, [5], "{first:?}");
        assert_eq!(first.who(CAUSE), Who::Runner, "{first:?}");
        assert!(verdict(first, CAUSE, NAMES).starts_with("verdict RUNNER (the output thread"));
        assert_eq!(
            (judged.underruns, judged.stalls.len()),
            (0, 0),
            "{judged:?}"
        );
    }

    #[test]
    fn a_timer_s_steady_oversleep_is_no_stall() {
        // Every wake 3 ms late (the floor): 12 ms is 9 ms past it, within
        // a period; 14 ms is 11 ms past it, a stall.
        let calls = [(3_000, 0), (12_000, 0), (3_000, 0), (14_000, 0), (3_000, 0)];
        let (judged, aside) = run(&calls, 12).expect("judged");
        assert!(aside.is_empty(), "{aside:?}");
        assert_eq!(judged.floor_us, 3_000, "{judged:?}");
        assert_eq!(judged.stalls, [(4, 14_000)], "{judged:?}");
    }

    #[test]
    fn a_window_is_set_aside_only_when_it_would_fail_runner() {
        // A stall with no loss: judged at once.
        let (judged, aside) = run(&[(0, 0), (25_000, 0), (0, 0)], 12).expect("judged");
        assert!(aside.is_empty(), "{aside:?}");
        assert_eq!(
            (judged.stalls.len(), judged.lost()),
            (1, false),
            "{judged:?}"
        );
        // An underrun with the consumer on time: judged at once (OURS).
        let (judged, aside) = run(&[(0, 0), (0, 1)], 12).expect("judged");
        assert!(aside.is_empty(), "{aside:?}");
        assert_eq!((judged.underruns, judged.who(CAUSE)), (1, Who::Ours));
        // An underrun after a stall: set aside, then a clean window after
        // the guard is judged.
        let (judged, aside) = run(&[(0, 0), (25_000, 0), (0, 1)], 40).expect("judged");
        assert_eq!(aside.len(), 1, "{aside:?}");
        assert_eq!(
            (aside[0].underruns, aside[0].underrun_at.as_slice()),
            (1, &[3][..])
        );
        assert_eq!(
            (judged.underruns, judged.stalls.len()),
            (0, 0),
            "{judged:?}"
        );
        // Every window lost to the runner: the last is judged as it is.
        let mut calls = Vec::new();
        for n in 1..=WINDOWS as u64 {
            calls.extend([(0, n - 1), (25_000, n - 1), (0, n)]);
            calls.extend(std::iter::repeat_n((0, n), 17));
        }
        let (judged, aside) = run(&calls, calls.len()).expect("judged");
        assert_eq!(aside.len(), WINDOWS - 1, "{aside:?}");
        assert_eq!(
            (judged.underruns, judged.who(CAUSE)),
            (1, Who::Runner),
            "{judged:?}"
        );
    }

    /// One callback at `ms` after `t0`, `late` µs late, the bridge at
    /// `underruns` after it.
    fn at(
        w: &mut Witness<impl FnMut() -> u64>,
        t0: Instant,
        ms: u64,
        late: u64,
        underruns: u64,
        judging: bool,
    ) -> Option<(Window, Vec<Window>)> {
        let now = t0 + Duration::from_millis(ms);
        w.called(now, late, underruns, judging, Played::default())
    }

    #[test]
    fn a_stall_in_the_guard_restarts_it_and_a_window_counts_from_before_its_first_callback() {
        let t0 = Instant::now();
        let mut w = witness();
        // A window lost to a stall: set aside at 100 ms, guarded to 200.
        assert!(at(&mut w, t0, 0, 0, 0, true).is_none());
        assert!(at(&mut w, t0, 10, 25_000, 0, true).is_none());
        for ms in (20..=100).step_by(10) {
            assert!(at(&mut w, t0, ms, 0, 1, true).is_none());
        }
        // A stall at 150 ms restarts the guard to 250: 210 is unjudged.
        assert!(at(&mut w, t0, 150, 25_000, 1, true).is_none());
        assert!(at(&mut w, t0, 210, 0, 1, true).is_none());
        // The next window opens at 250 on a callback that underran: the
        // underrun is its first callback's.
        assert!(at(&mut w, t0, 250, 0, 2, true).is_none());
        let judged = (260..=350)
            .step_by(10)
            .find_map(|ms| at(&mut w, t0, ms, 0, 2, true));
        let (judged, aside) = judged.expect("judged at 350 ms");
        assert_eq!(aside.len(), 1, "{aside:?}");
        assert_eq!(
            (
                judged.underruns,
                judged.underrun_at.as_slice(),
                judged.calls
            ),
            (1, &[1][..], 11),
            "{judged:?}"
        );
    }

    #[test]
    fn a_stall_in_the_settling_guards_the_first_window() {
        let t0 = Instant::now();
        let mut w = witness();
        // Settling: a stall at 10 ms, its catch-up underrun at 20 ms (now
        // judged, but inside the guard to 110 ms).
        assert!(at(&mut w, t0, 0, 0, 0, false).is_none());
        assert!(at(&mut w, t0, 10, 25_000, 0, false).is_none());
        assert!(at(&mut w, t0, 20, 0, 1, true).is_none());
        let judged = (30..=300)
            .step_by(10)
            .find_map(|ms| at(&mut w, t0, ms, 0, 1, true));
        let (judged, aside) = judged.expect("judged");
        assert!(aside.is_empty(), "{aside:?}");
        assert_eq!((judged.underruns, judged.lost()), (0, false), "{judged:?}");
    }
}
