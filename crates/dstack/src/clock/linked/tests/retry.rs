//! A follower start that failed for a cause that can pass is tried again
//! (dstack #431, mbc-snv 19.11.2026: no Npcap device for the adapter 4 s
//! after logon, and no second try): the backoff on fake time, the lines,
//! the status, and a late start that becomes the source.

use std::sync::atomic::{AtomicBool, AtomicI64};

use super::*;

const S: i64 = 1_000_000_000;

/// What the test's starter answers, in turn.
#[derive(Debug, Clone, Copy)]
enum Answer {
    /// A cause that can pass (no Npcap device yet).
    Pass,
    /// A cause that cannot (Npcap not installed).
    Last,
    /// The follower starts.
    Start,
}

/// When the starter was called: the test's fake time at each call; and
/// whether the keeper of the follower it started was dropped.
#[derive(Default)]
struct Calls {
    now: AtomicI64,
    at: Mutex<Vec<i64>>,
    kept_dropped: Arc<AtomicBool>,
}

/// The test's keeper: marks its drop (a dropped keeper stops a real
/// follower's thread).
struct Keeper(Arc<AtomicBool>);

impl Drop for Keeper {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl Calls {
    /// The calls so far, in whole seconds.
    fn seconds(&self) -> Vec<i64> {
        self.at.lock().unwrap().iter().map(|t| t / S).collect()
    }
}

/// A starter on "Ethernet 3" that answers `script` in turn (then `Pass`
/// for ever), records each call's time, and starts `fake`.
fn starter(calls: &Arc<Calls>, fake: &Arc<Fake>, script: Vec<Answer>) -> Starter {
    let calls = Arc::clone(calls);
    let fake = Arc::clone(fake);
    let mut script = script.into_iter();
    Starter::new("Ethernet 3", Some(-4_671), move |iface, seed| {
        assert_eq!((iface, seed), ("Ethernet 3", Some(-4_671)));
        let n = {
            let mut at = calls.at.lock().unwrap();
            at.push(calls.now.load(Ordering::Relaxed));
            at.len()
        };
        match script.next().unwrap_or(Answer::Pass) {
            Answer::Pass => Attempt::Failed {
                passes: true,
                reason: "another failure",
                error: format!("no Npcap device (try {n})"),
            },
            Answer::Last => Attempt::Failed {
                passes: false,
                reason: "Npcap is not installed",
                error: "wpcap.dll".into(),
            },
            Answer::Start => Attempt::Started(
                Box::new(Arc::clone(&fake)),
                Box::new(Keeper(Arc::clone(&calls.kept_dropped))),
            ),
        }
    })
}

/// A ready service and a locked, settled follower on its time.
fn ready() -> (Arc<MonoClock>, Arc<Fake>) {
    let service = Arc::new(MonoClock::new());
    service.set_k(1_000_000_000_000);
    let fake = Arc::new(Fake(Mutex::new((
        Time::OnService(Arc::clone(&service), 0),
        ALIVE_LOCKED,
    ))));
    (service, fake)
}

/// The linked clock started at `t0_s` with `start`, and the boot's log.
fn boot(service: &Arc<MonoClock>, start: Starter, calls: &Calls, t0_s: i64) -> (Linked, String) {
    calls.now.store(t0_s * S, Ordering::Relaxed);
    let mut linked = None;
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        linked = Some(Linked::start_at(Arc::clone(service), start, t0_s * S));
    });
    (linked.expect("started"), log)
}

/// One selector round at `t` s, the starter seeing the same time.
fn tick(linked: &Linked, calls: &Calls, t: i64) -> Source {
    calls.now.store(t * S, Ordering::Relaxed);
    linked.tick_at(t * S)
}

fn waiting(linked: &Linked) -> Option<Waiting> {
    linked.follower_counts().and_then(|c| c.waiting)
}

/// The lines of `line` in `log`.
fn lines<'a>(log: &'a str, line: &str) -> Vec<&'a str> {
    log.lines().filter(|l| l.contains(line)).collect()
}

