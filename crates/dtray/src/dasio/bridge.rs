// SPDX-License-Identifier: MIT OR Apache-2.0

//! A dasio bridge instance's `[bridge]` table (dasio#106, dasio 0.1.83;
//! dstack #292): dasio's own key, like `iface`, which makes the instance a
//! bridge from Dante to a local ASIO driver. dasio's schema
//! (`crates/dasio/src/kind.rs`):
//!
//! ```toml
//! [bridge]
//! driver = "<the registered ASIO driver's name>"
//! to_asio = [1, 2]        # the driver output of each RX channel, 1-based
//! from_asio = [1, 2]      # the driver input of each TX channel, 1-based
//! app_buffer_ns = 5000000 # optional
//! ```
//!
//! [`parse`] takes the table's shape (the keys and their types: a typo is
//! refused, as dstack's keys are by `DeviceConfig`); [`Bridge::problems`]
//! is what dasio refuses at its start (no driver, a channel 0 or twice, a
//! map's length against the channel counts, the app buffer's range). The
//! window opens a file with such problems, so it can be fixed there, and
//! refuses to apply one ([`super::plan`]). [`write`](fn@write) puts the table back
//! into the file's text, the comments and other keys kept.

use std::collections::BTreeSet;

use serde::Deserialize;
use toml_edit::{Array, DocumentMut, Item, Table, TableLike, Value as TomlValue};

use dlink::bridge::AppBuffer;

/// The device config's table that makes an instance a bridge.
pub const KEY: &str = "bridge";

/// The `[bridge]` table (channels 1-based, as written).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bridge {
    /// The registered ASIO driver's name.
    pub driver: String,
    /// The driver output of each RX channel, in RX order.
    #[serde(default)]
    pub to_asio: Vec<u32>,
    /// The driver input of each TX channel, in TX order.
    #[serde(default)]
    pub from_asio: Vec<u32>,
    /// The bridge FIFO's target, ns (`None`: dstack's default).
    #[serde(default)]
    pub app_buffer_ns: Option<u64>,
}

/// The table as the file holds it.
#[derive(Deserialize)]
struct Whole {
    bridge: Bridge,
}

/// The [`Bridge`] of the `[bridge]` item taken out of a config; the
/// reason otherwise.
pub fn parse(item: Item) -> Result<Bridge, String> {
    if !item.is_table_like() {
        return Err(format!("{KEY}: not a table"));
    }
    let mut doc = DocumentMut::new();
    doc.insert(KEY, item);
    toml::from_str::<Whole>(&doc.to_string())
        .map(|w| w.bridge)
        .map_err(|e| format!("[{KEY}]: {}", e.message()))
}

/// The labels of the window's bridge fields.
pub const DRIVER: &str = "ASIO driver";
pub const TO_ASIO: &str = "Dante RX to driver outputs";
pub const FROM_ASIO: &str = "Driver inputs to Dante TX";
pub const APP_BUFFER: &str = "App buffer";

impl Bridge {
    /// Why dasio would refuse this bridge on a device with `rx` RX and `tx`
    /// TX channels (its `kind::bridge` and `BridgeConfig::spec`), in the
    /// window's words; empty when it would not.
    pub fn problems(&self, rx: usize, tx: usize) -> Vec<String> {
        let mut out = Vec::new();
        if self.driver.trim().is_empty() {
            out.push(format!("{DRIVER}: none chosen"));
        }
        for (label, list, channels, side) in [
            (TO_ASIO, &self.to_asio, rx, "RX"),
            (FROM_ASIO, &self.from_asio, tx, "TX"),
        ] {
            out.extend(list_problems(label, list));
            if list.len() != channels {
                out.push(format!(
                    "{label}: {} given for {channels} {side} channels (one for each)",
                    list.len()
                ));
            }
        }
        if self.to_asio.is_empty() && self.from_asio.is_empty() {
            out.push("the bridge carries no channel".to_string());
        }
        // dlink's own check and text (the range the bridge's FIFO takes);
        // unset is dstack's default, always in range.
        if let Some(ns) = self.app_buffer_ns
            && let Some(problem) = AppBuffer::new(0, 0).set(Some(ns)).problem()
        {
            out.push(format!("{APP_BUFFER}: {problem}"));
        }
        out
    }
}

/// A map's own problems: a channel 0 (they are 1-based) or one given twice.
fn list_problems(label: &str, list: &[u32]) -> Vec<String> {
    let mut out = Vec::new();
    if list.contains(&0) {
        out.push(format!("{label}: channels count from 1"));
    }
    let mut seen = BTreeSet::new();
    let twice: BTreeSet<u32> = list.iter().filter(|&&c| !seen.insert(c)).copied().collect();
    out.extend(twice.iter().map(|c| format!("{label}: channel {c} twice")));
    out
}

