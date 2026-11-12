// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The device's place while it is not there yet (dstack #225, #230): an
//! address no interface holds, an interface without its IPv4 address. A
//! start takes it at once or fails ([`PlaceWait::Now`], dlink's
//! `Link::start`), or waits for it ([`PlaceWait::Forever`], the device
//! binaries). A wait looks the place up every [`PLACE_RETRY`] and logs at
//! its start, once a minute and when the place is there, naming the kind
//! of place, never the address or the interface (P6).

use std::io;
use std::time::Duration;

use tracing::{info, warn};

use super::{IfAddr, Place, find_interface, pick_interface, refuse_unreachable};

/// How often a wait looks a place up again.
pub const PLACE_RETRY: Duration = Duration::from_secs(1);
/// Every how many lookups a wait is logged again: once a minute.
pub const PLACE_LOG_EVERY: u32 = 60;

/// How a start meets a place not there yet (dstack #230).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceWait {
    /// Fails at once with [`pick_interface`]'s error, as every start did
    /// up to v0.6.97: dlink's `Link::start`, whose apps retry and stop on
    /// their own.
    Now,
    /// Waits with no time limit ([`wait_for_interface`], dstack #225): the
    /// device binaries (dstack-dev, dsend, dlisten) and `embed::Embedded`
    /// (dasio, iemmixer).
    Forever,
}

/// The place `wait` gives: [`pick_interface`]'s choice from one look at
/// `addrs` (the box's interfaces) for [`PlaceWait::Now`], else
/// [`wait_for_interface`]'s.
pub fn resolve_place(
    place: &Place,
    wait: PlaceWait,
    mut addrs: impl FnMut() -> io::Result<Vec<IfAddr>>,
    sleep: impl FnMut(Duration),
) -> Result<IfAddr, String> {
    match wait {
        PlaceWait::Now => pick_interface(place, &addrs().map_err(interfaces_unread)?),
        PlaceWait::Forever => wait_for_interface(place, addrs, sleep),
    }
}

/// The error of an interface list that cannot be read.
fn interfaces_unread(e: io::Error) -> String {
    format!("the box's interfaces: {e}")
}

/// What a wait logs for its kind of place.
struct Waited {
    /// What is waited for ("the device's address").
    what: &'static str,
    /// The wait's line.
    waiting: &'static str,
}

impl Waited {
    fn of(place: &Place) -> Waited {
        match place {
            Place::Ip(_) => Waited {
                what: "the device's address",
                waiting: "waiting for the device's address to appear on an interface \
                          (looked up every second, no time limit)",
            },
            Place::Iface(_) => Waited {
                what: "the device's interface",
                waiting: "waiting for the device's interface to appear with an IPv4 \
                          address (looked up every second, no time limit)",
            },
        }
    }
}

/// The device's place as [`pick_interface`] picks it, waited for (dstack
/// #225): an address no interface holds yet, or an interface not there or
/// without its IPv4 address yet, is looked up again in `addrs` (the box's
/// interfaces) after `sleep(PLACE_RETRY)`, with no time limit, so a device
/// started at boot or logon comes up whenever its network does. The wait
/// is logged at its start, once a minute and when the place is there,
/// naming the kind of place, never the address or the interface (P6). Any
/// other refusal ([`pick_interface`]'s) and an interface list that cannot
/// be read fail at once.
pub fn wait_for_interface(
    place: &Place,
    mut addrs: impl FnMut() -> io::Result<Vec<IfAddr>>,
    sleep: impl FnMut(Duration),
) -> Result<IfAddr, String> {
    // An address the Dante network cannot reach never comes: refused
    // before any lookup.
    if let Place::Ip(ip) = place {
        refuse_unreachable(*ip)?;
    }
    wait_until(
        &Waited::of(place),
        || find_interface(place, &addrs().map_err(interfaces_unread)?),
        sleep,
    )
}

/// `lookup` until it finds its thing (`Some`), `sleep(PLACE_RETRY)`
/// between two looks, logged as `words` says. A lookup's error ends the
/// wait at once.
fn wait_until<T>(
    words: &Waited,
    mut lookup: impl FnMut() -> Result<Option<T>, String>,
    mut sleep: impl FnMut(Duration),
) -> Result<T, String> {
    let mut tries: u32 = 0;
    loop {
        if let Some(found) = lookup()? {
            if tries > 0 {
                info!(
                    seconds = tries,
                    "{} is there now: the device starts", words.what
                );
            }
            return Ok(found);
        }
        if tries.is_multiple_of(PLACE_LOG_EVERY) {
            warn!(minutes = tries / PLACE_LOG_EVERY, "{}", words.waiting);
        }
        sleep(PLACE_RETRY);
        tries = tries.saturating_add(1);
    }
}
