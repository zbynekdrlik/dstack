//! The Realtek NIC standard (`docs/nic-standard.json`, dstack #465) as the
//! start line reads it (dstack #466): the file decides which driver a NIC
//! should run, and the start line follows it instead of a list of its own.
//!
//! The file is embedded at build time from a copy under the crate
//! (`crates/dstack/nic-standard.json`): a git dependency builds from the
//! whole checkout, but `cargo package` and `cargo vendor` keep only the
//! crate's own files, so a path into `docs/` would break those builds. A
//! test keeps the copy byte-equal to `docs/nic-standard.json`, the source.
//!
//! Only the fields the start line reads are parsed; the file's own
//! validation is the checkers' (`tools/linux/nic-standard-check.py`,
//! `tools/windows/nic-standard-check.ps1`), which CI runs on it.

use std::cmp::Ordering;
use std::sync::OnceLock;

use serde::Deserialize;

/// The standard's file, a copy of `docs/nic-standard.json`.
pub const FILE: &str = include_str!("../../nic-standard.json");

/// The standard: its entries, one per chip and OS.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Standard {
    /// The file's version (raised by one on every change).
    pub version: u32,
    /// The entries.
    pub adapters: Vec<Entry>,
}

/// One chip on one OS.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Entry {
    /// The entry's id (`rtl8125-rtl8168-windows`).
    pub id: String,
    /// The chip (`RTL8125 / RTL8168`).
    pub chip: String,
    /// `windows` or `linux`.
    pub os: String,
    /// What the entry matches.
    #[serde(rename = "match")]
    pub matches: Match,
    /// The driver rule.
    pub driver: DriverRule,
}

/// The ids an entry matches.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Match {
    /// Windows: prefixes of the device's hardware id (any case).
    #[serde(default)]
    pub hardware_id: Vec<String>,
}

/// An entry's driver rule.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DriverRule {
    /// Windows: the allowed driver files, each with its own minimum.
    #[serde(default)]
    pub allowed: Vec<Allowed>,
    /// Drivers that fail, each with its evidence.
    #[serde(default)]
    pub bad: Vec<Bad>,
    /// `settled` or `open`.
    pub status: String,
    /// Why the rule holds.
    pub evidence: String,
    /// The measurement that decides an open rule.
    pub plan: Option<String>,
}

/// One allowed Windows driver file and its own lowest version (#465: the
/// RTL8156B entry allows `rtucx22x64.sys` from 11.19.602.2025 and
/// `rtu56cx22x64.sys` from 1156.22.113.2026).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Allowed {
    /// The driver file (`rt640x64.sys`), matched in any case.
    pub file: String,
    /// The lowest allowed version of this file, when the standard has one.
    pub min_version: Option<String>,
}

impl Allowed {
    /// `file min or newer`, or the file alone without a minimum.
    pub fn wanted(&self) -> String {
        match &self.min_version {
            Some(min) => format!("{} {min} or newer", self.file),
            None => self.file.clone(),
        }
    }
}

/// A driver the standard names bad: a file, a version, or both.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Bad {
    /// The driver file, when the bad driver is a file.
    pub file: Option<String>,
    /// The exact version, when the bad driver is a version.
    pub version: Option<String>,
    /// Why it fails.
    pub evidence: String,
}

impl Standard {
    /// The standard parsed from the file's text.
    pub fn parse(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    /// The Windows entry whose hardware id prefix starts `hardware_id`
    /// (any case), as the checkers find it.
    pub fn windows_entry(&self, hardware_id: &str) -> Option<&Entry> {
        let id = hardware_id.trim().to_ascii_uppercase();
        self.adapters.iter().find(|e| {
            e.os == "windows"
                && e.matches
                    .hardware_id
                    .iter()
                    .any(|p| id.starts_with(&p.to_ascii_uppercase()))
        })
    }
}

/// The embedded standard ([`FILE`]), parsed once; `None` only when the
/// file does not parse, which a test rules out.
pub fn embedded() -> Option<&'static Standard> {
    static PARSED: OnceLock<Option<Standard>> = OnceLock::new();
    PARSED.get_or_init(|| Standard::parse(FILE).ok()).as_ref()
}

/// Two dotted versions compared number by number, the shorter padded with
/// zeros (10.79.20 equals 10.79.20.0), as both checkers compare them.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let numbers = |s: &str| -> Vec<u64> {
        s.split(|c: char| !c.is_ascii_digit())
            .filter(|n| !n.is_empty())
            .map(|n| n.parse().unwrap_or(u64::MAX))
            .collect()
    };
    let (mut x, mut y) = (numbers(a), numbers(b));
    let width = x.len().max(y.len());
    x.resize(width, 0);
    y.resize(width, 0);
    x.cmp(&y)
}

/// The file name at the end of a driver's `ImagePath`
/// (`\SystemRoot\System32\drivers\rt640x64.sys` gives `rt640x64.sys`).
pub fn file_name(image_path: &str) -> &str {
    let path = image_path.trim().trim_matches('"');
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// The driver file of the service `service`: its `ImagePath`'s file, or
/// without one the file Windows loads then (`System32\drivers\<service>.sys`).
pub fn driver_file(service: &str, image_path: Option<&str>) -> String {
    match image_path.map(file_name).filter(|f| !f.is_empty()) {
        Some(file) => file.to_string(),
        None => format!("{}.sys", service.trim()),
    }
}

#[cfg(test)]
mod tests;
