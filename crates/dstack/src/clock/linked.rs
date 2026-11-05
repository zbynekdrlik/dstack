//! The linked clock (dstack #68, owner 2.11.2026): a dlink device's media
//! clock is dsync's in-process follower (dsync-ptp) when it is locked, and
//! the dsync service's (the DSYX poller's [`MonoClock`]) otherwise. It is
//! never worse than the service: when the follower cannot run (no
//! capability for port 319 on Linux, no Npcap on Windows) or is not locked
//! while the service is, the device runs on the service as before; and it
//! has a clock when the service does not run.
//!
//! A fresh follower takes over from a ready service only once settled
//! (dsync 1.27.3: locked in 9 s, but 20-60 µs off until its boot fit is
//! done, 37 s seeded, 70 s not), and it starts seeded with the service's
//! frequency word when the service is locked and settled ([`seed_of`]).
//!
//! A selector ([`Linked::tick`], once a second, off the media threads)
//! picks the source by [`choose`]; the media threads read only the atomic
//! state and the chosen clock.
//!
//! A change between the follower and the service is a handover
//! ([`handover`], v0.6.31, iemmixer 4.11.2026): the two trail the
//! grandmaster by the same receive latency, so when their times agree
//! within [`HANDOVER_NS`] the epoch stays and the media threads see one
//! small step of time, which the pump's and the TX flows' schedules absorb
//! (no re-align, nothing lost). When they disagree, a switch away from a
//! source that still works waits up to [`HANDOVER_WAIT`] rounds for them to
//! agree; then, or at once when the source in use is gone, the switch
//! changes the epoch and the media threads re-align as on a re-anchor.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use tracing::{info, warn};

use dsync_ptp::follower::model::ClockModel;

use super::{ClockCounts, FollowerCounts, MonoClock, NOT_READY_NAP_NS, PtpClock, monotonic_ns};

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
    pub(crate) fn from_u8(v: u8) -> Source {
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
    /// Its boot fit is done and its recent error small (dsync's
    /// `FollowerState::settled`).
    pub settled: bool,
    /// It has a PTP time (anchored; locked or in holdover).
    pub anchored: bool,
    /// Its re-anchors and new time bases so far.
    pub epoch: u32,
}

/// The source for a follower in `internal` (none when it did not start),
/// a service clock that is ready or not, whether the follower was locked
/// once, and the source in use: the follower when locked, and settled or
/// already in use or without a ready service; else the service when
/// ready; else the follower in holdover after a lock; else none.
pub fn choose(
    internal: Option<View>,
    service_ready: bool,
    was_locked: bool,
    current: Source,
) -> Source {
    match internal {
        Some(v)
            if v.alive
                && v.locked
                && (v.settled || current == Source::Internal || !service_ready) =>
        {
            Source::Internal
        }
        _ if service_ready => Source::Service,
        Some(v) if v.alive && v.anchored && was_locked => Source::Internal,
        _ => Source::None,
    }
}

/// Two sources whose times differ by at most this hand over without a
/// re-align (30 µs, dsync's settled bound: the follower takes over a ready
/// service only once settled). The step it leaves in media time is at most
/// 6 samples at 192 kHz, under half the smallest pump margin (W is at least
/// the block, 16 samples), and under the TX send slack every advertised
/// latency covers ([`crate::device::TX_SEND_SLACK_NS`], 50 µs; a step back
/// sends the next packets that much later, once). The step is measured
/// with the reads' own gap counted against it ([`Linked::tick`]).
pub const HANDOVER_NS: i64 = 30_000;
/// How many rounds (one a second) a switch away from a source that still
/// works waits for the two sources to agree before it re-aligns.
pub const HANDOVER_WAIT: u32 = 60;

// The step stays under half the tightest pump margin: 16 samples at 192 kHz.
const _: () = assert!(HANDOVER_NS * 192_000 * 2 < 16 * 1_000_000_000);
const _: () = assert!(HANDOVER_NS < crate::device::TX_SEND_SLACK_NS as i64);

/// How a change of source goes ([`handover`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handover {
    /// The times agree: the epoch stays.
    Seamless,
    /// They do not, and the source in use still works: keep it this round.
    Wait,
    /// A re-align: the epoch changes.
    Break,
}

