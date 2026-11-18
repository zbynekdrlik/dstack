//! Why the follower is not in use (dstack #420): the reason, the
//! once-a-minute limiter on fake time, and the selector's line and status.

use super::*;

const S: i64 = 1_000_000_000;
const GM: [u8; 6] = [0x00, 0x1d, 0xc1, 0xab, 0xcd, 0xef];

fn view(alive: bool, locked: bool, settled: bool) -> View {
    View {
        alive,
        locked,
        settled,
        anchored: locked,
        epoch: 0,
    }
}

/// Each reason from the first condition that holds, whatever the later
/// ones say.
#[test]
fn the_reason_is_the_first_that_holds() {
    let all = view(true, true, true);
    assert_eq!(
        waiting_of(
            View {
                alive: false,
                ..all
            },
            Some(9)
        ),
        Waiting::Stopped
    );
    assert_eq!(
        waiting_of(view(false, false, false), Some(0)),
        Waiting::Stopped
    );
    assert_eq!(waiting_of(view(false, true, true), None), Waiting::Stopped);
    assert_eq!(waiting_of(all, Some(0)), Waiting::NoPtp);
    assert_eq!(
        waiting_of(view(true, false, false), Some(0)),
        Waiting::NoPtp
    );
    assert_eq!(
        waiting_of(view(true, false, true), Some(1)),
        Waiting::NotLocked
    );
    assert_eq!(
        waiting_of(view(true, false, false), Some(1)),
        Waiting::NotLocked
    );
    assert_eq!(
        waiting_of(view(true, true, false), Some(1)),
        Waiting::NotSettled
    );
    assert_eq!(waiting_of(all, Some(1)), Waiting::WaitsToAgree);
    assert_eq!(waiting_of(all, Some(60)), Waiting::WaitsToAgree);
}

/// A wait's first round has counted nothing yet: never `no_ptp` for a
/// follower that is locked (the review of #420: a locked, settled follower
/// read `no_ptp` for its first second).
#[test]
fn a_wait_s_first_round_judges_the_lock_not_the_count() {
    assert_eq!(
        waiting_of(view(true, true, true), None),
        Waiting::WaitsToAgree
    );
    assert_eq!(
        waiting_of(view(true, true, false), None),
        Waiting::NotSettled
    );
    assert_eq!(
        waiting_of(view(true, false, false), None),
        Waiting::NotLocked
    );
}

#[test]
fn each_reason_has_its_own_status_word() {
    let words = [
        Waiting::Stopped,
        Waiting::NoPtp,
        Waiting::NotLocked,
        Waiting::NotSettled,
        Waiting::WaitsToAgree,
    ]
    .map(Waiting::word);
    assert_eq!(
        words,
        [
            "stopped",
            "no_ptp",
            "not_locked",
            "not_settled",
            "waits_to_agree"
        ]
    );
}

#[test]
fn a_grandmaster_reads_as_hex_pairs_or_none() {
    assert_eq!(grandmaster_text(Some(GM)), "00:1d:c1:ab:cd:ef");
    assert_eq!(grandmaster_text(None), "none");
}

/// One line a minute of waiting, from the minute's start (never at once),
/// with the windows of that minute; the windows heard lately are the last
/// whole minute's or this one's so far, the more (none counted on a wait's
/// first round).
#[test]
fn the_minute_limiter_counts_a_minute_of_waiting_on_fake_time() {
    let mut m = Minute::default();
    let t0 = 5 * S;
    assert_eq!(m.round(t0, 100, true), (None, None));
    assert_eq!(m.round(t0 + S, 100, true), (Some(0), None));
    assert_eq!(m.round(t0 + 30 * S, 103, true), (Some(3), None));
    assert_eq!(m.round(t0 + 60 * S - 1, 110, true), (Some(10), None));
    // A minute: the line, with that minute's windows.
    assert_eq!(m.round(t0 + 60 * S, 111, true), (Some(11), Some(11)));
    // The last minute's count holds until this one's passes it.
    assert_eq!(m.round(t0 + 61 * S, 112, true), (Some(11), None));
    assert_eq!(m.round(t0 + 90 * S, 140, true), (Some(29), None));
    assert_eq!(m.round(t0 + 120 * S - 1, 140, true), (Some(29), None));
    assert_eq!(m.round(t0 + 120 * S, 141, true), (Some(30), Some(30)));
    // A silent minute after it.
    assert_eq!(m.round(t0 + 150 * S, 141, true), (Some(30), None));
    assert_eq!(m.round(t0 + 180 * S, 141, true), (Some(0), Some(0)));
    // In use: nothing, and the next wait starts a minute of its own.
    assert_eq!(m.round(t0 + 181 * S, 150, false), (None, None));
    assert_eq!(m, Minute::default());
    assert_eq!(m.round(t0 + 200 * S, 160, true), (None, None));
    assert_eq!(m.round(t0 + 240 * S, 161, true), (Some(1), None));
    assert_eq!(m.round(t0 + 260 * S, 165, true), (Some(5), Some(5)));
    // A count that went back (never in dsync): this minute's count reads
    // as 0; the last whole minute's 5 holds.
    assert_eq!(m.round(t0 + 261 * S, 100, true), (Some(5), None));
}

/// What the test's follower has: its view, windows and grandmaster, and
/// its time's offset from the service's (`None`: no time).
#[derive(Clone, Copy)]
struct Heard {
    view: View,
    windows: u64,
    gm: Option<[u8; 6]>,
    offset_ns: Option<i64>,
}

