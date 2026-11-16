// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which thread lost the real-time soak's audio (dstack #339): the test's
//! app thread (its tick writes the TX FIFO and reads the RX FIFO) or the
//! pump (it reads the TX FIFO and writes the RX FIFO), each not run long
//! enough by the box to lose it, or neither: then the loss is ours. A run
//! that lost audio fails either way; the verdict only says whether a rerun
//! can pass.
//!
//! What each thread can cost, from the bridges' design (`bridge::player`):
//! - A TX underrun: the TX FIFO ran dry between the depth read at the
//!   tick's start and the tick's write. The pump runs on the app's clock
//!   (module doc of `soak_real_time.rs`), so it never takes frames ahead of
//!   real time: the FIFO's depth runs dry no earlier than its depth less
//!   one converter call ([`CALL_MS`]) after the read, unless the pump
//!   already owed blocks at the read (a backlog it then catches up at
//!   once). So the app's write must have landed that late, the pump's
//!   backlog at the read counted in. And the depth read must be the usual
//!   one (at least the run's median less a call: the sawtooth's bottom
//!   moves by a call with the calls' phase), less what the pump took on
//!   time while this tick ran late: the tick's lateness past the run's
//!   median lateness (the median depth is read at the median lateness). A
//!   FIFO short of that lost a write or gave frames it never had, which no
//!   thread's lateness explains. The pump's backlog at the read never
//!   lowers it (a pump behind leaves the FIFO fuller); it enters the dry
//!   point only.
//! - A TX re-centre: the pump was not run while the app kept writing, until
//!   the depth passed the target by the player's hard excess.
//! - An RX dropout: the pump was not run long enough to leave less than a
//!   tick's read in the RX FIFO (the depth before a read normally holds a
//!   tick and some spare: the run's median less [`CHUNK`]). The pump only
//!   adds to that FIFO, so a dropout whose depth before the read covered
//!   the read (a tick and a call) no pump stall explains.
//! - A pump restart (a wake more than W late) explains an RX dropout, never
//!   a TX re-centre: its skipped blocks make the TX player resync to the
//!   target (counted apart, `resyncs`), which takes the excess away.
//!
//! A pump stall is read from its late wakes (the pump records each wake one
//! block or more behind its due time, with its wall time): it was not run
//! from at most `behind` before the wake until the wake. The app thread's
//! hold is where its write landed (`Fifo::last_write`, the app's clock)
//! against the moment the depth before it was read. Evidence shows a thread
//! was late (not run, or blocked), not why: a stack lock that blocks the
//! pump reads like the box. The rules lean to OURS: a pump lag under one
//! block (1.33 ms) is never recorded as a late wake, so an app hold within
//! about that much plus [`SLACK_MS`] of the dry point can read OURS.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use super::harness::{APP_RATE, CHUNK};
use super::{LATE_TICK, Tick, held};

/// One converter call of the TX player, ms (`dlink::bridge::player::BLOCK`
/// output frames, at the app's rate on both sides).
pub(crate) const CALL_MS: f64 = dlink::bridge::player::BLOCK as f64 * 1e3 / APP_RATE as f64;
/// The app's tick, ms.
pub(crate) const CHUNK_MS: f64 = CHUNK as f64 * 1e3 / APP_RATE as f64;
/// The slack of a comparison in ms: the wall time of a late wake is whole
/// ms, and the converter's call varies by its ratio.
pub(crate) const SLACK_MS: f64 = 1.0;

/// One late pump wake: its wall time (whole ms since the Unix epoch), how
/// far behind its due time it was (µs), and whether the pump restarted.
pub(crate) type Wake = (u64, u64, bool);

/// The pump's late wakes, merged as the test's ticks see them: the status
/// keeps the last 32 only.
#[derive(Debug, Default)]
pub(crate) struct Wakes {
    pub(crate) seen: BTreeSet<Wake>,
    /// The count the last merge read (`WakeLateness::late_wakes`).
    pub(crate) count: u64,
}