/// A map as the window shows it: `1, 2, 3`.
pub fn map_text(list: &[u32]) -> String {
    list.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A map typed in the window (`1, 2, 3`; spaces aside, empty for none);
/// the reason when an entry is no channel number.
pub fn parse_map(label: &str, text: &str) -> Result<Vec<u32>, String> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    text.split(',')
        .map(|s| {
            let s = s.trim();
            if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
                return Err(format!(
                    "{label}: \"{s}\" is not a channel number (write them as 1, 2, 3)"
                ));
            }
            s.parse::<u32>()
                .map_err(|_| format!("{label}: {s} is too large"))
        })
        .collect()
}

/// The app buffer as the window shows it, in ms (`5`, `1.333333`); empty
/// for the default.
pub fn buffer_text(ns: Option<u64>) -> String {
    let Some(ns) = ns else {
        return String::new();
    };
    let (whole, frac) = (ns / 1_000_000, ns % 1_000_000);
    if frac == 0 {
        return whole.to_string();
    }
    let frac = format!("{frac:06}");
    format!("{whole}.{}", frac.trim_end_matches('0'))
}

/// The app buffer typed in ms (up to 6 decimals, a dot); `None` when
/// empty (the default); the reason otherwise.
pub fn parse_buffer(text: &str) -> Result<Option<u64>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    let bad = || format!("{APP_BUFFER}: \"{t}\" is not a time in ms (like 5 or 2.5)");
    let (whole, frac) = t.split_once('.').unwrap_or((t, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() || !digits(whole) || !digits(frac) || frac.len() > 6 {
        return Err(bad());
    }
    let whole: u64 = whole.parse().map_err(|_| bad())?;
    let frac: u64 = format!("{frac:0<6}").parse().map_err(|_| bad())?;
    whole
        .checked_mul(1_000_000)
        .and_then(|ns| ns.checked_add(frac))
        .map(Some)
        .ok_or_else(bad)
}

/// The bridge the window's fields give: the chosen `driver`, the two maps
/// and the app buffer as typed; every entry that is not a number, joined
/// with "; ", otherwise.
pub fn from_texts(
    driver: &str,
    to_asio: &str,
    from_asio: &str,
    app_buffer: &str,
) -> Result<Bridge, String> {
    let to = parse_map(TO_ASIO, to_asio);
    let from = parse_map(FROM_ASIO, from_asio);
    let app = parse_buffer(app_buffer);
    match (to, from, app) {
        (Ok(to_asio), Ok(from_asio), Ok(app_buffer_ns)) => Ok(Bridge {
            driver: driver.to_string(),
            to_asio,
            from_asio,
            app_buffer_ns,
        }),
        (to, from, app) => Err([to.err(), from.err(), app.err()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("; ")),
    }
}

/// `key` set to `v` in `t`: replaced in place when the key was there, so
/// the key's own comment lines and the value's (a comment after it on its
/// line) stay.
fn set(t: &mut dyn TableLike, key: &str, v: TomlValue) {
    let mut v = v;
    match t.get_mut(key) {
        Some(item) => {
            if let Some(old) = item.as_value() {
                *v.decor_mut() = old.decor().clone();
            }
            *item = Item::Value(v);
        }
        None => {
            t.insert(key, Item::Value(v));
        }
    }
}

/// A map as an inline TOML array (`[1, 2]`, as dasio's configs write it).
fn array(list: &[u32]) -> Array {
    list.iter().map(|&c| i64::from(c)).collect()
}

/// `doc` (a config's whole text) with the `[bridge]` table at `new`: only
/// the keys that differ from `old` written (the others, their comments and
/// order kept), the table added when the file had none and taken out for
/// `None`.
pub fn write(
    doc: &mut DocumentMut,
    old: Option<&Bridge>,
    new: Option<&Bridge>,
) -> Result<(), String> {
    let Some(new) = new else {
        doc.remove(KEY);
        return Ok(());
    };
    if doc.get(KEY).is_none() {
        doc.insert(KEY, Item::Table(Table::new()));
    }
    let t = doc
        .get_mut(KEY)
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| format!("{KEY}: not a table"))?;
    if old.map(|o| &o.driver) != Some(&new.driver) {
        set(t, "driver", TomlValue::from(new.driver.as_str()));
    }
    if old.map(|o| &o.to_asio) != Some(&new.to_asio) {
        set(t, "to_asio", TomlValue::Array(array(&new.to_asio)));
    }
    if old.map(|o| &o.from_asio) != Some(&new.from_asio) {
        set(t, "from_asio", TomlValue::Array(array(&new.from_asio)));
    }
    if old.map(|o| o.app_buffer_ns) != Some(new.app_buffer_ns) {
        match new.app_buffer_ns {
            Some(ns) => {
                let ns =
                    i64::try_from(ns).map_err(|_| format!("app_buffer_ns: {ns} is too large"))?;
                set(t, "app_buffer_ns", TomlValue::from(ns));
            }
            None => {
                t.remove("app_buffer_ns");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
