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
//! It also reads the adapter's driver (hardware id, provider, driver file
//! and version) and judges it by the Realtek NIC standard
//! (`docs/nic-standard.json`, dstack #465, embedded: [`standard`]): a
//! driver the standard names bad, another file than the standard's, a
//! version under its minimum, or a Realtek adapter without an entry warns;
//! the standard's driver is an info line ([`driver_verdict`],
//! [`driver_line`], dstack #466). Why a driver is bad (the 20-30 ms receive
//! DPC of rt640x64 10.72, dasio#1) is the standard's evidence text, printed
//! with the warning.

use std::cmp::Ordering;

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

pub mod standard;

/// What the start line knows of the Dante NIC's driver (the network class
/// key's values, the driver file from its service's `ImagePath`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriverFacts {
    /// `ProviderName` (`Realtek`, `Microsoft`).
    pub provider: Option<String>,
    /// The device's first hardware id (`Enum\<DeviceInstanceID>\HardwareID`,
    /// as the checkers read it: `PCI\VEN_10EC&DEV_8125&...`, `RTK_VLANMP`),
    /// or the class key's `MatchingDeviceId` when that is not read.
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
    /// A Realtek adapter (a Realtek VLAN adapter too) the standard has no
    /// entry for.
    Unknown,
    /// Not a Realtek adapter, or not read.
    Other,
}

/// The verdict on the driver `facts` against the standard `std`: the
/// Windows entry for the adapter's hardware id judges it as the checkers
/// do (`judge_driver` in `tools/linux/nic-standard-check.py`); without an
/// entry, a Realtek adapter has no standard (a warning, as the checkers
/// fail it) and another vendor's is not judged.
pub fn driver_verdict<'a>(std: &'a standard::Standard, facts: &DriverFacts) -> DriverVerdict<'a> {
    match facts
        .hardware_id
        .as_deref()
        .and_then(|h| std.windows_entry(h))
    {
        Some(entry) => judge(entry, facts),
        None if realtek(facts) => DriverVerdict::Unknown,
        None => DriverVerdict::Other,
    }
}

/// The hardware id prefixes of a Realtek adapter, as the Windows checker
/// tells one (`Test-RealtekAdapter`): its PCI or USB vendor id, or its VLAN
/// adapter.
const REALTEK_IDS: [&str; 3] = [r"PCI\VEN_10EC&", r"USB\VID_0BDA&", "RTK_"];

/// Whether the adapter is a Realtek one, as the Windows checker tells it:
/// its description names Realtek, or its hardware id starts with a Realtek
/// prefix ([`REALTEK_IDS`]; the inbox driver's provider is Microsoft);
/// also a Realtek provider.
fn realtek(facts: &DriverFacts) -> bool {
    let upper = |v: &Option<String>| v.as_deref().map(|s| s.trim().to_ascii_uppercase());
    let named = |v: &Option<String>| upper(v).is_some_and(|s| s.contains("REALTEK"));
    named(&facts.provider)
        || named(&facts.description)
        || upper(&facts.hardware_id).is_some_and(|h| REALTEK_IDS.iter().any(|p| h.starts_with(p)))
}

/// The entry's verdict on a driver: a bad one (its file, its exact
/// version, or both), then the allowed files (any case), then the minimum
/// version.
fn judge<'a>(entry: &'a standard::Entry, facts: &DriverFacts) -> DriverVerdict<'a> {
    let file = facts.file.as_deref().map(str::trim);
    let version = facts.version.as_deref().map(str::trim);
    let is_file = |name: &str| file.is_some_and(|f| f.eq_ignore_ascii_case(name));
    let rule = &entry.driver;
    if let Some(bad) = rule.bad.iter().find(|b| {
        b.file.as_deref().is_none_or(is_file)
            && b.version.as_deref().is_none_or(|v| version == Some(v))
    }) {
        return DriverVerdict::Bad(entry, bad);
    }
    if !rule.files.iter().any(|f| is_file(f)) {
        return DriverVerdict::File(entry);
    }
    if let Some(min) = &rule.min_version
        && !version.is_some_and(|v| standard::compare_versions(v, min) != Ordering::Less)
    {
        return DriverVerdict::Old(entry);
    }
    DriverVerdict::Standard(entry)
}

/// One start-line sentence about the NIC: a warning or an info line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// A warning (else an info line).
    pub warn: bool,
    /// The sentence.
    pub text: String,
}

/// The start line's sentence for a driver verdict (none for another
/// vendor's adapter). A warning names the standard's driver and its
/// evidence (or, while the rule is open, the plan that decides it).
pub fn driver_line(verdict: &DriverVerdict<'_>, facts: &DriverFacts) -> Option<Line> {
    let file = facts.file.as_deref().map_or("(file not read)", str::trim);
    let version = facts
        .version
        .as_deref()
        .map_or("(version not read)", str::trim);
    let want = |e: &standard::Entry| {
        let files = e.driver.files.join(" or ");
        match &e.driver.min_version {
            Some(min) => format!("{files} {min} or newer"),
            None => files,
        }
    };
    let note = |e: &standard::Entry| match (e.driver.status.as_str(), &e.driver.plan) {
        ("open", Some(plan)) => format!("open: {plan}"),
        _ => e.driver.evidence.clone(),
    };
    let line = |warn: bool, text: String| Some(Line { warn, text });
    match verdict {
        DriverVerdict::Standard(e) => line(
            false,
            format!(
                "the Dante NIC's driver {file} {version} is the NIC standard's for the {} ({}, docs/nic-standard.json)",
                e.chip, e.id
            ),
        ),
        DriverVerdict::Bad(e, bad) => line(
            true,
            format!(
                "the Dante NIC's driver {file} {version} is one the NIC standard names bad for the {} ({}): {}; install {} (docs/nic-standard.md)",
                e.chip,
                e.id,
                bad.evidence,
                want(e)
            ),
        ),
        DriverVerdict::File(e) => line(
            true,
            format!(
                "the Dante NIC's driver {file} {version} is not the NIC standard's for the {} ({}): install {} (docs/nic-standard.md); {}",
                e.chip,
                e.id,
                want(e),
                note(e)
            ),
        ),
        DriverVerdict::Old(e) => line(
            true,
            format!(
                "the Dante NIC's driver {file} {version} is under the NIC standard's minimum for the {} ({}): install {} (docs/nic-standard.md); {}",
                e.chip,
                e.id,
                want(e),
                note(e)
            ),
        ),
        DriverVerdict::Unknown => line(
            true,
            format!(
                "the Dante NIC is a Realtek adapter ({}) the NIC standard has no entry for: its driver {file} {version} was never measured; measure it and add it to docs/nic-standard.json",
                facts
                    .hardware_id
                    .as_deref()
                    .unwrap_or("hardware id not read")
            ),
        ),
        DriverVerdict::Other => None,
    }
}

#[cfg(windows)]
mod windows;
#[cfg(all(windows, test))]
use windows::sys;
#[cfg(windows)]
pub use windows::{check, driver_facts, report};

#[cfg(test)]
mod tests;
