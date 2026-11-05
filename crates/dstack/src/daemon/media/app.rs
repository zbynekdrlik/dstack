// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The app's own part of the status, off the device's control task (dstack
//! #104 step 5).
//!
//! - [`AppHook::set`] stores the app's hook and starts, once, the
//!   `dstack-app-status` thread. That thread calls the hook when its last
//!   part is [`REFRESH`] old or older and someone read the status within
//!   [`READ_WINDOW`] ([`next`]): nobody reading means no calls.
//! - [`AppHook::read`] (the status request) returns the cached part and
//!   never calls the hook, so a slow hook never delays the device's ARC,
//!   CMC or status answers. A part older than [`STALE`] carries
//!   `"stale":true` ([`mark_stale`]); a hook still running then is logged
//!   once as stalled.
//! - A hook that panics leaves the last part (stale in time) and the
//!   thread running: it is called again at the next refresh, and a new hook
//!   takes over; the panic is warned once for each hook.
//! - The thread ends with the [`AppHook`] (its owner, the `Media`): a stop
//!   flag it checks between calls. A call in progress is never cut short.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tracing::{info, warn};

/// How often the thread calls the hook at most.
pub const REFRESH: Duration = Duration::from_secs(1);
/// The thread calls the hook only while a status was read within this.
pub const READ_WINDOW: Duration = Duration::from_secs(5);
/// A part older than this carries `"stale":true`.
pub const STALE: Duration = Duration::from_secs(3);

/// An app's status hook ([`crate::daemon::media::Media::set_app_status`]).
pub type AppStatus = Arc<dyn Fn() -> String + Send + Sync>;

/// What the thread does next ([`next`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// Call the hook now.
    Call,
    /// Wait this long (or until woken), then decide again.
    Wait(Duration),
    /// Nobody reads: wait until woken (a read, a new hook, the stop).
    Idle,
}

/// The thread's decision at `now`, given the last status read (`read`) and
/// when the last part was stored (`at`, `None` before the first call of
/// this hook).
pub fn next(now: Instant, read: Option<Instant>, at: Option<Instant>) -> Next {
    let Some(read) = read else {
        return Next::Idle;
    };
    if now.saturating_duration_since(read) >= READ_WINDOW {
        return Next::Idle;
    }
    match at {
        Some(at) if now.saturating_duration_since(at) < REFRESH => {
            Next::Wait(REFRESH - now.saturating_duration_since(at))
        }
        _ => Next::Call,
    }
}

/// A part stored at `at` is stale at `now`.
pub fn is_stale(now: Instant, at: Instant) -> bool {
    now.saturating_duration_since(at) > STALE
}

/// `part` (one compact JSON object, [`crate::ctl::app_object`]) with
/// `"stale":true` set (over any `stale` key of the app's own), compact.
pub fn mark_stale(part: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(part) {
        Ok(serde_json::Value::Object(mut obj)) => {
            obj.insert("stale".into(), serde_json::Value::Bool(true));
            serde_json::Value::Object(obj).to_string()
        }
        _ => r#"{"stale":true}"#.to_string(),
    }
}

/// What the readers and the thread share.
#[derive(Default)]
struct State {
    hook: Option<AppStatus>,
    /// Bumped by each new hook: a call of the hook it replaced stores
    /// nothing.
    generation: u64,
    /// The last part the hook returned (`None`: refused, no JSON object
    /// within the size) and when that call returned.
    part: Option<(Option<String>, Instant)>,
    /// When the hook's last call ended, returned or panicked: the
    /// refresh's clock ([`next`]).
    called: Option<Instant>,
    /// The hook panicked (since it was set).
    panicked: bool,
    /// The last status read.
    read: Option<Instant>,
    /// The thread is inside a call of the hook.
    calling: bool,
    /// The thread was started.
    started: bool,
    /// The owner is gone: the thread ends.
    stop: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

fn lock(m: &Mutex<State>) -> MutexGuard<'_, State> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The app's hook, its thread and its cached part (see the module).
#[derive(Default)]
pub struct AppHook {
    shared: Arc<Shared>,
    /// The hook gave text that is no JSON object, and that was warned.
    refused: AtomicBool,
    /// The hook was found stalled, and that was warned.
    stalled: AtomicBool,
    /// The hook was found to have panicked, and that was warned.
    panic_warned: AtomicBool,
}

impl AppHook {
    /// Sets the hook (a later call replaces it, its old part dropped and a
    /// refusal, a stall or a panic of the new one warned again); starts the
    /// thread at the first call. A hook replaced while its call stalls
    /// holds the thread: the new one is first called once the old call
    /// returns, and the status has no app part until then.
    pub fn set(&self, f: AppStatus) {
        let start = {
            let mut s = lock(&self.shared.state);
            s.hook = Some(f);
            s.generation += 1;
            s.part = None;
            s.called = None;
            s.panicked = false;
            // Under the lock: a reader of the old hook's part cannot warn
            // after the reset and leave the new hook's warning spent.
            self.refused.store(false, Ordering::Relaxed);
            self.stalled.store(false, Ordering::Relaxed);
            self.panic_warned.store(false, Ordering::Relaxed);
            !std::mem::replace(&mut s.started, true)
        };
        self.shared.wake.notify_all();
        if start {
            let shared = Arc::clone(&self.shared);
            match std::thread::Builder::new()
                .name("dstack-app-status".into())
                .spawn(move || run(&shared))
            {
                Ok(_) => info!("the app's status thread started"),
                Err(e) => warn!(
                    error = %e,
                    "the app's status thread did not start; the status leaves the app part out"
                ),
            }
        }
    }

