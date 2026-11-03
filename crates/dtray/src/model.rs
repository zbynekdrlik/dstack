// SPDX-License-Identifier: MIT OR Apache-2.0

//! The readings as the tray shows them (pure, every platform).
//!
//! A device's status line ([`dstack::ctl::status`]) gives:
//! - its clock: `clock.ready`, `clock.source` (`dsync` or `system`),
//!   `clock.holdover`;
//! - its RX channels: `channels[].status`, ARC 0x3000's word (0 without a
//!   subscription), receiving when [`SubStatus::from_code`] says
//!   [`SubState::Connected`];
//! - its late counts, cumulative since the device started: the RX flows'
//!   `rx[].late` summed, and the engine's `engine.missed` and
//!   `engine.overruns` (only once an engine is attached).
//!
//! [`Tracker::update`] keeps each device's counts from the update before,
//! so the view shows how much they grew since (the first sight of a device
//! and a device that restarted show 0). The colour is the worst device's
//! ([`Colour`]).

use std::collections::BTreeMap;

use dstack::subscribe::{SubState, SubStatus};
use serde_json::Value;

use crate::discover::Reading;

/// The icon's colour, worst last: the view's colour is the largest of its
/// devices'.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Colour {
    /// No dstack device on this PC.
    Grey,
    /// Every device is fine.
    Green,
    /// A subscribed channel receives nothing, or a late count grew.
    Orange,
    /// A device's status cannot be read, or its clock is not ready.
    Red,
}

/// What the tray shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub colour: Colour,
    /// One short line (a tray tooltip holds at most [`TOOLTIP_MAX`]).
    pub tooltip: String,
    /// One line per device, in the readings' order.
    pub lines: Vec<String>,
}

/// The longest tooltip, in characters (Windows keeps 127 and a NUL).
pub const TOOLTIP_MAX: usize = 127;

/// The late counts of a device, cumulative.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub late: u64,
    pub missed: u64,
    pub overruns: u64,
}

impl Counts {
    /// How much each count grew since `before`; a count that went down
    /// (the device restarted) grew by 0.
    pub fn since(self, before: Counts) -> Counts {
        Counts {
            late: self.late.saturating_sub(before.late),
            missed: self.missed.saturating_sub(before.missed),
            overruns: self.overruns.saturating_sub(before.overruns),
        }
    }
}

/// A device's status line, the fields the tray shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// The clock is ready (a missing field: not ready).
    pub ready: bool,
    /// `dsync` or `system`; `None` when missing.
    pub source: Option<String>,
    pub holdover: bool,
    /// RX channels with a subscription.
    pub subscribed: usize,
    /// Of them, the ones receiving.
    pub connected: usize,
    pub counts: Counts,
    /// The status has an engine part.
    pub engine: bool,
}

/// A field as a number: a JSON number, or a bool as 0/1; `None` when
/// missing or neither.
fn num(v: Option<&Value>) -> Option<u64> {
    match v? {
        Value::Number(n) => n.as_u64(),
        Value::Bool(b) => Some(u64::from(*b)),
        _ => None,
    }
}

/// A flag field: a number other than 0, or `true`; missing is false.
fn flag(v: Option<&Value>) -> bool {
    num(v).is_some_and(|n| n != 0)
}

/// The status of `line`; `None` when it is no JSON object.
pub fn parse(line: &str) -> Option<Status> {
    let v: Value = serde_json::from_str(line).ok()?;
    let obj = v.as_object()?;
    let clock = obj.get("clock");
    let field = |part: Option<&Value>, key: &str| part.and_then(|p| p.get(key)).cloned();
    let statuses: Vec<u64> = obj
        .get("channels")
        .and_then(Value::as_array)
        .map(|chs| chs.iter().filter_map(|c| num(c.get("status"))).collect())
        .unwrap_or_default();
    let subscribed = statuses.iter().filter(|&&s| s != 0).count();
    let connected = statuses
        .iter()
        .filter_map(|&s| u32::try_from(s).ok())
        .filter_map(SubStatus::from_code)
        .filter(|s| s.state() == SubState::Connected)
        .count();
    let late = obj
        .get("rx")
        .and_then(Value::as_array)
        .map(|flows| flows.iter().filter_map(|f| num(f.get("late"))).sum())
        .unwrap_or(0);
    let engine = obj.get("engine");
    Some(Status {
        ready: flag(field(clock, "ready").as_ref()),
        source: field(clock, "source").and_then(|s| s.as_str().map(str::to_string)),
        holdover: flag(field(clock, "holdover").as_ref()),
        subscribed,
        connected,
        counts: Counts {
            late,
            missed: num(field(engine, "missed").as_ref()).unwrap_or(0),
            overruns: num(field(engine, "overruns").as_ref()).unwrap_or(0),
        },
        engine: engine.is_some_and(Value::is_object),
    })
}

