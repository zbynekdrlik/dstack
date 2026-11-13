// SPDX-License-Identifier: MIT OR Apache-2.0

//! What each list of the settings window offers (pure, every platform):
//! the window fills its combo boxes from these and maps a selection back
//! to a value through the same list.

use std::collections::BTreeSet;
use std::net::Ipv4Addr;

use dstack::stream::BLOCKS;

use super::{MAX_CHANNELS, MIN_CHANNELS, cpu_label};
use crate::latency::{DANTE_NS, label_wide};

/// One entry of a list: the value it stands for and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice<T> {
    pub value: T,
    pub text: String,
    /// Shown grey (another instance's CPU).
    pub greyed: bool,
}

fn choice<T>(value: T, text: String) -> Choice<T> {
    Choice {
        value,
        text,
        greyed: false,
    }
}

/// The index of `value` in `list`; the first entry when it is not there.
pub fn index_of<T: PartialEq>(list: &[Choice<T>], value: &T) -> usize {
    list.iter().position(|c| c.value == *value).unwrap_or(0)
}

/// `frames` at `rate` Hz in ms, two decimals.
fn ms(frames: u64, rate: u32) -> String {
    format!("{:.2}", frames as f64 * 1000.0 / f64::from(rate.max(1)))
}

/// The buffers: each of dstack's blocks, with its time at `rate`
/// (`256 samples (2.67 ms @ 96 kHz)`), and `current` when it is none of
/// them.
pub fn blocks(current: u64, rate: u32) -> Vec<Choice<u64>> {
    let mut values: Vec<u64> = BLOCKS.to_vec();
    if !values.contains(&current) {
        values.push(current);
        values.sort_unstable();
    }
    values
        .into_iter()
        .map(|b| {
            choice(
                b,
                format!("{b} samples ({} ms @ {} kHz)", ms(b, rate), khz(rate)),
            )
        })
        .collect()
}

/// `rate` in kHz as people write it (96, 44.1).
pub fn khz(rate: u32) -> String {
    if rate.is_multiple_of(1000) {
        (rate / 1000).to_string()
    } else {
        format!("{}", f64::from(rate) / 1000.0)
    }
}

/// The RX latencies: Dante's values inside the device's `range` (its ARC
/// settings' min and max, ns; every value when it is not known), and
/// `current` when it is none of them.
pub fn rx_latencies(current: u64, range: Option<(u64, u64)>) -> Vec<Choice<u64>> {
    let mut values: Vec<u64> = DANTE_NS
        .iter()
        .map(|&ns| u64::from(ns))
        .filter(|ns| range.is_none_or(|(min, max)| (min..=max).contains(ns)))
        .collect();
    if !values.contains(&current) {
        values.push(current);
        values.sort_unstable();
    }
    values
        .into_iter()
        .map(|ns| choice(ns, label_wide(ns)))
        .collect()
}

/// The TX latencies: the RX latency's (`None`, the config leaves it out),
/// then Dante's values, and `current` when it is none of them.
pub fn tx_latencies(current: Option<u64>) -> Vec<Choice<Option<u64>>> {
    let mut values: Vec<u64> = DANTE_NS.iter().map(|&ns| u64::from(ns)).collect();
    if let Some(c) = current.filter(|c| !values.contains(c)) {
        values.push(c);
        values.sort_unstable();
    }
    std::iter::once(choice(None, "Same as the RX latency".to_string()))
        .chain(
            values
                .into_iter()
                .map(|ns| choice(Some(ns), label_wide(ns))),
        )
        .collect()
}

/// The channel counts: [`MIN_CHANNELS`] to [`MAX_CHANNELS`], and
/// `current` when it is outside them.
pub fn channels(current: usize) -> Vec<Choice<usize>> {
    let mut values: Vec<usize> = (MIN_CHANNELS..=MAX_CHANNELS).collect();
    if !values.contains(&current) {
        values.push(current);
        values.sort_unstable();
    }
    values
        .into_iter()
        .map(|n| choice(n, n.to_string()))
        .collect()
}

/// A CPU list: `Any`, then CPU 0 to `count - 1`, those in `busy` (another
/// instance's) greyed, and `current` when the PC has no such CPU.
pub fn cpus(
    count: usize,
    busy: &BTreeSet<usize>,
    current: Option<usize>,
) -> Vec<Choice<Option<usize>>> {
    let mut values: Vec<usize> = (0..count).collect();
    if let Some(c) = current.filter(|&c| c >= count) {
        values.push(c);
    }
    std::iter::once(choice(None, cpu_label(None)))
        .chain(values.into_iter().map(|c| Choice {
            value: Some(c),
            text: cpu_label(Some(c)),
            greyed: busy.contains(&c),
        }))
        .collect()
}

/// A network adapter of the PC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adapter {
    /// Windows' interface alias (Get-NetAdapter Name), what `iface` names.
    pub alias: String,
    pub description: String,
    /// Its IPv4 addresses, the first first.
    pub addresses: Vec<Ipv4Addr>,
    /// The receive link speed, Mbit/s.
    pub mbit: Option<u64>,
}

/// An adapter as the list shows it: the alias, then its description, its
/// IPv4 address and its speed where known.
pub fn adapter_text(a: &Adapter) -> String {
    let mut parts = vec![a.alias.clone()];
    if !a.description.is_empty() {
        parts.push(a.description.clone());
    }
    if let Some(ip) = a.addresses.first() {
        parts.push(ip.to_string());
    }
    match a.mbit {
        Some(m) if m >= 1000 && m.is_multiple_of(1000) => parts.push(format!("{} Gbps", m / 1000)),
        Some(m) if m >= 1000 => parts.push(format!("{} Gbps", m as f64 / 1000.0)),
        Some(m) => parts.push(format!("{m} Mbps")),
        None => {}
    }
    parts.join(" · ")
}

/// The alias of the adapter of `adapters` holding `ip` (any of its
/// addresses).
pub fn alias_of(adapters: &[Adapter], ip: Ipv4Addr) -> Option<String> {
    adapters
        .iter()
        .find(|a| a.addresses.contains(&ip))
        .map(|a| a.alias.clone())
}

/// The interface list: first the config's own choice when it names none
/// (`None`: the service's install flag; with the alias of the adapter
/// the device runs on, `running`, when known), then every adapter with an
/// IPv4 address, and the configured alias when no adapter has it (shown
/// as not found: dasio logs "no such interface").
pub fn interfaces(
    adapters: &[Adapter],
    configured: Option<&str>,
    running: Option<&str>,
) -> Vec<Choice<Option<String>>> {
    let mut out = Vec::new();
    if configured.is_none() {
        let text = match running {
            Some(alias) => format!("As installed (now {alias})"),
            None => "As installed".to_string(),
        };
        out.push(choice(None, text));
    }
    for a in adapters.iter().filter(|a| !a.addresses.is_empty()) {
        out.push(choice(Some(a.alias.clone()), adapter_text(a)));
    }
    if let Some(alias) = configured
        && !out.iter().any(|c| c.value.as_deref() == Some(alias))
    {
        out.push(choice(
            Some(alias.to_string()),
            format!("{alias} (not found)"),
        ));
    }
    out
}

#[cfg(test)]
mod tests;
