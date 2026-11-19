//! Starting dsync's follower, and starting it again (dstack #431, mbc-snv
//! 19.11.2026: 4 s after logon Npcap had no device for the pinned adapter
//! yet, the start failed once, nothing tried it again, and the device ran
//! without its follower until it was restarted).
//!
//! A start that failed for a cause that can pass ([`start_passes`]: no
//! Npcap device for the adapter yet, the adapter without an IPv4 yet, the
//! capture not opened) is tried again on the selector's round
//! ([`super::Linked::tick`]) after [`retry_delay_s`]: 1, 2, 4, 8, 16, then
//! every 30 s. A cause that cannot pass (no Npcap installed, no capability
//! for port 319, no adapter of that name) stays failed, as before. While
//! it tries, the follower's status reads `stopped` and the once-a-minute
//! line names the tries; still failing [`ESCALATE_AFTER_S`] after the
//! first failure, one warning says so, and the tries go on for ever. Once
//! it starts, the selector takes it the usual way (it agrees with the
//! service, or waits to).

use dsync_ptp::follower::FollowerError;

use std::sync::Arc;

use tracing::{info, warn};

use super::{
    Absorbing, Follow, Linked, Minute, MonoClock, NOT_IN_USE_LINE, NOT_STARTED_LINE, RETRY_LINE,
    Report, STARTED_LINE, STILL_NOT_STARTED_LINE, Source, Waiting, follower_failure, monotonic_ns,
};

/// What one start of the follower gave.
pub enum Attempt {
    /// It runs: its clock and the value that keeps it running.
    Started(Box<dyn Follow>, Box<dyn Send + Sync>),
    /// It did not start.
    Failed {
        /// The cause can pass: try again ([`start_passes`]).
        passes: bool,
        /// What the app can do about it, in a word ([`follower_failure`]).
        reason: &'static str,
        /// The error's text.
        error: String,
    },
}

/// The run that starts a follower on an interface with a seed.
type Run = Box<dyn FnMut(&str, Option<i32>) -> Attempt + Send>;

/// Starts the follower on `iface`, seeded with `seed_ppb`, as often as the
/// linked clock asks ([`attempt`] for dsync's).
pub struct Starter {
    pub(super) iface: String,
    pub(super) seed_ppb: Option<i32>,
    run: Run,
}

impl Starter {
    /// A starter that runs `run` with the interface and the seed.
    pub fn new(
        iface: impl Into<String>,
        seed_ppb: Option<i32>,
        run: impl FnMut(&str, Option<i32>) -> Attempt + Send + 'static,
    ) -> Starter {
        Starter {
            iface: iface.into(),
            seed_ppb,
            run: Box::new(run),
        }
    }

    /// dsync's follower ([`attempt`]).
    pub fn dsync(iface: impl Into<String>, seed_ppb: Option<i32>) -> Starter {
        Starter::new(iface, seed_ppb, attempt)
    }

    /// One start.
    pub(super) fn attempt(&mut self) -> Attempt {
        (self.run)(&self.iface, self.seed_ppb)
    }
}

/// One start of dsync's follower on the interface `iface`, seeded with
/// `seed_ppb` ([`super::seed_of`]) when given.
pub fn attempt(iface: &str, seed_ppb: Option<i32>) -> Attempt {
    let mut cfg = dsync_ptp::follower::FollowerConfig::on_interface(iface);
    if let Some(ppb) = seed_ppb {
        cfg = cfg.with_frequency_seed(i64::from(ppb));
    }
    match dsync_ptp::follower::Follower::start(cfg) {
        Ok(f) => Attempt::Started(Box::new(Absorbing::new(f.clock())), Box::new(f)),
        Err(e) => failed(&e),
    }
}

/// A failed start's [`Attempt`]: whether its cause can pass, its word and
/// its text.
pub fn failed(e: &FollowerError) -> Attempt {
    Attempt::Failed {
        passes: start_passes(e),
        reason: follower_failure(e),
        error: e.to_string(),
    }
}

