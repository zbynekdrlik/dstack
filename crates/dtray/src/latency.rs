// SPDX-License-Identifier: MIT OR Apache-2.0

//! The latency menu (pure, every platform): the values a device offers,
//! as Dante Controller lists them, within the range its ARC settings give
//! ([`dstack::arc::control::Settings`]: `latency_min_ns`,
//! `latency_max_ns`), the configured one checked. The Windows menu shows
//! them and writes a choice with [`dstack::arc::control::set_latency`].

use dstack::arc::control::Settings;

/// Dante's latency values, ns: 0.25, 0.5, 1, 2, 5 and 10 ms.
pub const DANTE_NS: [u32; 6] = [
    250_000, 500_000, 1_000_000, 2_000_000, 5_000_000, 10_000_000,
];

/// One value of the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub ns: u32,
    /// As the menu shows it (`0.25 ms`).
    pub label: String,
    /// The device's configured latency.
    pub checked: bool,
}

/// `ns` as the menu shows it: milliseconds, no trailing zeros
/// (`0.25 ms`, `1 ms`).
pub fn label(ns: u32) -> String {
    let whole = ns / 1_000_000;
    let frac = ns % 1_000_000;
    if frac == 0 {
        return format!("{whole} ms");
    }
    let digits = format!("{frac:06}");
    format!("{whole}.{} ms", digits.trim_end_matches('0'))
}

/// The menu of a device with `settings`: each of [`DANTE_NS`] from its
/// minimum to its maximum, both included, the configured one checked.
/// None when the device names no range (no value is guessed).
pub fn choices(settings: &Settings) -> Vec<Choice> {
    let (Some(min), Some(max)) = (settings.latency_min_ns, settings.latency_max_ns) else {
        return Vec::new();
    };
    DANTE_NS
        .iter()
        .filter(|&&ns| min <= ns && ns <= max)
        .map(|&ns| Choice {
            ns,
            label: label(ns),
            checked: settings.latency_configured_ns == Some(ns),
        })
        .collect()
}

#[cfg(test)]
mod tests;