/// mbc-snv: the start fails at boot for a cause that can pass and is tried
/// again 1, 2, 4, 8, 16, 30, 30 ... s after each failure. Meanwhile the
/// device runs on the service, the status reads `stopped`, and one line a
/// minute names the tries; the try that starts it logs the start, and the
/// selector then takes the follower the usual way.
#[test]
fn a_start_that_failed_for_a_passing_cause_is_tried_again_until_it_is_the_source() {
    crate::test_log::undecided();
    let (service, fake) = ready();
    let calls = Arc::new(Calls::default());
    let mut script = vec![Answer::Pass; 8];
    script.push(Answer::Start);
    let (linked, log) = boot(&service, starter(&calls, &fake, script), &calls, 5);
    let warned = lines(&log, RETRY_LINE);
    assert_eq!(warned.len(), 1, "{log}");
    for field in [
        "WARN",
        "iface=Ethernet 3",
        "reason=\"another failure\"",
        "error=no Npcap device (try 1)",
        "retry_in_s=1",
    ] {
        assert!(warned[0].contains(field), "{field} in {}", warned[0]);
    }
    assert!(!log.contains(NOT_STARTED_LINE), "{log}");
    assert!(!linked.has_internal());
    // Before any round: already `stopped`, so the status has the follower.
    assert_eq!(waiting(&linked), Some(Waiting::Stopped));
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        for t in 6..126 {
            assert_eq!(tick(&linked, &calls, t), Source::Service, "at {t} s");
            assert_eq!(waiting(&linked), Some(Waiting::Stopped), "at {t} s");
        }
    });
    assert_eq!(calls.seconds(), [5, 6, 8, 12, 20, 36, 66, 96]);
    // The minute from the first round (6 s): one line, at 66 s, after that
    // second's try.
    let minute = lines(&log, NOT_IN_USE_LINE);
    assert_eq!(minute.len(), 1, "{log}");
    for field in [
        "INFO",
        "reason=stopped",
        "iface=Ethernet 3",
        "start_attempts=7",
        "retrying_for_s=61",
        "start_failure=\"another failure\"",
        "error=no Npcap device (try 7)",
        "start_ms=",
        "source=Service",
    ] {
        assert!(minute[0].contains(field), "{field} in {}", minute[0]);
    }
    assert!(!log.contains(STARTED_LINE), "{log}");
    assert!(!log.contains(NOT_STARTED_LINE), "{log}");
    assert!(!linked.has_internal());
    // The ninth try starts it: one line, and the selector agrees and hands
    // over (a seamless switch, or after a wait for agreement).
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        let mut t = 126;
        while tick(&linked, &calls, t) != Source::Internal {
            assert!(t < 126 + 2 * i64::from(HANDOVER_WAIT), "never handed over");
            t += 1;
        }
        assert_eq!(waiting(&linked), None);
        for t in t + 1..t + 100 {
            assert_eq!(tick(&linked, &calls, t), Source::Internal);
        }
    });
    assert!(linked.has_internal());
    assert_eq!(calls.seconds(), [5, 6, 8, 12, 20, 36, 66, 96, 126]);
    let started = lines(&log, STARTED_LINE);
    assert_eq!(started.len(), 1, "{log}");
    for field in [
        "INFO",
        "iface=Ethernet 3",
        "seed_ppb=Some(-4671)",
        "start_attempts=9",
        "retrying_for_s=121",
        "start_ms=",
    ] {
        assert!(started[0].contains(field), "{field} in {}", started[0]);
    }
    assert!(!log.contains("reason=stopped"), "{log}");
    // The follower started late is kept running for the clock's life.
    assert!(!calls.kept_dropped.load(Ordering::SeqCst));
    drop(linked);
    assert!(calls.kept_dropped.load(Ordering::SeqCst));
}

