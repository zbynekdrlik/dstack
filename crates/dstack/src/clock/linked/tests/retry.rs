//! A follower start that failed for a cause that can pass is tried again
//! (dstack #431, mbc-snv 19.11.2026: no Npcap device for the adapter 4 s
//! after logon, and no second try): the backoff on fake time, the lines,
//! the status, and a late start that becomes the source.

use std::sync::atomic::AtomicI64;

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

/// When the starter was called: the test's fake time at each call.
#[derive(Default)]
struct Calls {
    now: AtomicI64,
    at: Mutex<Vec<i64>>,
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
            Answer::Start => Attempt::Started(Box::new(Arc::clone(&fake)), Box::new(())),
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
        "start_attempts=7",
        "retrying_for_s=61",
        "error=no Npcap device (try 7)",
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
    ] {
        assert!(started[0].contains(field), "{field} in {}", started[0]);
    }
    assert!(!log.contains("reason=stopped"), "{log}");
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
    let capture = FollowerError::Other(
        std::io::Error::other("pinned PTP interface Ethernet 3: no Npcap device").into(),
    );
    assert!(start_passes(&capture));
    match failed(&capture) {
        Attempt::Failed {
            passes,
            reason,
            error,
        } => {
            assert!(passes);
            assert_eq!(reason, "another failure");
            assert_eq!(error, "pinned PTP interface Ethernet 3: no Npcap device");
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
