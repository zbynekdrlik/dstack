// SPDX-License-Identifier: MIT OR Apache-2.0

//! A TEST RUN serves its devices on loopback only (dstack #366).
//!
//! dev1's whole-device tests ran their devices on the box's routed address
//! (the LAN leg), so each test binary announced mDNS there (about 35
//! datagrams a second) and its info and heartbeat multicasts went out with
//! it; dasio found dev1 among the top mDNS senders on the FOH PC's Dante leg
//! in the minutes its NIC stalled sends (dasio#127). Loopback multicast works
//! on Linux and Windows and never leaves the box, but the host refuses
//! loopback in production (no MAC, unreachable for Dante).
//!
//! So a process is a test run when [`LOOPBACK_ENV`] is `1` (this
//! workspace's `.cargo/config.toml` sets it for every process cargo runs:
//! `cargo test`, nextest, cargo-mutants, CI). In a test run a device on a
//! loopback address is served there with a locally administered MAC
//! ([`loopback_net`]), and a device on any other address the lookup finds
//! is refused before anything is bound ([`TestRun::admit`]), unless
//! [`LIVE_ENV`] is `1` (an explicitly live test on a real network). Without
//! the variable (production, and every consumer: a git dependency's
//! `.cargo` is never read) nothing changes.

use std::ffi::OsString;
use std::net::Ipv4Addr;

use crate::daemon::{IfAddr, Place};
use crate::info::NetInfo;

/// `1`: this process is a test run (devices on loopback only).
pub const LOOPBACK_ENV: &str = "DSTACK_TEST_LOOPBACK";
/// `1`: a test run that may serve on a real network (a live test).
pub const LIVE_ENV: &str = "DSTACK_LIVE_TEST";

/// The refusal of a site address in a test run (names no address).
pub const SITE_REFUSED: &str = "a test run (DSTACK_TEST_LOOPBACK=1) serves a device on a loopback \
     address only: on any other its mDNS, info and heartbeat multicasts reach the site's network. \
     Use 127.0.0.1, or set DSTACK_LIVE_TEST=1 for a live test";

/// What the environment says about this process.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TestRun {
    /// A test run: devices on loopback only.
    pub loopback: bool,
    /// An explicitly live test: a site address is allowed.
    pub live: bool,
}

impl TestRun {
    /// The process's own environment.
    pub fn from_env() -> TestRun {
        TestRun::from_vars(|key| std::env::var_os(key))
    }

    /// From `get` (a variable's value, `None` when unset): each flag is set
    /// by the exact value `1`.
    pub fn from_vars(get: impl Fn(&str) -> Option<OsString>) -> TestRun {
        let on = |key| get(key).is_some_and(|v| v == "1");
        TestRun {
            loopback: on(LOOPBACK_ENV),
            live: on(LIVE_ENV),
        }
    }

    /// A loopback address served as it is in a test run (no interface is
    /// looked up; the production lookup refuses loopback); `None` for every
    /// other place, and outside a test run: the production lookup.
    pub fn loopback(self, place: &Place) -> Option<IfAddr> {
        match place {
            Place::Ip(ip) if self.loopback && ip.is_loopback() => Some(IfAddr {
                name: "lo".into(),
                ip: *ip,
                netmask: Ipv4Addr::new(255, 0, 0, 0),
            }),
            _ => None,
        }
    }

    /// The production lookup's answer `found` as this run takes it: a test
    /// run refuses a site address ([`SITE_REFUSED`]) unless it is live. A
    /// place not there yet is not judged (the lookup waits or fails as in
    /// production), so the guard refuses only an address the device would
    /// really serve on.
    pub fn admit(self, found: IfAddr) -> Result<IfAddr, String> {
        if self.loopback && !self.live && !found.ip.is_loopback() {
            return Err(SITE_REFUSED.into());
        }
        Ok(found)
    }
}

/// The network info of a test run's loopback device: a locally
/// administered unicast MAC holding the address (`02:00:` and its four
/// bytes; the loopback's own MAC is all zeros, which no device id may be),
/// the address's netmask, no gateway, no link speed.
pub fn loopback_net(ifa: &IfAddr) -> NetInfo {
    let [a, b, c, d] = ifa.ip.octets();
    NetInfo {
        mac: [0x02, 0x00, a, b, c, d],
        ip: ifa.ip,
        netmask: ifa.netmask,
        gateway: Ipv4Addr::UNSPECIFIED,
        link_speed: 0,
    }
}

#[cfg(test)]
mod tests;