/// A cause that never passes: the tries go on for ever, and 10 minutes
/// after the first failure one warning says so (the starts so far, the
/// seconds, the last error), never before and never again; the minute
/// line goes on as before (#431's escalation, dstack's call: retry for
/// ever, but say it loudly once).
#[test]
fn a_start_still_failing_after_10_minutes_warns_once_and_is_tried_on() {
    crate::test_log::undecided();
    let (service, fake) = ready();
    let calls = Arc::new(Calls::default());
    let (linked, _) = boot(&service, starter(&calls, &fake, vec![]), &calls, 5);
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        for t in 6..605 {
            assert_eq!(tick(&linked, &calls, t), Source::Service, "at {t} s");
        }
    });
    assert!(!log.contains(STILL_NOT_STARTED_LINE), "{log}");
    assert!(!log.contains("WARN"), "{log}");
    // Tries at 5, 6, 8, 12, 20, 36, then every 30 s from 66 to 576.
    assert_eq!(calls.seconds().len(), 24);
    assert_eq!(calls.seconds().last(), Some(&576));
    // 600 s after the first failure (at 5 s): the warning, in that round.
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        assert_eq!(tick(&linked, &calls, 605), Source::Service);
    });
    let warned = lines(&log, STILL_NOT_STARTED_LINE);
    assert_eq!(warned.len(), 1, "{log}");
    for field in [
        "WARN",
        "iface=Ethernet 3",
        "start_attempts=24",
        "retrying_for_s=600",
        "start_failure=\"another failure\"",
        "error=no Npcap device (try 24)",
        "start_ms=",
    ] {
        assert!(warned[0].contains(field), "{field} in {}", warned[0]);
    }
    assert_eq!(log.lines().count(), 1, "{log}");
    // The tries go on every 30 s, the minute line as before, no second
    // warning.
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        for t in 606..=1300 {
            assert_eq!(tick(&linked, &calls, t), Source::Service, "at {t} s");
            assert_eq!(waiting(&linked), Some(Waiting::Stopped), "at {t} s");
        }
    });
    assert!(!log.contains(STILL_NOT_STARTED_LINE), "{log}");
    assert!(!log.contains("WARN"), "{log}");
    assert_eq!(calls.seconds().len(), 24 + 24);
    assert_eq!(calls.seconds().last(), Some(&1296));
    let minute = lines(&log, NOT_IN_USE_LINE);
    assert_eq!(minute.len(), 12, "{log}");
    for l in minute {
        assert!(l.contains("INFO") && l.contains("reason=stopped"), "{l}");
    }
    assert!(!linked.has_internal());
}

/// The warning is due from 600 s after the first failure, to the ns, and
/// once.
#[test]
fn the_tries_escalate_once_from_600_s_after_the_first_failure() {
    use crate::clock::linked::retry::Retry;
    let start = Starter::new("Ethernet 3", None, |_, _| unreachable!("not run"));
    let t0 = 5 * S;
    let mut r = Retry::new(start, t0, "another failure", "first".into());
    assert_eq!(ESCALATE_AFTER_S, 600);
    assert!(!r.escalate(t0 + 600 * S - 1));
    assert!(!r.escalate(t0 + 599 * S));
    assert!(r.escalate(t0 + 600 * S));
    for t in [600, 601, 1200, 100_000] {
        assert!(!r.escalate(t0 + t * S), "again at {t} s");
    }
}

/// A follower started by a try waits a minute of its own before its first
/// line: never the tries' minute, which would call a fresh follower's 0
/// windows `no_ptp` in the very round it started (the review of #431: the
/// 30 s tries start it on a minute's edge every other try).
#[test]
fn a_follower_started_late_waits_its_own_minute_before_a_line() {
    crate::test_log::undecided();
    let (service, fake) = ready();
    // Alive, hearing nothing yet, not locked: not taken from the service.
    set(
        &fake,
        Time::OnService(Arc::clone(&service), 0),
        View {
            alive: true,
            ..View::default()
        },
    );
    let calls = Arc::new(Calls::default());
    let mut script = vec![Answer::Pass; 8];
    script.push(Answer::Start);
    let (linked, _) = boot(&service, starter(&calls, &fake, script), &calls, 5);
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        for t in 6..126 {
            tick(&linked, &calls, t);
        }
    });
    // The tries' minute: lines at 66 s, and its next due at 126 s.
    assert_eq!(lines(&log, NOT_IN_USE_LINE).len(), 1, "{log}");
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        assert_eq!(tick(&linked, &calls, 126), Source::Service);
    });
    assert!(linked.has_internal(), "started at 126 s");
    assert_eq!(lines(&log, NOT_IN_USE_LINE).len(), 0, "{log}");
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        for t in 127..186 {
            assert_eq!(tick(&linked, &calls, t), Source::Service, "at {t} s");
        }
    });
    assert_eq!(lines(&log, NOT_IN_USE_LINE).len(), 0, "{log}");
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        assert_eq!(tick(&linked, &calls, 186), Source::Service);
    });
    let l = lines(&log, NOT_IN_USE_LINE);
    assert_eq!(l.len(), 1, "{log}");
    for field in ["reason=no_ptp", "windows_last_minute=0"] {
        assert!(l[0].contains(field), "{field} in {}", l[0]);
    }
    assert!(!l[0].contains("start_attempts"), "{}", l[0]);
    assert_eq!(waiting(&linked), Some(Waiting::NoPtp));
}