/// A change of source with the new source's time `offset_ns` from the old
/// one's (`None`: one of them has no time), `voluntary` when the source in
/// use still works, after `waited` rounds of waiting: seamless within
/// [`HANDOVER_NS`]; else a wait while voluntary and under
/// [`HANDOVER_WAIT`]; else a break.
pub fn handover(offset_ns: Option<i64>, voluntary: bool, waited: u32) -> Handover {
    match offset_ns {
        Some(d) if d.unsigned_abs() <= HANDOVER_NS.unsigned_abs() => Handover::Seamless,
        _ if voluntary && waited < HANDOVER_WAIT => Handover::Wait,
        _ => Handover::Break,
    }
}

/// The new source's time `new` less the old one's, read `before` and
/// `after` it: the difference farther from 0, so the time between the reads
/// counts against agreement, never for it; `None` past `i64`.
pub fn offset_of(before: i128, new: i128, after: i128) -> Option<i64> {
    [new - before, new - after]
        .into_iter()
        .max_by_key(|d| d.unsigned_abs())
        .and_then(|d| i64::try_from(d).ok())
}

/// The first wait of a handover (with the offset).
pub const WAIT_LINE: &str = "media clock: the new source waits to agree with the one in use";
/// A wait that ended without a switch (the selector chose the source in
/// use again).
pub const WAIT_ENDED_LINE: &str = "media clock: the wait for a handover ended, no switch";
/// A handover without a re-align.
pub const SEAMLESS_LINE: &str = "media clock source handed over, no re-align";
/// A handover with a re-align.
pub const RE_ALIGN_LINE: &str = "media clock source changed with a re-align";
/// The first source after none.
pub const FIRST_SOURCE_LINE: &str = "media clock: a first source is ready";
/// No source ready any more (a warning).
pub const NO_SOURCE_LINE: &str = "media clock: no source ready";

/// The epoch the media threads see: the re-aligns (`switches`) in the high
/// half; the re-anchors since the last re-align in the low half (a seamless
/// handover carries them over to the new source).
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
    /// Its counts for the status: windows (as `polls`), re-anchors,
    /// holdovers, whether it is in holdover now, and the re-anchors
    /// absorbed.
    fn counts(&self) -> FollowerCounts;
}

/// What the media threads read of the selector's choice, in one word so a
/// reader never sees a new source with an old base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct State {
    /// The source in use.
    source: Source,
    /// The re-aligns so far (the epoch's high half).
    breaks: u16,
    /// The source's re-anchor count that reads as the low half's 0.
    base: u32,
}

impl State {
    fn pack(self) -> u64 {
        let [b0, b1] = self.breaks.to_be_bytes();
        let [c0, c1, c2, c3] = self.base.to_be_bytes();
        u64::from_be_bytes([0, self.source as u8, b0, b1, c0, c1, c2, c3])
    }

    fn unpack(w: u64) -> State {
        let [_, s, b0, b1, c0, c1, c2, c3] = w.to_be_bytes();
        State {
            source: Source::from_u8(s),
            breaks: u16::from_be_bytes([b0, b1]),
            base: u32::from_be_bytes([c0, c1, c2, c3]),
        }
    }
}

/// What [`Linked::tick`] measured at a switch.
struct Measured {
    offset_ns: Option<i64>,
    from_anchors: u32,
    to_anchors: u32,
}