/// Whether a start that failed with `e` can succeed later without a change
/// on the PC (dsync-ptp v1.33.1's kinds): no, when Npcap is not installed
/// (`NpcapMissing`), port 319 is refused (`PermissionDenied`) or no adapter
/// has the name (`InterfaceNotFound` of a missing NIC); yes, when the
/// adapter has no IPv4 yet (`InterfaceNotFound` of a present one) or the
/// capture did not open (`Other`: on Windows `no Npcap device carries <ip>`
/// while Npcap binds to a fresh adapter at boot). A kind dsync adds
/// later stays failed.
pub fn start_passes(e: &FollowerError) -> bool {
    match e {
        FollowerError::InterfaceNotFound { cause, .. } => !cause
            .downcast_ref::<dsync_ptp::net::PinnedInterfaceUnavailable>()
            .is_some_and(|p| p.missing),
        FollowerError::Other(_) => true,
        _ => false,
    }
}

/// The longest wait between two tries, s.
pub const RETRY_MAX_S: i64 = 30;

/// The wait after `failures` failed starts, s: 1, 2, 4, 8, 16, then
/// [`RETRY_MAX_S`] (a first failure waits 1 s).
pub fn retry_delay_s(failures: u32) -> i64 {
    (1_i64 << failures.saturating_sub(1).min(5)).min(RETRY_MAX_S)
}

/// How long the tries run before one warning says the cause did not pass,
/// s (10 minutes; the tries go on).
pub const ESCALATE_AFTER_S: i64 = 600;

const S: i64 = 1_000_000_000;

/// A follower that did not start yet, for a cause that can pass: the
/// selector's to try again ([`super::Linked::tick`]).
pub(super) struct Retry {
    pub(super) starter: Starter,
    /// The first failure's monotonic time, ns.
    first_ns: i64,
    /// The starts that failed so far.
    pub(super) attempts: u32,
    /// The next try's monotonic time, ns.
    next_ns: i64,
    /// The last failure's word ([`follower_failure`]).
    pub(super) reason: &'static str,
    /// The last failure's text.
    pub(super) error: String,
    /// How long the last try took, ms (the selector's round waits for it;
    /// 0 for the first, which the start timed itself).
    pub(super) start_ms: u128,
    /// [`STILL_NOT_STARTED_LINE`] was logged.
    escalated: bool,
}

impl Retry {
    /// The tries after a first failure at `now_ns` for `reason` with
    /// `error`.
    pub(super) fn new(starter: Starter, now_ns: i64, reason: &'static str, error: String) -> Retry {
        Retry {
            starter,
            first_ns: now_ns,
            attempts: 1,
            next_ns: now_ns.saturating_add(retry_delay_s(1) * S),
            reason,
            error,
            start_ms: 0,
            escalated: false,
        }
    }

    /// Whether a try is due at `now_ns`.
    pub(super) fn due(&self, now_ns: i64) -> bool {
        now_ns >= self.next_ns
    }

    /// A try at `now_ns` that took `start_ms` failed again for `reason`
    /// with `error`.
    pub(super) fn failed(
        &mut self,
        now_ns: i64,
        reason: &'static str,
        error: String,
        start_ms: u128,
    ) {
        self.attempts = self.attempts.saturating_add(1);
        self.next_ns = now_ns.saturating_add(retry_delay_s(self.attempts) * S);
        self.reason = reason;
        self.error = error;
        self.start_ms = start_ms;
    }

    /// Whole seconds since the first failure at `now_ns`.
    pub(super) fn retrying_for_s(&self, now_ns: i64) -> i64 {
        now_ns.saturating_sub(self.first_ns) / S
    }

    /// Whether [`STILL_NOT_STARTED_LINE`] is due at `now_ns`: the first time
    /// the tries have run [`ESCALATE_AFTER_S`], and never again.
    pub(super) fn escalate(&mut self, now_ns: i64) -> bool {
        if self.escalated || self.retrying_for_s(now_ns) < ESCALATE_AFTER_S {
            return false;
        }
        self.escalated = true;
        true
    }
}

impl Linked {
    /// A linked clock over the follower `start` starts on the service's
    /// clock: the device then runs on the service until the follower is
    /// in use. A start that fails for a cause that can pass is tried again
    /// by the selector ([`Linked::tick`], [`retry_delay_s`]); one that
    /// cannot pass is not.
    pub fn start(service: Arc<MonoClock>, start: Starter) -> Linked {
        Linked::start_at(service, start, monotonic_ns())
    }

