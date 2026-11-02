//! The Dante NIC's large send offload, checked at start on Windows (dstack
//! #1, the FOH PC, 1.11.2026). With LSO on, another sender's large TCP
//! send leaves the NIC as one long burst, and our packets wait behind it:
//! dasio's A/B on the FOH PC counted 184 gaps in 30 s with LSO on and 117
//! with it off.
//!
//! The device reads the adapter's `*LsoV2IPv4` and `*LsoV1IPv4` settings
//! (the values Windows' advanced NIC properties keep in the registry, under
//! the network class key, by the adapter's GUID) and warns when either is
//! on. It never changes the NIC: that is the PC owner's step (the link
//! resets when applied).
//!
//! It also reads the adapter's driver (`ProviderName`, `DriverVersion`)
//! and warns on a Realtek driver measured with a long receive DPC
//! ([`REALTEK_BAD`]); one never measured gets an info line ([`driver`]).

/// What the start line says about the NIC's large send offload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// LSO is off (every version the adapter has).
    Off,
    /// LSO is on for IPv4 (v2 or v1).
    On,
    /// Not read: the adapter keeps no such setting, or it was not found.
    Unknown,
}

/// The verdict on an adapter's `*LsoV2IPv4` and `*LsoV1IPv4` values (each
/// `"1"` when on, `"0"` when off, absent when the adapter has none).
pub fn verdict(v2: Option<&str>, v1: Option<&str>) -> Verdict {
    let on = |v: Option<&str>| v.map(|s| s.trim() == "1");
    match (on(v2), on(v1)) {
        (Some(true), _) | (_, Some(true)) => Verdict::On,
        (None, None) => Verdict::Unknown,
        _ => Verdict::Off,
    }
}

/// Realtek driver versions (as `Get-NetAdapter` and the registry's
/// `DriverVersion` show them) measured with a 20 to 30 ms receive DPC that
/// held every send completion about every 96 to 130 s: late packets at a
/// receiver and failed sends (dasio#1, the FOH PC, 2.11.2026; RTL8125).
pub const REALTEK_BAD: [&str; 2] = ["10.79.20.1003", "10.72.524.2024"];
/// Realtek driver versions measured clean on the same PCs.
pub const REALTEK_GOOD: [&str; 1] = ["10.39.212.2020"];

/// What the start line says about the NIC's driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Driver {
    /// A Realtek driver in [`REALTEK_BAD`].
    KnownBad,
    /// A Realtek driver in [`REALTEK_GOOD`].
    KnownGood,
    /// Another Realtek driver: never measured, neither good nor bad.
    Unverified,
    /// Not a Realtek driver, or not read.
    Other,
}

/// The verdict on an adapter's driver from its `ProviderName` and
/// `DriverVersion` (the network class key's values).
pub fn driver(provider: Option<&str>, version: Option<&str>) -> Driver {
    let realtek = provider.is_some_and(|p| p.to_ascii_lowercase().contains("realtek"));
    match version.map(str::trim) {
        Some(v) if realtek && REALTEK_BAD.contains(&v) => Driver::KnownBad,
        Some(v) if realtek && REALTEK_GOOD.contains(&v) => Driver::KnownGood,
        Some(_) if realtek => Driver::Unverified,
        _ => Driver::Other,
    }
}

#[cfg(windows)]
mod windows;
#[cfg(all(windows, test))]
use windows::sys;
#[cfg(windows)]
pub use windows::{check, check_driver, report};

#[cfg(test)]
mod tests;