/// The linked clock (see the module).
pub struct Linked {
    internal: Option<Box<dyn Follow>>,
    service: Arc<MonoClock>,
    /// [`State::pack`].
    state: AtomicU64,
    /// Rounds a voluntary switch has waited for agreement.
    waited: AtomicU32,
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
            state: AtomicU64::new(
                State {
                    source: Source::None,
                    breaks: 0,
                    base: 0,
                }
                .pack(),
            ),
            waited: AtomicU32::new(0),
            was_locked: AtomicBool::new(false),
            _keep: keep,
        }
    }

    fn state(&self) -> State {
        State::unpack(self.state.load(Ordering::Acquire))
    }

    /// The source in use.
    pub fn source(&self) -> Source {
        self.state().source
    }

    /// Whether the follower started.
    pub fn has_internal(&self) -> bool {
        self.internal.is_some()
    }

    /// The follower's counts ([`Follow::counts`]); `None` when it did not
    /// start.
    pub fn follower_counts(&self) -> Option<FollowerCounts> {
        self.internal.as_ref().map(|f| f.counts())
    }

    /// A source's re-anchor count (the follower's re-anchors and time
    /// bases, the service's re-anchors; 0 for none).
    fn component(&self, source: Source) -> u32 {
        match source {
            Source::Internal => self.internal.as_ref().map_or(0, |f| f.view().epoch),
            Source::Service => self.service.epoch(),
            Source::None => 0,
        }
    }

    /// A source's PTP time now, ns (`None`: it has none).
    fn time_of(&self, source: Source) -> Option<i128> {
        match source {
            Source::Internal => self.internal.as_ref()?.now_ns().map(i128::from),
            Source::Service => self.service.now_ns().map(i128::from),
            Source::None => None,
        }
    }

    /// A switch's measure: the two sources' re-anchor counts, read before
    /// their times, and `to`'s time less `from`'s ([`Linked::offset_ns`]),
    /// `None` when either re-aligned while it was read (a follower step the
    /// tracker absorbs moves no count: [`offset_of`]'s farther difference
    /// carries it).
    fn measure(&self, from: Source, to: Source) -> Measured {
        let from_anchors = self.component(from);
        let to_anchors = self.component(to);
        let offset_ns = self
            .offset_ns(from, to)
            .filter(|_| self.component(from) == from_anchors && self.component(to) == to_anchors);
        Measured {
            offset_ns,
            from_anchors,
            to_anchors,
        }
    }

    /// `to`'s time less `from`'s: `from` read before and after `to`, and the
    /// difference farther from 0 of the two, so the time between the reads
    /// (a preempted selector) counts against agreement, never for it.
    /// `None` when either has no time.
    fn offset_ns(&self, from: Source, to: Source) -> Option<i64> {
        let before = self.time_of(from)?;
        let new = self.time_of(to)?;
        let after = self.time_of(from)?;
        offset_of(before, new, after)
    }

    /// The selector's round: picks the source by [`choose`]; a change
    /// between the follower and the service is a [`handover`] (seamless,
    /// a wait, or a re-align that moves the epoch); every change is logged.
    pub fn tick(&self) -> Source {
        let view = self.internal.as_ref().map(|f| f.view());
        if view.is_some_and(|v| v.alive && v.locked) {
            self.was_locked.store(true, Ordering::Release);
        }
        let service_ready = self.service.k_ns().is_some();
        let s = self.state();
        let before = s.source;
        let next = choose(
            view,
            service_ready,
            self.was_locked.load(Ordering::Acquire),
            before,
        );
        if next == before {
            if self.waited.swap(0, Ordering::AcqRel) != 0 {
                info!(source = ?before, "{WAIT_ENDED_LINE}");
            }
            return next;
        }
        // From or to none there is no offset, and the source in use is not
        // a ready service: a break.
        let m = self.measure(before, next);
        let offset = m.offset_ns;
        let waited = self.waited.load(Ordering::Acquire);
        let way = handover(offset, before == Source::Service && service_ready, waited);
        // The counts the offset was measured with: a re-anchor after them
        // still moves the epoch.
        let low = m.from_anchors.wrapping_sub(s.base);
        let comp = m.to_anchors;
        let state = match way {
            Handover::Wait => {
                if waited == 0 {
                    info!(source = ?next, from = ?before, offset_ns = ?offset, "{WAIT_LINE}");
                }
                self.waited.store(waited.wrapping_add(1), Ordering::Release);
                return before;
            }
            Handover::Seamless => State {
                source: next,
                breaks: s.breaks,
                base: comp.wrapping_sub(low),
            },
            Handover::Break => State {
                source: next,
                breaks: s.breaks.wrapping_add(1),
                base: comp,
            },
        };
        self.state.store(state.pack(), Ordering::Release);
        self.waited.store(0, Ordering::Release);
        match (way, next) {
            (_, Source::None) => warn!(from = ?before, "{NO_SOURCE_LINE}"),
            (Handover::Seamless, _) => {
                info!(source = ?next, from = ?before, offset_ns = ?offset, "{SEAMLESS_LINE}")
            }
            _ if before != Source::None => {
                info!(source = ?next, from = ?before, offset_ns = ?offset, waited, "{RE_ALIGN_LINE}")
            }
            _ => info!(source = ?next, from = ?before, "{FIRST_SOURCE_LINE}"),
        }
        next
    }
}

