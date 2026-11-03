//! The linked clock (dstack #68, owner 2.11.2026): a dlink device's media
//! clock is dsync's in-process follower (dsync-ptp) when it is locked, and
//! the dsync service's (the DSYX poller's [`MonoClock`]) otherwise. It is
//! never worse than the service: when the follower cannot run (no
//! capability for port 319 on Linux, no Npcap on Windows) or is not locked
//! while the service is, the device runs on the service as before; and it
//! has a clock when the service does not run.
//!
//! A selector ([`Linked::tick`], once a second, off the media threads)
//! picks the source by [`choose`]; the media threads read only the atomic
//! choice and the chosen clock. A change of source changes the epoch, so
//! the media threads re-align as on a re-anchor.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::time::Duration;

use tracing::{info, warn};

use super::{MonoClock, NOT_READY_NAP_NS, PtpClock, monotonic_ns};

/// Where the linked clock takes PTP time from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Source {
    /// Nothing ready yet.
    None = 0,
    /// The in-process follower (dsync-ptp).
    Internal = 1,
    /// The dsync service on the PC (DSYX).
    Service = 2,
}

impl Source {
    fn from_u8(v: u8) -> Source {
        match v {
            1 => Source::Internal,
            2 => Source::Service,
            _ => Source::None,
        }
    }
}

/// What the selector reads of the follower.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct View {
    /// Its receive thread runs.
    pub alive: bool,
    /// Locked (DSYX byte 41's rule: |e| ≤ 50 µs for 3 windows).
    pub locked: bool,
    /// It has a PTP time (anchored; locked or in holdover).
    pub anchored: bool,
    /// Its re-anchors and new time bases so far.
    pub epoch: u32,
}

/// The source for a follower in `internal` (none when it did not start),
/// a service clock that is ready or not, and whether the follower was
/// locked once: the follower when locked; else the service when ready;
/// else the follower in holdover after a lock; else none.
pub fn choose(internal: Option<View>, service_ready: bool, was_locked: bool) -> Source {
    match internal {
        Some(v) if v.alive && v.locked => Source::Internal,
        _ if service_ready => Source::Service,
        Some(v) if v.alive && v.anchored && was_locked => Source::Internal,
        _ => Source::None,
    }
}

/// The epoch the media threads see: a change of source always changes the
/// high half; the chosen clock's own re-anchors the low half.
pub fn epoch_of(switches: u32, component: u32) -> u32 {
    // `wrapping_add` of disjoint halves: an `|` would leave its `^` mutant
    // equal (.claude/rules/tests.md).
    (switches << 16).wrapping_add(component & 0xffff)
}

/// A started follower's clock and the value that keeps it running (both
/// `None` when it did not start).
pub type Started = (Option<Box<dyn Follow>>, Option<Box<dyn Send + Sync>>);

/// The follower as the linked clock reads it.
pub trait Follow: Send + Sync {
    /// PTP time now, ns; `None` until anchored.
    fn now_ns(&self) -> Option<i64>;
    /// Its state for the selector.
    fn view(&self) -> View;
}

/// The linked clock (see the module).
pub struct Linked {
    internal: Option<Box<dyn Follow>>,
    service: Arc<MonoClock>,
    choice: AtomicU8,
    switches: AtomicU32,
    was_locked: AtomicBool,
    /// Keeps the follower's thread alive for the clock's life.
    _keep: Option<Box<dyn Send + Sync>>,
}

impl Linked {
    /// A linked clock over the follower `internal` (none when it could not
    /// start) and the service's clock; `keep` holds the follower running.
    pub fn new(
        internal: Option<Box<dyn Follow>>,
        service: Arc<MonoClock>,
        keep: Option<Box<dyn Send + Sync>>,
    ) -> Linked {
        Linked {
            internal,
            service,
            choice: AtomicU8::new(Source::None as u8),
            switches: AtomicU32::new(0),
            was_locked: AtomicBool::new(false),
            _keep: keep,
        }
    }

    /// The source in use.
    pub fn source(&self) -> Source {
        Source::from_u8(self.choice.load(Ordering::Acquire))
    }