/// A follower the test drives ([`Heard`]).
struct Hears {
    service: Arc<MonoClock>,
    state: Mutex<Heard>,
}

impl Follow for Arc<Hears> {
    fn now_ns(&self) -> Option<i64> {
        let offset = self.state.lock().unwrap().offset_ns?;
        self.service
            .now_ns()
            .map(|t| i64::try_from(t).unwrap() + offset)
    }
    fn view(&self) -> View {
        self.state.lock().unwrap().view
    }
    fn counts(&self) -> FollowerCounts {
        let Heard {
            view: v,
            windows,
            gm,
            ..
        } = *self.state.lock().unwrap();
        FollowerCounts {
            counts: ClockCounts {
                polls: windows,
                ..ClockCounts::default()
            },
            locked: v.locked,
            settled: v.settled,
            offset_ns: Some(-4_200),
            grandmaster: gm,
            ..FollowerCounts::default()
        }
    }
}

/// The lines of [`NOT_IN_USE_LINE`] in `log`.
fn lines(log: &str) -> Vec<&str> {
    log.lines()
        .filter(|l| l.contains(NOT_IN_USE_LINE))
        .collect()
}

/// strih-snv (18.11.2026): the follower started and the service was taken;
/// then nothing for 2.5 h. Now each minute it is not in use says why, the
/// status keeps the reason, and once it is in use both stop.
#[test]
fn the_selector_says_once_a_minute_why_the_follower_is_not_in_use() {
    crate::test_log::undecided();
    let service = Arc::new(MonoClock::new());
    service.set_k(1_000_000_000_000);
    let hears = Arc::new(Hears {
        service: Arc::clone(&service),
        state: Mutex::new(Heard {
            view: view(true, false, false),
            windows: 0,
            gm: None,
            offset_ns: None,
        }),
    });
    let linked = Linked::new(
        Some(Box::new(Arc::clone(&hears))),
        Arc::clone(&service),
        None,
    );
    let waiting = || linked.follower_counts().and_then(|c| c.waiting);
    // A minute hearing nothing: one line, at its end.
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        for t in 0..=60 {
            assert_eq!(linked.tick_at(t * S), Source::Service);
            // The first round has counted nothing: judged on the lock.
            let want = if t == 0 {
                Waiting::NotLocked
            } else {
                Waiting::NoPtp
            };
            assert_eq!(waiting(), Some(want), "at {t} s");
        }
    });
    let l = lines(&log);
    assert_eq!(l.len(), 1, "{log}");
    for field in [
        "reason=no_ptp",
        "windows_last_minute=0",
        "grandmaster=none",
        "locked=false",
        "settled=false",
        "error_ns=Some(-4200)",
        "offset_ns=None",
    ] {
        assert!(l[0].contains(field), "{field} in {}", l[0]);
    }
    // It hears a grandmaster and closes a window a second, unlocked.
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        for t in 61..=120 {
            {
                let mut s = hears.state.lock().unwrap();
                s.windows += 1;
                s.gm = Some(GM);
            }
            assert_eq!(linked.tick_at(t * S), Source::Service);
            assert_eq!(waiting(), Some(Waiting::NotLocked), "at {t} s");
        }
    });
    let l = lines(&log);
    assert_eq!(l.len(), 1, "{log}");
    for field in [
        "reason=not_locked",
        "windows_last_minute=60",
        "grandmaster=00:1d:c1:ab:cd:ef",
    ] {
        assert!(l[0].contains(field), "{field} in {}", l[0]);
    }
    // Locked and settled, 1 ms off the service: the switch waits to agree,
    // and the line carries the offset.
    {
        let mut s = hears.state.lock().unwrap();
        s.view = view(true, true, true);
        s.offset_ns = Some(1_000_000);
    }
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        for t in 121..=180 {
            hears.state.lock().unwrap().windows += 1;
            assert_eq!(linked.tick_at(t * S), Source::Service, "at {t} s");
            assert_eq!(waiting(), Some(Waiting::WaitsToAgree), "at {t} s");
        }
    });
    let l = lines(&log);
    assert_eq!(l.len(), 1, "{log}");
    for field in [
        "reason=waits_to_agree",
        "windows_last_minute=60",
        "locked=true",
        "settled=true",
        "offset_ns=Some(",
    ] {
        assert!(l[0].contains(field), "{field} in {}", l[0]);
    }
    // The wait ends in a re-align; from then on, in use: no reason, no line.
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        let mut t = 181;
        while linked.tick_at(t * S) != Source::Internal {
            assert!(t < 1_000, "the wait never ended");
            t += 1;
        }
        assert_eq!(waiting(), None);
        for t in t + 1..t + 200 {
            assert_eq!(linked.tick_at(t * S), Source::Internal);
            assert_eq!(waiting(), None);
        }
    });
    assert_eq!(lines(&log).len(), 0, "{log}");
    assert_eq!(log.matches(RE_ALIGN_LINE).count(), 1, "{log}");
}

/// No follower started: nothing to say about it.
#[test]
fn a_linked_clock_without_a_follower_logs_no_reason() {
    crate::test_log::undecided();
    let service = Arc::new(MonoClock::new());
    service.set_k(1_000_000_000_000);
    let linked = Linked::new(None, service, None);
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        for t in 0..=130 {
            assert_eq!(linked.tick_at(t * S), Source::Service);
        }
    });
    assert_eq!(lines(&log).len(), 0, "{log}");
    assert_eq!(linked.follower_counts(), None);
}
