// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! What the stack knows of the dsync service from its own poller (dstack
//! #429, the clock figures on the info port): the last poll's outcome and
//! the last reply with the moment it came. Nothing more: dsync's phase
//! error and its `:8898` status are not read here (the DSYX reply carries
//! neither).

use super::{Reading, Sample};

/// What the last poll of the service gave the tracker ([`Sample`], k
/// left out).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollOutcome {
    /// A reply whose k the tracker may use.
    Usable,
    /// No reply within the poll's timeout.
    NoReply,
    /// A reply, dsync not locked.
    NotLocked,
    /// A reply without a date offset.
    NoDateState,
    /// A reply within the guard of a date step.
    StepNear,
    /// A reply while a date slew runs.
    Slewing,
}

impl PollOutcome {
    /// The outcome of the sample `s`.
    pub fn of(s: Sample) -> PollOutcome {
        match s {
            Sample::Usable { .. } => PollOutcome::Usable,
            Sample::NoReply => PollOutcome::NoReply,
            Sample::NotLocked => PollOutcome::NotLocked,
            Sample::NoDateState => PollOutcome::NoDateState,
            Sample::StepNear => PollOutcome::StepNear,
            Sample::Slewing => PollOutcome::Slewing,
        }
    }
}

/// The service as the poller read it last ([`super::ClockStats::service`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServiceRead {
    /// The last poll's outcome; `None` before the first poll (and on a
    /// device whose clock polls no service: the test system clock).
    pub outcome: Option<PollOutcome>,
    /// The last reply and the monotonic time it came, ns
    /// ([`super::monotonic_ns`]); `None` before the first. A poll without a
    /// reply keeps it.
    pub reply: Option<(Reading, i64)>,
}

impl ServiceRead {
    /// One poll: its sample `s`, and its reply `reading` read at the
    /// monotonic time `at_ns`.
    pub fn polled(&mut self, s: Sample, reading: Option<Reading>, at_ns: i64) {
        self.outcome = Some(PollOutcome::of(s));
        if let Some(r) = reading {
            self.reply = Some((r, at_ns));
        }
    }
}