impl Wakes {
    /// Merges the pump's late wakes: `count` since the start, `recent` the
    /// ones the status keeps.
    pub(crate) fn merge(&mut self, count: u64, recent: impl Iterator<Item = Wake>) {
        if count > self.count {
            self.count = count;
            self.seen.extend(recent);
        }
    }

    /// The late wakes no merge saw: left the status's list between two
    /// merges, or skipped while the pump wrote their slot (about: two
    /// wakes alike in all three fields count as one).
    pub(crate) fn lost(&self) -> u64 {
        self.count.saturating_sub(self.seen.len() as u64)
    }

    /// The stalls (wall ms: start, end) that overlap `from..=to` (ms).
    fn stalls(&self, from: f64, to: f64) -> impl Iterator<Item = (f64, f64, &Wake)> {
        self.seen.iter().filter_map(move |w| {
            let end = w.0 as f64;
            let start = end - w.1 as f64 / 1e3;
            (start <= to + SLACK_MS && end + SLACK_MS >= from).then_some((start, end, w))
        })
    }
}

/// What a loss needed and what the threads did around it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Loss {
    pub(crate) tick: usize,
    pub(crate) what: &'static str,
    /// The thread that explains it, if any.
    pub(crate) runner: Option<String>,
    /// The threads' record around it, shown either way.
    pub(crate) around: String,
}

/// The TX re-centre's stall: the depth before the tick's write `depth_ms`
/// plus the tick it writes must pass the target `target_ms` by the excess
/// `excess_ms` (`bridge::player::hard_excess_s`).
pub(crate) fn recentre_stall_ms(target_ms: f64, excess_ms: f64, depth_ms: f64) -> f64 {
    target_ms + excess_ms - (depth_ms + CHUNK_MS)
}

/// The losses the run's rules do not allow (`gap_on_time`, `rx_twice_late`,
/// and the RX dropouts at ticks on time past `restarts`), each judged
/// against the run's usual depths (the medians before a write and a read,
/// and the median lateness they were read at); `target_ms` and `excess_ms`
/// are the TX bridge's.
pub(crate) fn losses(
    ticks: &[Tick],
    wakes: &Wakes,
    restarts: u64,
    target_ms: f64,
    excess_ms: f64,
) -> Vec<Loss> {
    let rx_median_ms = median(ticks.iter().map(|t| t.rx_depth_ms));
    let tx_median_ms = median(ticks.iter().map(|t| t.tx_depth_ms));
    let late_median_ms = median(ticks.iter().map(late_ms));
    let on_time_rx: u64 = (0..ticks.len())
        .filter(|&i| !held(ticks, i))
        .map(|i| ticks[i].rx)
        .sum();
    let mut out = Vec::new();
    for (i, t) in ticks.iter().enumerate() {
        let from = if i == 0 {
            t.top_ms
        } else {
            ticks[i - 1].end_ms
        } as f64;
        let to = t.end_ms as f64;
        let on_time = t.late < LATE_TICK;
        let usual_ms = usual_tx_depth_ms(tx_median_ms, late_median_ms, late_ms(t));
        let tx = [
            (on_time && t.underrun).then(|| ("TX underrun", tx_underrun(t, wakes, usual_ms))),
            (on_time && t.recentre).then(|| {
                let need = recentre_stall_ms(target_ms, excess_ms, t.tx_depth_ms);
                ("TX re-centre", pump_stall(wakes, from, to, need, false))
            }),
        ];
        for (what, runner) in tx.into_iter().flatten() {
            out.push(Loss {
                tick: i,
                what,
                runner,
                around: format!(
                    "{} (the run's usual TX depth {tx_median_ms:.2} ms at its median lateness \
                     {late_median_ms:.2} ms; {usual_ms:.2} ms at this tick's)",
                    around(t, wakes, from, to)
                ),
            });
        }
        let judged = if held(ticks, i) {
            t.rx > 1
        } else {
            t.rx > 0 && on_time_rx > restarts
        };
        if judged {
            let need = rx_median_ms - CHUNK_MS;
            // A read the FIFO covered: the pump only adds to it.
            let covered = t.rx_depth_ms >= CHUNK_MS + CALL_MS + SLACK_MS;
            out.push(Loss {
                tick: i,
                what: "RX dropout",
                runner: pump_stall(wakes, from, to, need, true).filter(|_| !covered),
                around: around(t, wakes, from, to),
            });
        }
    }
    out
}