/// A cause that cannot pass (Npcap not installed) stays as before: one
/// warning, no second try, no follower in the status.
#[test]
fn a_start_that_failed_for_a_lasting_cause_is_never_tried_again() {
    crate::test_log::undecided();
    let (service, fake) = ready();
    let calls = Arc::new(Calls::default());
    let (linked, log) = boot(
        &service,
        starter(&calls, &fake, vec![Answer::Last]),
        &calls,
        5,
    );
    let warned = lines(&log, NOT_STARTED_LINE);
    assert_eq!(warned.len(), 1, "{log}");
    for field in [
        "WARN",
        "iface=Ethernet 3",
        "reason=\"Npcap is not installed\"",
        "error=wpcap.dll",
    ] {
        assert!(warned[0].contains(field), "{field} in {}", warned[0]);
    }
    assert!(!log.contains(RETRY_LINE), "{log}");
    assert_eq!(linked.follower_counts(), None);
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        for t in 6..=200 {
            assert_eq!(tick(&linked, &calls, t), Source::Service, "at {t} s");
        }
    });
    assert_eq!(calls.seconds(), [5]);
    assert_eq!(linked.follower_counts(), None);
    assert!(!log.contains(NOT_IN_USE_LINE), "{log}");
    assert!(!log.contains(NOT_STARTED_LINE), "{log}");
}

/// A try whose cause has become one that cannot pass ends the tries with
/// the lasting warning, once; the status then has no follower.
#[test]
fn a_try_that_fails_for_a_lasting_cause_ends_the_tries() {
    crate::test_log::undecided();
    let (service, fake) = ready();
    let calls = Arc::new(Calls::default());
    let script = vec![Answer::Pass, Answer::Pass, Answer::Last];
    let (linked, _) = boot(&service, starter(&calls, &fake, script), &calls, 5);
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        for t in 6..8 {
            assert_eq!(tick(&linked, &calls, t), Source::Service, "at {t} s");
            assert_eq!(waiting(&linked), Some(Waiting::Stopped), "at {t} s");
        }
        for t in 8..=200 {
            assert_eq!(tick(&linked, &calls, t), Source::Service, "at {t} s");
            assert_eq!(linked.follower_counts(), None, "at {t} s");
        }
    });
    assert_eq!(calls.seconds(), [5, 6, 8]);
    let warned = lines(&log, NOT_STARTED_LINE);
    assert_eq!(warned.len(), 1, "{log}");
    for field in ["WARN", "start_attempts=3", "error=wpcap.dll"] {
        assert!(warned[0].contains(field), "{field} in {}", warned[0]);
    }
    assert!(!log.contains(NOT_IN_USE_LINE), "{log}");
    assert!(!linked.has_internal());
}