impl PtpClock for Linked {
    fn now_ns(&self) -> Option<u64> {
        self.time_of(self.source())
            .and_then(|t| u64::try_from(t).ok())
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
        let s = self.state();
        epoch_of(
            u32::from(s.breaks),
            self.component(s.source).wrapping_sub(s.base),
        )
    }
}

/// The step of a re-anchor from the model `old` to `new`: `new`'s time at
/// its own anchor less `old`'s at that instant (`old`'s rate counted);
/// `None` when either has no anchor.
pub fn re_anchor_step(old: &ClockModel, new: &ClockModel) -> Option<i64> {
    let was = old.ptp_at(new.m0)?;
    new.ptp_at(new.m0).map(|now| now.wrapping_sub(was))
}

/// The follower's re-anchors sorted by their step (v0.6.31, iemmixer
/// 4.11.2026: a grandmaster reboot re-anchored it by +2557 ns, then -5710
/// ns, and each re-aligned the media threads): a change of dsync's
/// (epoch, reanchors) whose [`re_anchor_step`] is within [`HANDOVER_NS`] is
/// absorbed, any other a re-align. The service needs no such rule: its
/// tracker holds k for steps under [`super::TOLERANCE_NS`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Absorb {
    last: Option<ClockModel>,
    realigns: u32,
    absorbed: u32,
}

impl Absorb {
    /// The model now; returns the re-aligns and the absorbed so far.
    pub fn feed(&mut self, m: ClockModel) -> (u32, u32) {
        if let Some(old) = self.last
            && (old.epoch, old.reanchors) != (m.epoch, m.reanchors)
        {
            match re_anchor_step(&old, &m) {
                Some(d) if d.unsigned_abs() <= HANDOVER_NS.unsigned_abs() => {
                    self.absorbed = self.absorbed.wrapping_add(1);
                }
                _ => self.realigns = self.realigns.wrapping_add(1),
            }
        }
        self.last = Some(m);
        (self.realigns, self.absorbed)
    }
}

/// What the linked clock reads of dsync's follower.
pub trait Model: Send + Sync {
    /// The published model (a stale receive thread reads as holdover).
    fn model(&self) -> ClockModel;
    /// Its receive thread runs.
    fn alive(&self) -> bool;
    /// PTP time now, ns.
    fn now_ns(&self) -> Option<i64>;
}

/// dsync-ptp's reader handle.
impl Model for dsync_ptp::follower::PtpClock {
    fn model(&self) -> ClockModel {
        dsync_ptp::follower::PtpClock::model(self)
    }

    fn alive(&self) -> bool {
        dsync_ptp::follower::PtpClock::alive(self)
    }

    fn now_ns(&self) -> Option<i64> {
        self.now_ptp_ns()
    }
}

/// The follower as the linked clock reads it: its view's epoch counts only
/// the re-anchors to re-align for ([`Absorb`]). Whichever thread reads
/// first (the selector, the pump, the TX thread) feeds the tracker; one
/// that finds it busy reads the counts last published, so a media thread
/// never waits.
pub struct Absorbing<M> {
    source: M,
    pub(crate) absorb: std::sync::Mutex<Absorb>,
    /// The last (re-aligns, absorbed), packed.
    published: AtomicU64,
}

impl<M: Model> Absorbing<M> {
    pub fn new(source: M) -> Self {
        Absorbing {
            source,
            absorb: std::sync::Mutex::new(Absorb::default()),
            published: AtomicU64::new(0),
        }
    }