/// A tick's lateness, ms.
fn late_ms(t: &Tick) -> f64 {
    t.late.as_secs_f64() * 1e3
}

/// The TX depth a tick `late_ms` late should read before its write: the
/// run's median `median_ms` (read at the median lateness `late_median_ms`)
/// less what the pump took on time over the tick's lateness past that
/// median (dstack #339: a tick 3.55 ms late read 8.88 ms where the median
/// was 14.85 ms, and its write was held 10.99 ms). A tick earlier than the
/// median reads no more than the median: the rule never tightens.
pub(crate) fn usual_tx_depth_ms(median_ms: f64, late_median_ms: f64, late_ms: f64) -> f64 {
    median_ms - (late_ms - late_median_ms).max(0.0)
}

/// A TX underrun's thread: the app's write landed past the dry point of the
/// depth read before it, the pump's backlog at that read counted in, and
/// that depth was the usual one (`usual_ms`, [`usual_tx_depth_ms`], less a
/// call).
fn tx_underrun(t: &Tick, wakes: &Wakes, usual_ms: f64) -> Option<String> {
    if t.tx_depth_ms + CALL_MS + SLACK_MS < usual_ms {
        return None;
    }
    let top = t.depth_at_ms as f64;
    let backlog = wakes
        .stalls(top, top)
        .map(|(start, end, _)| (top - start).clamp(0.0, end - start))
        .fold(0.0, f64::max);
    let dry = t.tx_depth_ms - CALL_MS;
    (t.wrote_ms + backlog + SLACK_MS >= dry).then(|| {
        format!(
            "the app's write landed {:.2} ms after the depth read ({:.2} ms after its due \
             time){}, past the {:.2} ms the TX FIFO held less one call",
            t.wrote_ms,
            t.wrote_ms + t.depth_after_ms + t.late.as_secs_f64() * 1e3,
            if backlog > 0.0 {
                format!(" with the pump {backlog:.2} ms behind at the read")
            } else {
                String::new()
            },
            t.tx_depth_ms
        )
    })
}

/// A pump stall overlapping `from..=to` (wall ms) of at least `need_ms`, or
/// a pump restart there when `restart` explains the loss.
fn pump_stall(wakes: &Wakes, from: f64, to: f64, need_ms: f64, restart: bool) -> Option<String> {
    wakes
        .stalls(from, to)
        .find(|(start, end, w)| (restart && w.2) || end - start + SLACK_MS >= need_ms)
        .map(|(start, end, w)| {
            format!(
                "the pump was not run for {:.2} ms (a late wake {} us behind at {}{}), \
                 {:.2} ms would lose it",
                end - start,
                w.1,
                w.0,
                if w.2 { ", restarted" } else { "" },
                need_ms
            )
        })
}

/// The threads' record around a loss: the pump's late wakes and its counts
/// at that tick, and the app's tick.
fn around(t: &Tick, wakes: &Wakes, from: f64, to: f64) -> String {
    let mut s = format!(
        "app: tick {:.2} ms late, write landed {:.2} ms after the depth read, TX depth before it \
         {:.2} ms, RX depth {:.2} ms; pump: late wakes [",
        t.late.as_secs_f64() * 1e3,
        t.wrote_ms,
        t.tx_depth_ms,
        t.rx_depth_ms
    );
    // A wider window than the evidence's: what led up to it.
    for (start, end, w) in wakes.stalls(from - 60.0, to + 60.0) {
        let _ = write!(s, "{:.0}..{:.0} ({} us), ", start, end, w.1);
    }
    let _ = write!(
        s,
        "] in {from:.0}..{to:.0}, at this tick: overruns +{}, writes_after_send +{}, blocks \
         over two periods +{}, longest block so far {} us",
        t.overruns, t.after_send, t.long_blocks, t.max_process_us
    );
    s
}