    /// [`Linked::start`] at the monotonic time `now_ns` (a test's own).
    pub(crate) fn start_at(service: Arc<MonoClock>, mut start: Starter, now_ns: i64) -> Linked {
        let took = std::time::Instant::now();
        let attempt = start.attempt();
        let start_ms = took.elapsed().as_millis();
        match attempt {
            Attempt::Started(f, keep) => {
                info!(
                    iface = %start.iface,
                    seed_ppb = ?start.seed_ppb,
                    start_attempts = 1,
                    start_ms,
                    "{STARTED_LINE}"
                );
                Linked::new(Some(f), service, Some(keep))
            }
            Attempt::Failed {
                passes: false,
                reason,
                error,
            } => {
                warn!(
                    iface = %start.iface,
                    reason,
                    error = %error,
                    start_ms,
                    "{NOT_STARTED_LINE}"
                );
                Linked::new(None, service, None)
            }
            Attempt::Failed {
                passes: true,
                reason,
                error,
            } => {
                warn!(
                    iface = %start.iface,
                    reason,
                    error = %error,
                    retry_in_s = retry_delay_s(1),
                    start_ms,
                    "{RETRY_LINE}"
                );
                let linked = Linked::new(None, service, None);
                linked.lock_report().retry = Some(Retry::new(start, now_ns, reason, error));
                linked
            }
        }
    }

    /// The selector's try of a follower start that is due at `now_ns`: a
    /// follower that starts is the selector's from this round on; a try
    /// that fails for a cause that can pass waits its next delay, one that
    /// cannot ends the tries ([`NOT_STARTED_LINE`]).
    pub(super) fn try_start(&self, now_ns: i64) {
        let mut r = self.lock_report();
        let Some(retry) = r.retry.as_mut().filter(|t| t.due(now_ns)) else {
            return;
        };
        let attempts = retry.attempts.saturating_add(1);
        let iface = retry.starter.iface.clone();
        let took = std::time::Instant::now();
        let attempt = retry.starter.attempt();
        let start_ms = took.elapsed().as_millis();
        match attempt {
            Attempt::Started(f, keep) => {
                info!(
                    iface = %iface,
                    seed_ppb = ?retry.starter.seed_ppb,
                    start_attempts = attempts,
                    retrying_for_s = retry.retrying_for_s(now_ns),
                    start_ms,
                    "{STARTED_LINE}"
                );
                // The keeper first: whoever sees the follower sees it kept.
                // Neither is set while the tries run.
                let _ = self.keep.set(keep);
                let _ = self.internal.set(f);
                r.retry = None;
                // Its own wait starts its own minute: never the tries' minute
                // with a fresh follower's 0 windows (a `no_ptp` line in the
                // round it started).
                r.minute = Minute::default();
            }
            Attempt::Failed {
                passes: true,
                reason,
                error,
            } => retry.failed(now_ns, reason, error, start_ms),
            Attempt::Failed {
                passes: false,
                reason,
                error,
            } => {
                warn!(
                    iface = %iface,
                    reason,
                    error = %error,
                    start_attempts = attempts,
                    start_ms,
                    "{NOT_STARTED_LINE}"
                );
                r.retry = None;
            }
        }
    }

    /// While the follower's start is tried again: [`NOT_IN_USE_LINE`] once
    /// a minute with the reason `stopped`, the interface, the starts so far,
    /// the seconds since the first failed, and the last failure's word,
    /// text and duration; once, [`ESCALATE_AFTER_S`] after the first
    /// failure, [`STILL_NOT_STARTED_LINE`] (a warning) with the same.
    pub(super) fn report_tries(&self, now_ns: i64, source: Source) {
        let mut r = self.lock_report();
        let Report { minute, retry, .. } = &mut *r;
        let Some(retry) = retry.as_mut() else {
            return;
        };
        if retry.escalate(now_ns) {
            warn!(
                iface = %retry.starter.iface,
                start_attempts = retry.attempts,
                retrying_for_s = retry.retrying_for_s(now_ns),
                start_failure = retry.reason,
                error = %retry.error,
                start_ms = retry.start_ms,
                "{STILL_NOT_STARTED_LINE}"
            );
        }
        if let (_, Some(_)) = minute.round(now_ns, 0, true) {
            info!(
                reason = %Waiting::Stopped.word(),
                iface = %retry.starter.iface,
                start_attempts = retry.attempts,
                retrying_for_s = retry.retrying_for_s(now_ns),
                start_failure = retry.reason,
                error = %retry.error,
                start_ms = retry.start_ms,
                source = ?source,
                "{NOT_IN_USE_LINE}"
            );
        }
    }
}
