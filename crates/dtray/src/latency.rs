// SPDX-License-Identifier: MIT OR Apache-2.0

//! The latency menu (pure, every platform): the values a device offers,
//! as Dante Controller lists them, within the range its ARC settings give
//! ([`dstack::arc::control::Settings`]: `latency_min_ns`,
//! `latency_max_ns`), the configured one checked. The tray menu shows
//! them and writes a choice with [`dstack::arc::control::set_latency`];
//! `dtray --set-latency <device> <ms>` takes the same choice from the
//! command line ([`ns_of_ms`], [`pick`], [`read_back`]; [`crate::set`]).

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

/// The value `ms` names, as the menu's labels write it without " ms"
/// (`0.25`, `0.5`, `1`, `2`, `5`, `10`): one of [`DANTE_NS`], or `None`.
pub fn ns_of_ms(ms: &str) -> Option<u32> {
    DANTE_NS
        .iter()
        .copied()
        .find(|&ns| label(ns) == format!("{ms} ms"))
}

/// The values of `choices` as the command line takes them: `0.5, 1, 2 ms`.
pub fn offered(choices: &[Choice]) -> String {
    let values: Vec<String> = choices
        .iter()
        .map(|c| c.label.trim_end_matches(" ms").to_string())
        .collect();
    format!("{} ms", values.join(", "))
}

/// The value of `choices` (a device's menu, [`choices`]) that `ms` names;
/// the reason otherwise: the device names no range, or `ms` is not one of
/// its values (the error names them).
pub fn pick(choices: &[Choice], ms: &str) -> Result<u32, String> {
    if choices.is_empty() {
        return Err("the device names no latency range; nothing written".to_string());
    }
    ns_of_ms(ms)
        .filter(|&ns| choices.iter().any(|c| c.ns == ns))
        .ok_or_else(|| {
            format!(
                "{ms} ms is not a latency the device offers ({}); nothing written",
                offered(choices)
            )
        })
}

/// The verdict on a write of `asked` from the configured latency the
/// device reads back after it: the line when they are equal, the reason
/// otherwise (a value read back as the menu shows it, or none).
pub fn read_back(asked: u32, read: Option<u32>) -> Result<String, String> {
    match read {
        Some(ns) if ns == asked => Ok(format!(
            "the latency is set to {}; it reads back {}",
            label(asked),
            label(ns)
        )),
        Some(ns) => Err(format!(
            "the latency {} was written, but it reads back {}",
            label(asked),
            label(ns)
        )),
        None => Err(format!(
            "the latency {} was written, but the device reads back no configured latency",
            label(asked)
        )),
    }
}

#[cfg(test)]
mod tests;