/// The verdict line over `losses`: RUNNER when every one has a thread the
/// box did not run long enough, else OURS.
pub(crate) fn verdict(losses: &[Loss], lost_wakes: u64) -> String {
    let mut s = String::new();
    let unexplained = losses.iter().filter(|l| l.runner.is_none()).count();
    let _ = if losses.is_empty() {
        write!(s, "VERDICT NO LOSS")
    } else if unexplained == 0 {
        write!(
            s,
            "VERDICT RUNNER: every loss ({}) had a thread the box did not run long enough; \
             rerun",
            losses.len()
        )
    } else {
        write!(
            s,
            "VERDICT OURS: {unexplained} of {} losses with no thread late enough to lose them",
            losses.len()
        )
    };
    let _ = write!(s, " ({lost_wakes} pump late wakes lost from the status)");
    for l in losses {
        let _ = write!(
            s,
            "\n- tick {} {}: {}\n  {}",
            l.tick,
            l.what,
            l.runner.as_deref().unwrap_or("no thread late enough"),
            l.around
        );
    }
    s
}

/// The median of `values` (0 when empty).
pub(crate) fn median(values: impl Iterator<Item = f64>) -> f64 {
    let mut v: Vec<f64> = values.collect();
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// A tick on time at wall ms `at`, its TX depth 12 ms and RX 33 ms.
    fn tick(at: u64) -> Tick {
        Tick {
            late: Duration::from_micros(500),
            top_ms: at,
            depth_at_ms: at,
            end_ms: at + 1,
            wrote_ms: 0.05,
            tx_depth_ms: 12.0,
            rx_depth_ms: 33.0,
            ..Tick::default()
        }
    }

    /// Ticks every 21 ms from wall ms 1000, the last one `last`.
    fn run(last: Tick) -> Vec<Tick> {
        let mut ticks: Vec<Tick> = (0..9).map(|i| tick(1000 + 21 * i)).collect();
        ticks.push(Tick {
            top_ms: 1189,
            depth_at_ms: 1189,
            end_ms: 1190,
            ..last
        });
        ticks
    }

    fn wakes(seen: &[Wake]) -> Wakes {
        Wakes {
            seen: seen.iter().copied().collect(),
            ..Wakes::default()
        }
    }

    fn judge(ticks: &[Tick], wakes: &Wakes) -> Vec<Loss> {
        losses(ticks, wakes, 0, 22.67, 45.33)
    }

    #[test]
    fn a_tx_underrun_is_the_app_s_only_when_its_write_landed_past_the_dry_point() {
        let gap = |wrote_ms| Tick {
            gap: true,
            underrun: true,
            wrote_ms,
            ..tick(0)
        };
        // Dry no earlier than 12 - 2.67 = 9.33 ms after the depth read, with 1 ms
        // of slack.
        let late = judge(&run(gap(8.4)), &Wakes::default());
        assert_eq!(late.len(), 1);
        assert_eq!(late[0].what, "TX underrun");
        assert!(
            late[0]
                .runner
                .as_deref()
                .unwrap()
                .contains("landed 8.40 ms")
        );
        let on_time = judge(&run(gap(8.3)), &Wakes::default());
        assert_eq!(on_time[0].runner, None);
        assert!(verdict(&on_time, 0).starts_with("VERDICT OURS: 1 of 1"));
        // The pump 5 ms behind at the read (a stall 1184..1192) took it.
        let backlog = judge(&run(gap(4.0)), &wakes(&[(1192, 8_000, false)]));
        assert!(
            backlog[0]
                .runner
                .as_deref()
                .unwrap()
                .contains("5.00 ms behind at the read")
        );
        // A FIFO already short before an on-time write: no thread explains it
        // (a lost write), however early its dry point.
        let short = Tick {
            tx_depth_ms: 1.2,
            ..gap(0.05)
        };
        let lost = judge(&run(short), &Wakes::default());
        assert_eq!(lost[0].runner, None);
        assert!(lost[0].around.contains("usual TX depth 12.00 ms"));
        // A call under the usual depth (the calls' phase) is still usual.
        let phase = Tick {
            tx_depth_ms: 8.34,
            ..gap(4.7)
        };
        assert!(judge(&run(phase), &Wakes::default())[0].runner.is_some());
        let under = Tick {
            tx_depth_ms: 8.32,
            ..gap(4.7)
        };
        assert_eq!(judge(&run(under), &Wakes::default())[0].runner, None);
        // A pump stall after the read leaves the FIFO fuller: no help.
        let after = judge(&run(gap(4.0)), &wakes(&[(1199, 8_000, false)]));
        assert_eq!(after[0].runner, None);
    }

    #[test]
    fn a_tick_late_reads_the_usual_depth_less_its_lateness() {
        // v0.6.158 tag CI (windows-2025, dstack #339): the run's usual TX depth
        // 14.85 ms; a tick 3.55 ms late read 8.88 ms and its write landed
        // 10.99 ms after that read. The pump took 3.05 ms on time over the
        // lateness past the run's median (0.5 ms): the usual depth there is
        // 11.80 ms, and 8.88 ms is within a call and the slack of it.
        let usual = |t: Tick| Tick {
            tx_depth_ms: 14.85,
            ..t
        };
        let ci = |late_us, tx_depth_ms| Tick {
            gap: true,
            underrun: true,
            late: Duration::from_micros(late_us),
            wrote_ms: 10.99,
            tx_depth_ms,
            ..tick(0)
        };
        let ticks = |last| {
            let mut v: Vec<Tick> = run(last).into_iter().map(usual).collect();
            v.last_mut().unwrap().tx_depth_ms = last.tx_depth_ms;
            v
        };
        assert!((usual_tx_depth_ms(14.85, 0.5, 3.55) - 11.80).abs() < 1e-9);
        let held = judge(&ticks(ci(3_550, 8.88)), &Wakes::default());
        assert_eq!(held.len(), 1);
        assert!(
            held[0]
                .runner
                .as_deref()
                .unwrap()
                .contains("landed 10.99 ms")
        );
        assert!(held[0].around.contains("11.80 ms at this tick's"));
        assert!(verdict(&held, 0).starts_with("VERDICT RUNNER"));
        // Short of that by more than a call and the slack: a lost write.
        assert_eq!(
            judge(&ticks(ci(3_550, 8.10)), &Wakes::default())[0].runner,
            None
        );
        // The same depth at a tick on time is short: a lost write.
        assert_eq!(
            judge(&ticks(ci(500, 8.88)), &Wakes::default())[0].runner,
            None
        );
        // A tick earlier than the median reads no more than the median.
        assert_eq!(usual_tx_depth_ms(14.85, 0.5, 0.0), 14.85);
    }

    #[test]
    fn a_gap_at_a_late_tick_is_not_judged() {
        let late = Tick {
            gap: true,
            underrun: true,
            recentre: true,
            late: LATE_TICK,
            ..tick(0)
        };
        assert!(judge(&run(late), &Wakes::default()).is_empty());
        assert!(verdict(&[], 0).starts_with("VERDICT NO LOSS"));
    }

    #[test]
    fn an_rx_dropout_is_the_pump_s_only_with_a_stall_past_the_read_s_room() {
        // A read the FIFO could not cover (20 ms before a 21.33 ms read).
        let drop = Tick {
            rx: 1,
            rx_depth_ms: 20.0,
            ..tick(0)
        };
        // The room: the median 33 ms less the tick's 21.33 ms = 11.67 ms.
        let long = judge(&run(drop), &wakes(&[(1186, 10_700, false)]));
        assert_eq!(long.len(), 1);
        assert_eq!(long[0].what, "RX dropout");
        assert!(long[0].runner.as_deref().unwrap().contains("10.70 ms"));
        assert!(verdict(&long, 0).starts_with("VERDICT RUNNER: every loss (1)"));
        let short = judge(&run(drop), &wakes(&[(1186, 10_600, false)]));
        assert_eq!(short[0].runner, None);
        // Outside the tick's window (after the previous tick's counts).
        let before = judge(&run(drop), &wakes(&[(1150, 20_000, false)]));
        assert_eq!(before[0].runner, None);
        // A restart there explains it whatever its length.
        let restart = judge(&run(drop), &wakes(&[(1186, 2_000, true)]));
        assert!(restart[0].runner.as_deref().unwrap().contains("restarted"));
        // A read the FIFO covered (a tick and a call and the slack, 25 ms):
        // the pump only adds to it, so no stall explains the dropout.
        let covered = |rx_depth_ms| Tick {
            rx_depth_ms,
            ..drop
        };
        let stall = wakes(&[(1186, 12_000, true)]);
        assert_eq!(judge(&run(covered(25.01)), &stall)[0].runner, None);
        assert!(judge(&run(covered(24.99)), &stall)[0].runner.is_some());
        // As many dropouts on time as restarts: none judged.
        assert!(losses(&run(drop), &Wakes::default(), 1, 22.67, 45.33).is_empty());
    }

    #[test]
    fn a_held_tick_is_judged_from_its_second_rx_dropout() {
        let held = Tick {
            rx: 1,
            late: LATE_TICK,
            ..tick(0)
        };
        assert!(judge(&run(held), &Wakes::default()).is_empty());
        let twice = judge(&run(Tick { rx: 2, ..held }), &Wakes::default());
        assert_eq!(twice.len(), 1);
    }

    #[test]
    fn a_tx_re_centre_needs_the_pump_stalled_past_the_excess() {
        // 22.67 + 45.33 - (12 + 21.33) = 34.67 ms.
        assert!((recentre_stall_ms(22.67, 45.33, 12.0) - 34.67).abs() < 0.01);
        let gap = Tick {
            gap: true,
            recentre: true,
            ..tick(0)
        };
        let long = judge(&run(gap), &wakes(&[(1189, 33_700, false)]));
        assert_eq!(long[0].what, "TX re-centre");
        assert!(long[0].runner.is_some());
        let short = judge(&run(gap), &wakes(&[(1189, 33_600, false)]));
        assert_eq!(short[0].runner, None);
        // A restart resyncs the TX player to its target: no re-centre.
        let restart = judge(&run(gap), &wakes(&[(1189, 9_500, true)]));
        assert_eq!(restart[0].runner, None);
        // An underrun and a re-centre between two ticks: both judged.
        let both = judge(
            &run(Tick {
                underrun: true,
                ..gap
            }),
            &Wakes::default(),
        );
        let what: Vec<_> = both.iter().map(|l| l.what).collect();
        assert_eq!(what, ["TX underrun", "TX re-centre"]);
    }

    #[test]
    fn the_merge_keeps_every_wake_and_counts_those_the_status_dropped() {
        let mut w = Wakes::default();
        w.merge(2, [(1, 10, false), (2, 20, false)].into_iter());
        w.merge(2, [(9, 90, false)].into_iter());
        assert_eq!(w.seen.len(), 2, "no new count, no merge");
        assert_eq!(w.lost(), 0);
        // The status's list again, one new wake in it: 37 never seen.
        w.merge(40, [(2, 20, false), (3, 30, false)].into_iter());
        assert_eq!((w.seen.len(), w.lost(), w.count), (3, 37, 40));
    }

    #[test]
    fn the_median_takes_the_middle() {
        assert_eq!(median([3.0, 1.0, 2.0].into_iter()), 2.0);
        assert_eq!(median(std::iter::empty()), 0.0);
    }
}
