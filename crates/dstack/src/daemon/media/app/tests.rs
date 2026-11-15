// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;

/// How long a test waits for the thread at most: generous, since the
/// coverage job runs instrumented and runners start threads late.
const WITHIN: Duration = Duration::from_secs(10);

/// Reads `hook` every 10 ms until `done` holds for what it gives, within
/// [`WITHIN`]; that last read.
fn read_until(hook: &AppHook, done: impl Fn(&Option<String>) -> bool) -> Option<String> {
    let end = Instant::now() + WITHIN;
    loop {
        let got = hook.read();
        if done(&got) {
            return got;
        }
        assert!(Instant::now() < end, "never came: last {got:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Waits (reading nothing) until `n` holds `want` or more, within
/// [`WITHIN`].
fn count_reaches(n: &AtomicUsize, want: usize) {
    let end = Instant::now() + WITHIN;
    while n.load(Ordering::Relaxed) < want {
        assert!(
            Instant::now() < end,
            "{} calls, not {want}",
            n.load(Ordering::Relaxed)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A hook returning `text` that counts its calls in `n`.
fn counted(n: &Arc<AtomicUsize>, text: &'static str) -> AppStatus {
    let n = Arc::clone(n);
    Arc::new(move || {
        n.fetch_add(1, Ordering::Relaxed);
        text.to_string()
    })
}

/// The thread calls the hook once a refresh, read or not (dstack #317:
/// before, only while a status was read within 5 s, so a reader that reads
/// rarely got the part of its last read, stale).
#[test]
fn the_thread_calls_at_most_once_a_refresh_read_or_not() {
    let t0 = Instant::now();
    let s = Duration::from_millis;
    // The first call at once.
    assert_eq!(next(t0, None), Next::Call);
    // Then once the part is a refresh old, waiting out the rest before.
    assert_eq!(next(t0 + s(300), Some(t0)), Next::Wait(REFRESH - s(300)));
    assert_eq!(next(t0, Some(t0)), Next::Wait(REFRESH));
    assert_eq!(next(t0 + REFRESH - s(1), Some(t0)), Next::Wait(s(1)));
    assert_eq!(next(t0 + REFRESH, Some(t0)), Next::Call);
    assert_eq!(next(t0 + REFRESH + s(1), Some(t0)), Next::Call);
    // A part from a clock "after now" counts as now.
    assert_eq!(next(t0, Some(t0 + s(5))), Next::Wait(REFRESH));
}

#[test]
fn a_part_is_stale_past_three_seconds_and_says_so() {
    let t0 = Instant::now();
    assert!(!is_stale(t0 + STALE, t0));
    assert!(is_stale(t0 + STALE + Duration::from_millis(1), t0));
    assert!(
        !is_stale(t0, t0 + Duration::from_secs(9)),
        "a part from later"
    );
    assert_eq!(
        mark_stale(r#"{"line":"x","n":2}"#),
        r#"{"line":"x","n":2,"stale":true}"#
    );
    assert_eq!(mark_stale("{}"), r#"{"stale":true}"#);
    assert_eq!(
        mark_stale(r#"{"stale":false}"#),
        r#"{"stale":true}"#,
        "ours wins"
    );
    assert_eq!(mark_stale("[1]"), r#"{"stale":true}"#);
}

#[test]
fn the_hook_is_called_once_a_refresh_read_or_not() {
    let calls = Arc::new(AtomicUsize::new(0));
    let hook = AppHook::default();
    assert_eq!(hook.read(), None, "no hook");
    let started = Instant::now();
    hook.set(counted(&calls, r#"{"state":"ok"}"#));
    // Unread: the first call at once, the next after a refresh.
    count_reaches(&calls, 1);
    count_reaches(&calls, 2);
    assert!(started.elapsed() >= REFRESH, "{:?}", started.elapsed());
    assert_eq!(hook.read().as_deref(), Some(r#"{"state":"ok"}"#));
}

/// dstack #317 (dasio, field 15.11.2026): an app that logs its status once
/// every 5 minutes read a part made at its previous read, stale and 5
/// minutes old: `to_asio_underruns: 0` in the middle of a storm that had
/// passed 900. A part read after a quiet spell is at most a refresh old,
/// never stale, and carries the counter as it is now.
#[test]
fn a_part_read_after_a_quiet_spell_is_fresh() {
    let calls = Arc::new(AtomicUsize::new(0));
    let n = Arc::clone(&calls);
    let hook = AppHook::default();
    hook.set(Arc::new(move || {
        format!(r#"{{"n":{}}}"#, n.fetch_add(1, Ordering::Relaxed) + 1)
    }));
    assert_eq!(
        read_until(&hook, Option::is_some).as_deref(),
        Some(r#"{"n":1}"#)
    );
    // Quiet past the old read window (5 s) and the stale age (3 s) after it.
    std::thread::sleep(Duration::from_secs(9));
    let got = hook.read().expect("a part");
    let v: serde_json::Value = serde_json::from_str(&got).unwrap();
    assert_eq!(v.get("stale"), None, "{got}");
    let n = v["n"].as_u64().unwrap();
    assert!(
        n >= 5,
        "a part about a refresh old, not the last read's: {got}"
    );
}

#[test]
fn a_hook_that_stalls_never_delays_the_status_which_turns_stale() {
    crate::test_log::undecided();
    let (release, gate) = mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let calls = Arc::new(AtomicUsize::new(0));
    let n = Arc::clone(&calls);
    let hook = AppHook::default();
    hook.set(Arc::new(move || {
        if n.fetch_add(1, Ordering::Relaxed) == 0 {
            return r#"{"n":1}"#.to_string();
        }
        // Stalls until the test releases it (or the test ends).
        let _ = gate.lock().unwrap().recv();
        r#"{"n":2}"#.to_string()
    }));
    assert_eq!(
        read_until(&hook, Option::is_some).as_deref(),
        Some(r#"{"n":1}"#)
    );
    let mut slowest = Duration::ZERO;
    let logs = crate::test_log::capture_warnings(|| {
        let end = Instant::now() + WITHIN;
        let mut stale_reads = 0;
        while stale_reads < 3 {
            let start = Instant::now();
            let got = hook.read();
            slowest = slowest.max(start.elapsed());
            if got.as_deref() == Some(r#"{"n":1,"stale":true}"#) {
                stale_reads += 1;
            } else {
                assert_eq!(got.as_deref(), Some(r#"{"n":1}"#));
            }
            assert!(Instant::now() < end, "never stale");
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    assert!(calls.load(Ordering::Relaxed) >= 2, "the second call stalls");
    // A read is a lock and a copy: it never waits out the hook's stall
    // (it turned stale while the hook still waited).
    assert!(slowest < STALE, "{slowest:?}");
    assert_eq!(logs.matches("has not returned").count(), 1, "{logs}");
    release.send(()).unwrap();
    read_until(&hook, |p| p.as_deref() == Some(r#"{"n":2}"#));
}

#[test]
fn a_new_hook_takes_effect_and_the_old_one_s_late_part_is_dropped() {
    let (release, gate) = mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let old_calls = Arc::new(AtomicUsize::new(0));
    let n = Arc::clone(&old_calls);
    let hook = AppHook::default();
    hook.set(Arc::new(move || {
        if n.fetch_add(1, Ordering::Relaxed) > 0 {
            let _ = gate.lock().unwrap().recv();
        }
        r#"{"hook":"old"}"#.to_string()
    }));
    read_until(&hook, |p| p.as_deref() == Some(r#"{"hook":"old"}"#));
    // The old hook's second call is in progress when the new hook comes.
    while old_calls.load(Ordering::Relaxed) < 2 {
        let _ = hook.read();
        std::thread::sleep(Duration::from_millis(10));
    }
    let new_calls = Arc::new(AtomicUsize::new(0));
    hook.set(counted(&new_calls, r#"{"hook":"new"}"#));
    assert_eq!(hook.read(), None, "the old part is gone with its hook");
    release.send(()).unwrap();
    // The old call's part is stored nowhere: the first part is the new one.
    assert_eq!(
        read_until(&hook, Option::is_some).as_deref(),
        Some(r#"{"hook":"new"}"#)
    );
    let old = old_calls.load(Ordering::Relaxed);
    let end = Instant::now() + REFRESH + Duration::from_millis(300);
    while Instant::now() < end {
        assert_eq!(hook.read().as_deref(), Some(r#"{"hook":"new"}"#));
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        old_calls.load(Ordering::Relaxed),
        old,
        "the old hook is gone"
    );
    count_reaches(&new_calls, 2);
}

#[test]
fn a_hook_that_panics_keeps_its_last_part_and_the_thread_alive() {
    crate::test_log::undecided();
    let calls = Arc::new(AtomicUsize::new(0));
    let n = Arc::clone(&calls);
    let hook = AppHook::default();
    hook.set(Arc::new(move || {
        if n.fetch_add(1, Ordering::Relaxed) == 0 {
            return r#"{"n":1}"#.to_string();
        }
        panic!("the app's hook fails (a test)");
    }));
    assert_eq!(
        read_until(&hook, Option::is_some).as_deref(),
        Some(r#"{"n":1}"#)
    );
    // Called again at each refresh after a panic, the last part kept; the
    // panic warned once over three panics and many reads.
    let logs = crate::test_log::capture_warnings(|| {
        while calls.load(Ordering::Relaxed) < 4 {
            let got = hook.read();
            assert!(
                got.as_deref().is_some_and(|p| p.starts_with(r#"{"n":1"#)),
                "{got:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        for _ in 0..3 {
            let _ = hook.read();
        }
    });
    assert_eq!(logs.matches("hook panicked").count(), 1, "{logs}");
    // A new hook takes over on the same thread, and warns nothing of the
    // old one's panics.
    hook.set(Arc::new(|| r#"{"n":2}"#.to_string()));
    let logs = crate::test_log::capture_warnings(|| {
        read_until(&hook, |p| p.as_deref() == Some(r#"{"n":2}"#));
    });
    assert_eq!(logs, "");
}

#[test]
fn the_thread_ends_with_its_owner() {
    let token = Arc::new(());
    let held = Arc::clone(&token);
    let hook = AppHook::default();
    hook.set(Arc::new(move || {
        let _ = &held;
        "{}".to_string()
    }));
    read_until(&hook, Option::is_some);
    assert_eq!(Arc::strong_count(&token), 2, "the hook holds it");
    drop(hook);
    // The thread ends and drops the hook with it.
    let end = Instant::now() + WITHIN;
    while Arc::strong_count(&token) > 1 {
        assert!(Instant::now() < end, "the thread still runs");
        std::thread::sleep(Duration::from_millis(10));
    }
}