    /// The cached part as compact JSON, `"stale":true` set once it is older
    /// than [`STALE`]; `None` without a hook, before the hook's first call
    /// returned, or when its text was no JSON object within
    /// [`crate::ctl::APP_MAX`] bytes (warned the first time for each hook).
    /// Never calls the hook: it counts as a read, which keeps the thread
    /// calling it ([`next`]).
    pub fn read(&self) -> Option<String> {
        let now = Instant::now();
        let (part, calling, panicked) = {
            let mut s = lock(&self.shared.state);
            s.hook.as_ref()?;
            s.read = Some(now);
            (s.part.clone(), s.calling, s.panicked)
        };
        self.shared.wake.notify_all();
        // Warned here, on the reader's thread, like the other warnings.
        if panicked && !self.panic_warned.swap(true, Ordering::Relaxed) {
            warn!(
                "the app's status hook panicked; the status keeps its last part, the hook is called again at each refresh (warned once)"
            );
        }
        let (text, at) = part?;
        let Some(text) = text else {
            if !self.refused.swap(true, Ordering::Relaxed) {
                warn!(
                    max_bytes = crate::ctl::APP_MAX,
                    "the app's status part is no JSON object within the size; the status leaves it out (warned once)"
                );
            }
            return None;
        };
        if !is_stale(now, at) {
            return Some(text);
        }
        if calling && !self.stalled.swap(true, Ordering::Relaxed) {
            warn!(
                stale_after_ms = STALE.as_millis(),
                "the app's status hook has not returned; the status carries its last part as stale (warned once)"
            );
        }
        Some(mark_stale(&text))
    }
}

impl Drop for AppHook {
    fn drop(&mut self) {
        lock(&self.shared.state).stop = true;
        self.shared.wake.notify_all();
    }
}

/// The thread: calls the hook as [`next`] decides, until the stop.
fn run(shared: &Shared) {
    let mut s = lock(&shared.state);
    while !s.stop {
        match (next(Instant::now(), s.read, s.called), s.hook.clone()) {
            (Next::Call, Some(hook)) => {
                let generation = s.generation;
                s.calling = true;
                drop(s);
                let text = catch_unwind(AssertUnwindSafe(|| hook()));
                s = lock(&shared.state);
                s.calling = false;
                if s.generation != generation {
                    continue;
                }
                let now = Instant::now();
                s.called = Some(now);
                match text {
                    Ok(text) => s.part = Some((crate::ctl::app_object(&text), now)),
                    Err(_) => s.panicked = true,
                }
            }
            (Next::Wait(d), _) => {
                s = shared
                    .wake
                    .wait_timeout(s, d)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
            _ => s = shared.wake.wait(s).unwrap_or_else(PoisonError::into_inner),
        }
    }
}

/// An app's command hook ([`crate::daemon::media::Media::set_app_command`]):
/// the words of a control request `app <words>`, answered `Ok` (done) or
/// `Err` (why not).
pub type AppCommand = Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

/// The app's command hook (dstack #139): set once or replaced, called on
/// the control task that got the request.
#[derive(Default)]
pub struct CommandHook {
    hook: Mutex<Option<AppCommand>>,
}

impl CommandHook {
    /// Sets the hook (a later call replaces it).
    pub fn set(&self, f: AppCommand) {
        *self.hook.lock().unwrap_or_else(PoisonError::into_inner) = Some(f);
    }

    /// The hook's answer to `words`, `None` without a hook. The hook runs
    /// outside the lock; a panic in it is an `Err` (warned), never the
    /// control task's.
    pub fn call(&self, words: &str) -> Option<Result<String, String>> {
        let f = self
            .hook
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()?;
        Some(
            catch_unwind(AssertUnwindSafe(|| f(words))).unwrap_or_else(|_| {
                warn!("the app's command hook panicked");
                Err(PANICKED.to_string())
            }),
        )
    }
}

/// The answer to a command whose hook panicked.
pub const PANICKED: &str = "the app's command failed";

#[cfg(test)]
mod tests;