/// A device's colour and, unless green, why (the first reason by weight).
pub fn judge(status: &Status, grew: Counts) -> (Colour, Option<String>) {
    if !status.ready {
        return (Colour::Red, Some("clock not ready".into()));
    }
    let silent = status.subscribed.saturating_sub(status.connected);
    if silent > 0 {
        let reason = format!(
            "{silent} of {} subscriptions not connected",
            status.subscribed
        );
        return (Colour::Orange, Some(reason));
    }
    let grown = [
        ("late", grew.late),
        ("missed", grew.missed),
        ("overruns", grew.overruns),
    ];
    if let Some((what, n)) = grown.iter().find(|(_, n)| *n > 0) {
        return (Colour::Orange, Some(format!("{what} +{n}")));
    }
    (Colour::Green, None)
}

/// A device's line.
pub fn line(name: &str, status: &Status, grew: Counts) -> String {
    let source = status.source.as_deref().unwrap_or("?");
    let ready = if status.ready { "ready" } else { "not ready" };
    let holdover = if status.holdover { ", holdover" } else { "" };
    let mut s = format!(
        "{name}: clock {source} {ready}{holdover}; RX {}/{} connected; late +{}",
        status.connected, status.subscribed, grew.late
    );
    if status.engine {
        s.push_str(&format!(
            "; missed +{}, overruns +{}",
            grew.missed, grew.overruns
        ));
    }
    s
}

/// `s` cut to at most `max` characters.
pub fn clip(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// The view's counts kept between updates (see the module).
#[derive(Debug, Default)]
pub struct Tracker {
    last: BTreeMap<String, Counts>,
}

impl Tracker {
    /// The view of `readings` (name, status line or why none), each
    /// device's counts kept for the next update. A device not in
    /// `readings` is forgotten.
    pub fn update(&mut self, readings: &[Reading]) -> View {
        let mut last = BTreeMap::new();
        let mut lines = Vec::with_capacity(readings.len());
        let mut worst: Option<(Colour, String)> = None;
        for (name, reading) in readings {
            let parsed = match reading {
                Ok(text) => parse(text).ok_or_else(|| "the status is not JSON".to_string()),
                Err(e) => Err(e.clone()),
            };
            let (colour, reason) = match parsed {
                Ok(status) => {
                    let grew = self
                        .last
                        .get(name)
                        .map_or(Counts::default(), |&before| status.counts.since(before));
                    last.insert(name.clone(), status.counts);
                    lines.push(line(name, &status, grew));
                    judge(&status, grew)
                }
                Err(e) => {
                    lines.push(format!("{name}: status unreadable ({e})"));
                    (Colour::Red, Some("status unreadable".to_string()))
                }
            };
            let text = reason.map_or_else(|| "ok".to_string(), |r| format!("{name}: {r}"));
            if worst.as_ref().is_none_or(|(c, _)| colour > *c) {
                worst = Some((colour, text));
            }
        }
        self.last = last;
        let n = readings.len();
        let (colour, tooltip) = match worst {
            None => (Colour::Grey, "dstack: no devices on this PC".to_string()),
            Some((c, why)) => {
                let devices = if n == 1 { "device" } else { "devices" };
                (c, format!("dstack: {n} {devices}, {why}"))
            }
        };
        View {
            colour,
            tooltip: clip(&tooltip, TOOLTIP_MAX),
            lines,
        }
    }
}

/// The view as `dtray --print` writes it: the tooltip, then each line.
pub fn print_text(view: &View) -> String {
    let mut s = view.tooltip.clone();
    s.push('\n');
    for l in &view.lines {
        s.push_str(l);
        s.push('\n');
    }
    s
}

/// Every reading as one JSON object (the menu's Copy status): each name to
/// its status object, or to `{"error": <why>}`.
pub fn copy_json(readings: &[Reading]) -> String {
    let mut all = serde_json::Map::new();
    for (name, reading) in readings {
        let v = match reading {
            Ok(text) => serde_json::from_str::<Value>(text)
                .unwrap_or_else(|_| serde_json::json!({ "error": "the status is not JSON" })),
            Err(e) => serde_json::json!({ "error": e }),
        };
        all.insert(name.clone(), v);
    }
    Value::Object(all).to_string()
}

#[cfg(test)]
mod tests;