    /// Whether the follower started.
    pub fn has_internal(&self) -> bool {
        self.internal.is_some()
    }

    /// The selector's round: picks the source by [`choose`]; a change is
    /// logged and counted (the epoch moves).
    pub fn tick(&self) -> Source {
        let view = self.internal.as_ref().map(|f| f.view());
        if view.is_some_and(|v| v.alive && v.locked) {
            self.was_locked.store(true, Ordering::Release);
        }
        let next = choose(
            view,
            self.service.k_ns().is_some(),
            self.was_locked.load(Ordering::Acquire),
        );
        let before = Source::from_u8(self.choice.swap(next as u8, Ordering::AcqRel));
        if before != next {
            self.switches.fetch_add(1, Ordering::AcqRel);
            match next {
                Source::None => warn!(from = ?before, "media clock: no source ready"),
                _ => info!(source = ?next, from = ?before, "media clock source"),
            }
        }
        next
    }
}

impl PtpClock for Linked {
    fn now_ns(&self) -> Option<u64> {
        match self.source() {
            Source::Internal => self
                .internal
                .as_ref()?
                .now_ns()
                .and_then(|t| u64::try_from(t).ok()),
            Source::Service => self.service.now_ns(),
            Source::None => None,
        }
    }

    fn sleep_until(&self, ptp_ns: u64) {
        match self.source() {
            Source::Service => self.service.sleep_until(ptp_ns),
            Source::Internal => match self.now_ns() {
                Some(now) => {
                    let left = i64::try_from(ptp_ns.saturating_sub(now)).unwrap_or(i64::MAX);
                    super::sys::sleep_until_monotonic(monotonic_ns().saturating_add(left));
                }
                None => std::thread::sleep(Duration::from_nanos(NOT_READY_NAP_NS)),
            },
            Source::None => std::thread::sleep(Duration::from_nanos(NOT_READY_NAP_NS)),
        }
    }

    fn epoch(&self) -> u32 {
        let component = match self.source() {
            Source::Internal => self.internal.as_ref().map_or(0, |f| f.view().epoch),
            Source::Service => self.service.epoch(),
            Source::None => 0,
        };
        epoch_of(self.switches.load(Ordering::Acquire), component)
    }
}

/// dsync's in-process follower (dsync-ptp) as the linked clock reads it.
impl Follow for dsync_ptp::follower::PtpClock {
    fn now_ns(&self) -> Option<i64> {
        self.now_ptp_ns()
    }

    fn view(&self) -> View {
        let s = self.state();
        View {
            alive: self.alive(),
            locked: s.locked,
            anchored: s.locked || s.holdover,
            epoch: s.epoch.wrapping_add(s.reanchors),
        }
    }
}

/// Starts dsync's follower on the interface `iface`: the linked clock's
/// internal source, or `None` with the reason logged (Linux: no capability
/// for port 319; Windows: no Npcap; anywhere: no such interface): the
/// device then runs on the service.
pub fn start_internal(iface: &str) -> Started {
    match dsync_ptp::follower::Follower::start(dsync_ptp::follower::FollowerConfig::on_interface(
        iface,
    )) {
        Ok(f) => {
            info!(iface, "media clock: dsync's in-process follower started");
            (Some(Box::new(f.clock())), Some(Box::new(f)))
        }
        Err(e) => {
            warn!(
                iface,
                reason = follower_failure(&e),
                error = %e,
                "media clock: dsync's in-process follower did not start: the dsync service's clock is used"
            );
            (None, None)
        }
    }
}

/// What the app can do about a follower that did not start, in a word.
pub fn follower_failure(e: &dsync_ptp::follower::FollowerError) -> &'static str {
    use dsync_ptp::follower::FollowerError;
    match e {
        FollowerError::NpcapMissing { .. } => "Npcap is not installed",
        FollowerError::PermissionDenied { .. } => "port 319 needs CAP_NET_BIND_SERVICE or root",
        FollowerError::InterfaceNotFound { .. } => "no such interface, or no IPv4 on it",
        _ => "another failure",
    }
}

#[cfg(test)]
mod tests;
