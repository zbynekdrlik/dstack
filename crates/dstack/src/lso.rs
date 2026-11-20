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
/// Measured on the [`MEASURED_FAMILY`] only (dstack #383).
pub const REALTEK_BAD: [&str; 2] = ["10.79.20.1003", "10.72.524.2024"];
/// Realtek driver versions measured clean on the same PCs.
pub const REALTEK_GOOD: [&str; 1] = ["10.39.212.2020"];
/// The adapter family both lists were measured on, as its `DriverDesc`
/// names it ("Realtek Gaming 2.5GbE Family Controller"): the same version
/// on the 1 GbE family measured clean (dsg, 17.11.2026; dstack #383).
pub const MEASURED_FAMILY: &str = "2.5gbe";

/// What the start line says about the NIC's driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Driver {
    /// A Realtek driver in [`REALTEK_BAD`].
    KnownBad,
    /// A Realtek driver in [`REALTEK_GOOD`].
    KnownGood,
    /// Another Realtek driver: never measured, neither good nor bad.
    Unverified,
    /// A VLAN virtual adapter (dstack #155: the Dante leg as a Realtek
    /// VLAN on the PC's one NIC): its driver is the VLAN layer's, not the
    /// NIC's, and is not judged.
    Virtual,
    /// Not a Realtek driver, or not read.
    Other,
}

/// Whether an adapter's `DriverDesc` names a VLAN virtual adapter, as
/// Realtek's Ethernet Diagnostic Utility creates one per VLAN ("Realtek
/// Virtual Adapter", dstack #155).
pub fn virtual_adapter(description: Option<&str>) -> bool {
    description.is_some_and(|d| d.to_ascii_lowercase().contains("virtual adapter"))
}

/// The verdict on a [`MEASURED_FAMILY`] adapter's driver from its
/// `ProviderName` and `DriverVersion` (the network class key's values).
pub fn driver(provider: Option<&str>, version: Option<&str>) -> Driver {
    let realtek = provider.is_some_and(|p| p.to_ascii_lowercase().contains("realtek"));
    match version.map(str::trim) {
        Some(v) if realtek && REALTEK_BAD.contains(&v) => Driver::KnownBad,
        Some(v) if realtek && REALTEK_GOOD.contains(&v) => Driver::KnownGood,
        Some(_) if realtek => Driver::Unverified,
        _ => Driver::Other,
    }
}

/// The verdict on an adapter's driver from its `ProviderName`,
/// `DriverVersion` and `DriverDesc` (dstack #383): [`driver`] on the
/// [`MEASURED_FAMILY`]; on another family, or one not read, a measured
/// version is only unverified.
pub fn driver_of(
    provider: Option<&str>,
    version: Option<&str>,
    description: Option<&str>,
) -> Driver {
    let measured = description.is_some_and(|d| d.to_ascii_lowercase().contains(MEASURED_FAMILY));
    match driver(provider, version) {
        Driver::KnownBad | Driver::KnownGood if !measured => Driver::Unverified,
        v => v,
    }
}

pub mod standard;

/// What the start line knows of the Dante NIC's driver (the network class
/// key's values, the driver file from its service's `ImagePath`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriverFacts {
    /// `ProviderName` (`Realtek`, `Microsoft`).
    pub provider: Option<String>,
    /// `MatchingDeviceId` (`PCI\VEN_10EC&DEV_8125&...`, `RTK_VLANMP`).
    pub hardware_id: Option<String>,
    /// The driver file (`rt640x64.sys`).
    pub file: Option<String>,
    /// `DriverVersion` (`10.79.20.1003`).
    pub version: Option<String>,
    /// `DriverDesc` (`Realtek Gaming 2.5GbE Family Controller`).
    pub description: Option<String>,
}

/// The start line's verdict on the Dante NIC's driver against the NIC
/// standard ([`standard`], dstack #466).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverVerdict<'a> {
    /// The driver is the entry's: an allowed file, at or above its minimum.
    Standard(&'a standard::Entry),
    /// A driver the entry names bad.
    Bad(&'a standard::Entry, &'a standard::Bad),
    /// Not one of the entry's driver files.
    File(&'a standard::Entry),
    /// Under the entry's minimum version (or no version read).
    Old(&'a standard::Entry),
    /// A Realtek adapter the standard has no entry for.
    Unknown,
    /// A VLAN virtual adapter the standard has no entry for: its driver is
    /// the VLAN layer's, the NIC's own is not read.
    Virtual,
    /// Not a Realtek adapter, or not read.
    Other,
}

/// The verdict on the driver `facts` against the standard `std`.
pub fn driver_verdict<'a>(std: &'a standard::Standard, facts: &DriverFacts) -> DriverVerdict<'a> {
    // Today's verdict: the lists above, on the measured family (#466 RED).
    let entry = facts
        .hardware_id
        .as_deref()
        .and_then(|h| std.windows_entry(h));
    let old = driver_of(
        facts.provider.as_deref(),
        facts.version.as_deref(),
        facts.description.as_deref(),
    );
    match (old, entry) {
        (Driver::KnownBad, Some(e)) => DriverVerdict::Old(e),
        (Driver::Other, _) => DriverVerdict::Other,
        (_, Some(e)) => DriverVerdict::Standard(e),
        (_, None) => DriverVerdict::Unknown,
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
