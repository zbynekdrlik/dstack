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

#[test]
fn the_thread_calls_only_while_read_and_at_most_once_a_refresh() {
    let t0 = Instant::now();
    let s = Duration::from_millis;
    // Never read, or read a window ago: nothing to do.
    assert_eq!(next(t0, None, None), Next::Idle);
    assert_eq!(next(t0 + READ_WINDOW, Some(t0), None), Next::Idle);
    assert_eq!(
        next(t0 + READ_WINDOW + s(1), Some(t0), Some(t0)),
        Next::Idle
    );
    // Read within the window: the first call at once.
    assert_eq!(next(t0 + READ_WINDOW - s(1), Some(t0), None), Next::Call);
    assert_eq!(next(t0, Some(t0), None), Next::Call);
    // Then once the part is a refresh old, waiting out the rest before.
    assert_eq!(
        next(t0 + s(300), Some(t0), Some(t0)),
        Next::Wait(REFRESH - s(300))
    );
    assert_eq!(next(t0, Some(t0), Some(t0)), Next::Wait(REFRESH));
    assert_eq!(next(t0 + REFRESH, Some(t0), Some(t0)), Next::Call);
    assert_eq!(
        next(t0 + REFRESH + s(1), Some(t0 + s(10)), Some(t0)),
        Next::Call
    );
    // A read or a part from a clock "after now" counts as now.
    assert_eq!(
        next(t0, Some(t0 + s(5)), Some(t0 + s(5))),
        Next::Wait(REFRESH)
    );
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
fn the_hook_is_called_only_when_the_status_is_read() {
    let calls = Arc::new(AtomicUsize::new(0));
    let hook = AppHook::default();
    assert_eq!(hook.read(), None, "no hook");
    hook.set(counted(&calls, r#"{"state":"ok"}"#));
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(calls.load(Ordering::Relaxed), 0, "nobody read");
    // The first read has no part yet; the thread calls the hook for it.
    let started = Instant::now();
    assert_eq!(hook.read(), None);
    count_reaches(&calls, 1);
    assert_eq!(
        read_until(&hook, Option::is_some).as_deref(),
        Some(r#"{"state":"ok"}"#)
    );
    // Within the read window it keeps refreshing, unread: not before a
    // refresh, and again after one.
    count_reaches(&calls, 2);
    assert!(started.elapsed() >= REFRESH, "{:?}", started.elapsed());
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