/// dsync-ptp v1.33.1's kinds: Npcap not installed, port 319 refused and no
/// adapter of that name cannot pass; an adapter without an IPv4 yet and a
/// capture that did not open ("no Npcap device carries <ip>") can.
#[test]
fn only_a_start_failure_that_can_pass_is_tried_again() {
    use dsync_ptp::follower::FollowerError;
    use dsync_ptp::net::PinnedInterfaceUnavailable;
    let pinned = |missing| FollowerError::InterfaceNotFound {
        interface: "Ethernet 3".into(),
        cause: PinnedInterfaceUnavailable {
            name: "Ethernet 3".into(),
            missing,
        }
        .into(),
    };
    assert!(!start_passes(&FollowerError::NpcapMissing {
        detail: "wpcap.dll".into()
    }));
    assert!(!start_passes(&FollowerError::PermissionDenied {
        cause: std::io::Error::from(std::io::ErrorKind::PermissionDenied).into()
    }));
    assert!(!start_passes(&pinned(true)), "no adapter of that name");
    assert!(start_passes(&pinned(false)), "the adapter has no IPv4 yet");
    assert!(start_passes(&FollowerError::InterfaceNotFound {
        interface: "Ethernet 3".into(),
        cause: std::io::Error::from(std::io::ErrorKind::NotFound).into(),
    }));
    // mbc-snv's failure as dsync-ptp v1.33.1 words it (`net_pcap.rs`;
    // the address a documentation one): an `anyhow!` from the open.
    const NO_DEVICE: &str =
        "pinned PTP interface Ethernet 3 (192.0.2.10): no Npcap device carries 192.0.2.10";
    let capture = FollowerError::Other(std::io::Error::other(NO_DEVICE).into());
    assert!(start_passes(&capture));
    match failed(&capture) {
        Attempt::Failed {
            passes,
            reason,
            error,
        } => {
            assert!(passes);
            assert_eq!(reason, "another failure");
            assert_eq!(error, NO_DEVICE);
        }
        Attempt::Started(..) => panic!("a failure"),
    }
}

/// dsync's own start on an adapter no PC has: failed, for good.
#[test]
fn dsync_s_start_on_no_such_adapter_fails_for_good() {
    for seed in [None, Some(-4_671)] {
        match attempt("dstack-no-such-nic", seed) {
            Attempt::Failed { passes, reason, .. } => {
                assert!(!passes);
                assert_eq!(reason, "no such interface, or no IPv4 on it");
            }
            Attempt::Started(..) => panic!("started on no adapter"),
        }
    }
}

/// The wait after each failed start: 1, 2, 4, 8, 16, then 30 s for ever.
#[test]
fn the_tries_wait_1_2_4_8_16_then_30_s() {
    let waits: Vec<i64> = (1..=8).map(retry_delay_s).collect();
    assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30, 30]);
    assert_eq!(RETRY_MAX_S, 30);
    // No failure yet reads as the first; any count stays at the longest.
    assert_eq!(retry_delay_s(0), 1);
    assert_eq!(retry_delay_s(u32::MAX), 30);
}

/// A try is due from its delay after the last failure, to the ns, never
/// before; the seconds since the first failure count whole seconds.
#[test]
fn a_try_is_due_its_delay_after_the_last_failure() {
    use crate::clock::linked::retry::Retry;
    let start = Starter::new("Ethernet 3", None, |_, _| unreachable!("not run"));
    let t0 = 5 * S;
    let mut r = Retry::new(start, t0, "another failure", "first".into());
    assert_eq!(
        (r.attempts, r.reason, r.error.as_str(), r.start_ms),
        (1, "another failure", "first", 0)
    );
    assert!(!r.due(t0 + S - 1));
    assert!(r.due(t0 + S));
    r.failed(
        t0 + S,
        "no such interface, or no IPv4 on it",
        "second".into(),
        7,
    );
    assert_eq!(
        (r.attempts, r.reason, r.error.as_str(), r.start_ms),
        (2, "no such interface, or no IPv4 on it", "second", 7)
    );
    assert!(!r.due(t0 + 3 * S - 1));
    assert!(r.due(t0 + 3 * S));
    r.failed(t0 + 3 * S + 7, "another failure", "third".into(), 0);
    assert!(!r.due(t0 + 7 * S + 6));
    assert!(r.due(t0 + 7 * S + 7));
    assert_eq!(r.retrying_for_s(t0 + 61 * S - 1), 60);
    assert_eq!(r.retrying_for_s(t0 + 61 * S), 61);
}