    /// The model now and the (re-aligns, absorbed) with it.
    ///
    /// The model is read under the lock, so the tracker sees models in the
    /// order dsync published them (a model read before the lock could reach
    /// it after a newer one and count one re-anchor three times). A reader
    /// that finds the lock taken reads its own model and the counts last
    /// published: on a re-align in that very step it may see the new time
    /// under the old epoch for one step (the pump then re-bootstraps on the
    /// jump itself, once more on the epoch), never for a step it absorbs.
    /// A poisoned lock is used as it is (`feed` cannot panic).
    fn read(&self) -> (ClockModel, u32, u32) {
        let held = match self.absorb.try_lock() {
            Ok(a) => Some(a),
            Err(std::sync::TryLockError::Poisoned(p)) => Some(p.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        };
        let m = self.source.model();
        if let Some(mut a) = held {
            let (r, ab) = a.feed(m);
            let [r0, r1, r2, r3] = r.to_be_bytes();
            let [a0, a1, a2, a3] = ab.to_be_bytes();
            self.published.store(
                u64::from_be_bytes([r0, r1, r2, r3, a0, a1, a2, a3]),
                Ordering::Release,
            );
        }
        let [r0, r1, r2, r3, a0, a1, a2, a3] = self.published.load(Ordering::Acquire).to_be_bytes();
        (
            m,
            u32::from_be_bytes([r0, r1, r2, r3]),
            u32::from_be_bytes([a0, a1, a2, a3]),
        )
    }
}

impl<M: Model> Follow for Absorbing<M> {
    fn now_ns(&self) -> Option<i64> {
        self.source.now_ns()
    }

    fn view(&self) -> View {
        let (m, realigns, _) = self.read();
        View {
            alive: self.source.alive(),
            locked: m.locked,
            settled: m.settled,
            anchored: m.locked || m.holdover,
            epoch: realigns,
        }
    }

    /// dsync-ptp counts phase-lock windows (it has no polls), re-anchors
    /// (every discontinuity of its published time) and holdovers.
    fn counts(&self) -> FollowerCounts {
        let (m, _, absorbed) = self.read();
        FollowerCounts {
            counts: ClockCounts {
                polls: m.windows,
                re_anchors: m.reanchors,
                holdovers: m.holdovers,
                in_holdover: m.holdover,
            },
            absorbed,
            locked: m.locked,
            settled: m.settled,
            offset_ns: m.offset_ns,
            recent_abs_e_ns: m.recent_abs_e_ns,
        }
    }
}

/// The follower's frequency seed from a service reply: its frequency word
/// (DSYX bytes 36-39, ppb, as dsync's `with_frequency_seed` takes it) when
/// the service is locked and settled; a word still pulling in is no seed.
pub fn seed_of(r: &super::dsyx::Reading) -> Option<i32> {
    (r.locked && r.settled).then_some(r.freq_ppb)
}

/// The service's seed now ([`seed_of`]): one DSYX request to `target`
/// ([`super::DSYNC`] on the box), at most its reply timeout; `None` when
/// the service does not answer or is not locked and settled.
pub async fn service_seed(target: std::net::SocketAddr) -> Option<i32> {
    let mut poller = super::Poller::connect(target).await.ok()?;
    let polled = poller
        .once(&MonoClock::new(), &std::sync::atomic::AtomicI32::new(0))
        .await;
    polled.reading.as_ref().and_then(seed_of)
}

/// Starts dsync's follower on the interface `iface`, seeded with `seed_ppb`
/// ([`seed_of`]) when given: the linked clock's internal source, or `None`
/// with the reason logged (Linux: no capability for port 319; Windows: no
/// Npcap; anywhere: no such interface): the device then runs on the
/// service.
pub fn start_internal(iface: &str, seed_ppb: Option<i32>) -> Started {
    let mut cfg = dsync_ptp::follower::FollowerConfig::on_interface(iface);
    if let Some(ppb) = seed_ppb {
        cfg = cfg.with_frequency_seed(i64::from(ppb));
    }
    match dsync_ptp::follower::Follower::start(cfg) {
        Ok(f) => {
            info!(
                iface,
                seed_ppb, "media clock: dsync's in-process follower started"
            );
            (Some(Box::new(Absorbing::new(f.clock()))), Some(Box::new(f)))
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
mod absorb_tests;
#[cfg(test)]
mod tests;
