// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The status's `clock` object ([`super::status`](fn@super::status)): the
//! media clock's state, the counts of the source in use, and a linked
//! clock's counts per source.

use std::sync::atomic::Ordering;

use super::status::Json;
use crate::clock::ClockCounts;
use crate::clock::linked::Source;
use crate::daemon::media::Media;

/// The clock status: the counts of the source in use on top
/// ([`clock_counts_in_use`]), and for a linked clock each source's apart
/// (`service`: the poller's polls; `follower`: dsync's follower's windows,
/// and its `locked`, `settled`, `offset_ns` and `recent_abs_e_ns`, what the
/// selector decides on, and `pull_ns`, the phase dsync still retires after
/// a grandmaster change in the same time base).
pub(super) fn clock_part(j: &mut Json, media: &Media) {
    let linked = media.clock_stats.linked();
    let service = media.clock_stats.counts();
    let follower = media.clock_stats.follower();
    let c = clock_counts_in_use(linked, service, follower.map(|f| f.counts));
    j.open(Some("clock"), '{');
    j.flag("ready", media.clock.now_ns().is_some());
    j.num("epoch", media.clock.epoch());
    j.num("polls", c.polls);
    j.num("re_anchors", c.re_anchors);
    j.num("holdovers", c.holdovers);
    j.flag("holdover", c.in_holdover);
    j.num("freq_ppb", media.freq.load(Ordering::Relaxed));
    j.word(
        "source",
        clock_source_word(media.clock_stats.is_system(), linked),
    );
    if linked.is_some() {
        j.open(Some("service"), '{');
        counts_part(j, "polls", service);
        j.close('}');
        if let Some(f) = follower {
            j.open(Some("follower"), '{');
            counts_part(j, "windows", f.counts);
            j.num("absorbed", f.absorbed);
            j.flag("locked", f.locked);
            j.flag("settled", f.settled);
            j.opt("offset_ns", f.offset_ns);
            j.opt("recent_abs_e_ns", f.recent_abs_e_ns);
            j.num("pull_ns", f.pull_ns);
            j.close('}');
        }
    }
    j.close('}');
}

/// One source's counts, its polls named `polls_key`.
fn counts_part(j: &mut Json, polls_key: &str, c: ClockCounts) {
    j.num(polls_key, c.polls);
    j.num("re_anchors", c.re_anchors);
    j.num("holdovers", c.holdovers);
    j.flag("holdover", c.in_holdover);
}

/// The counts the clock status shows on top: the follower's while a linked
/// clock uses it, else the dsync service poller's (iemmixer 4.11.2026: the
/// poller's holdover showed while the device ran on the follower).
pub(crate) fn clock_counts_in_use(
    linked: Option<Source>,
    service: ClockCounts,
    follower: Option<ClockCounts>,
) -> ClockCounts {
    match (linked, follower) {
        (Some(Source::Internal), Some(f)) => f,
        _ => service,
    }
}

/// The clock source's word in the status: `system` (tests), `dsync` (the
/// service only), else what a linked clock uses now: `internal` (dsync's
/// follower in process), `service`, or `none` (nothing ready yet).
pub fn clock_source_word(system: bool, linked: Option<Source>) -> &'static str {
    match (system, linked) {
        (true, _) => "system",
        (false, None) => "dsync",
        (false, Some(Source::Internal)) => "internal",
        (false, Some(Source::Service)) => "service",
        (false, Some(Source::None)) => "none",
    }
}
