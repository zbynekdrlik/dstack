// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The device's place while it is not there yet (dstack #225, #230): an
//! address no interface holds, an interface without its IPv4 address, the
//! default route's address on a box with no route yet ([`wait_for_route`]). A
//! start takes it at once or fails ([`PlaceWait::Now`], dlink's
//! `Link::start`), waits for it ([`PlaceWait::Forever`], the device
//! binaries), or waits until the app stops it ([`PlaceWait::Until`],
//! dlink's `Link::start_until`). A wait looks the place up every
//! [`PLACE_RETRY`] and logs at its start, once a minute and when the place
//! is there (or the app stopped it), naming the kind of place, never the
//! address or the interface (P6).

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tracing::{info, warn};

use super::{IfAddr, Place, find_interface, pick_interface, refuse_unreachable};

/// How often a wait looks a place up again.
pub const PLACE_RETRY: Duration = Duration::from_secs(1);
/// Every how many lookups a wait is logged again: once a minute.
pub const PLACE_LOG_EVERY: u32 = 60;

/// How a start meets a place not there yet (dstack #230).
#[derive(Debug, Clone, Copy)]
pub enum PlaceWait<'a> {
    /// Fails at once with [`pick_interface`]'s error, as every start did
    /// up to v0.6.97: dlink's `Link::start`, whose apps retry and stop on
    /// their own.
    Now,
    /// Waits with no time limit ([`wait_for_interface`], dstack #225): the
    /// device binaries (dstack-dev, dsend, dlisten) and `embed::Embedded`
    /// (dasio, iemmixer).
    Forever,
    /// Waits as [`PlaceWait::Forever`] until the flag is set, then fails
    /// with "stopped while waiting for the device's address" (or "..
    /// interface") within one [`PLACE_RETRY`] (dlink's `Link::start_until`).
    /// The flag ends a wait only: a place there is taken even with it set.
    Until(&'a AtomicBool),
}

/// The place `wait` gives: [`pick_interface`]'s choice from one look at
/// `addrs` (the box's interfaces) for [`PlaceWait::Now`], else
/// [`wait_for_interface`]'s, stopped by [`PlaceWait::Until`]'s flag.
pub fn resolve_place(
    place: &Place,
    wait: PlaceWait<'_>,
    mut addrs: impl FnMut() -> io::Result<Vec<IfAddr>>,
    sleep: impl FnMut(Duration),
) -> Result<IfAddr, String> {
    match wait {
        PlaceWait::Now => pick_interface(place, &addrs().map_err(interfaces_unread)?),
        PlaceWait::Forever => wait_on(place, addrs, sleep, None),
        PlaceWait::Until(stop) => wait_on(place, addrs, sleep, Some(stop)),
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
    /// How it comes ("to appear on an interface").
    comes: &'static str,
}

impl Waited {
    fn of(place: &Place) -> Waited {
        match place {
            Place::Ip(_) => Waited {
                what: "the device's address",
                comes: "to appear on an interface",
            },
            Place::Iface(_) => Waited {
                what: "the device's interface",
                comes: "to appear with an IPv4 address",
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
    addrs: impl FnMut() -> io::Result<Vec<IfAddr>>,
    sleep: impl FnMut(Duration),
) -> Result<IfAddr, String> {
    wait_on(place, addrs, sleep, None)
}

/// [`wait_for_interface`], ended by `stop` when one is given.
fn wait_on(
    place: &Place,
    mut addrs: impl FnMut() -> io::Result<Vec<IfAddr>>,
    sleep: impl FnMut(Duration),
    stop: Option<&AtomicBool>,
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
        stop,
    )
}

/// What a wait for the default route's address logs.
const ROUTE: Waited = Waited {
    what: "the default route's address",
    comes: "to appear",
};

/// The default route's address (dsend and dlisten given no `--ip` or
/// `--iface`), waited for as [`wait_for_interface`] waits (dstack #230): a
/// box at boot has no default route yet. `route` looks it up once
/// ([`route_answer`]: `None` while it is not there); its error fails at
/// once.
pub fn wait_for_route(
    route: impl FnMut() -> Result<Option<Ipv4Addr>, String>,
    sleep: impl FnMut(Duration),
) -> Result<Ipv4Addr, String> {
    wait_until(&ROUTE, route, sleep, None)
}

/// The default route's address from a UDP socket's `connect` and
/// `local_addr` (`connected`): `None` while there is no route (the
/// network unreachable, the host unreachable, no address to send from)
/// or no address given yet (0.0.0.0); an IPv6 address and any other error
/// are refused.
pub fn route_answer(connected: io::Result<SocketAddr>) -> Result<Option<Ipv4Addr>, String> {
    match connected {
        Ok(SocketAddr::V4(a)) if a.ip().is_unspecified() => Ok(None),
        Ok(SocketAddr::V4(a)) => Ok(Some(*a.ip())),
        Ok(other) => Err(format!("no IPv4 address on the default route ({other})")),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NetworkUnreachable
                    | io::ErrorKind::HostUnreachable
                    | io::ErrorKind::AddrNotAvailable
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(format!("the default route: {e}")),
    }
}

/// `lookup` until it finds its thing (`Some`), `sleep(PLACE_RETRY)`
/// between two looks, logged as `words` says. A lookup's error ends the
/// wait at once; so does `stop`, once set, after the next look.
fn wait_until<T>(
    words: &Waited,
    mut lookup: impl FnMut() -> Result<Option<T>, String>,
    mut sleep: impl FnMut(Duration),
    stop: Option<&AtomicBool>,
) -> Result<T, String> {
    let limit = match stop {
        Some(_) => "until the app stops the start",
        None => "no time limit",
    };
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
        if stop.is_some_and(|s| s.load(Ordering::Relaxed)) {
            let why = format!("stopped while waiting for {}", words.what);
            info!(seconds = tries, "{why}");
            return Err(why);
        }
        if tries.is_multiple_of(PLACE_LOG_EVERY) {
            warn!(
                minutes = tries / PLACE_LOG_EVERY,
                "waiting for {} {} (looked up every second, {limit})", words.what, words.comes
            );
        }
        sleep(PLACE_RETRY);
        tries = tries.saturating_add(1);
    }
}
