// SPDX-License-Identifier: MIT OR Apache-2.0

//! The tray menu's decisions (pure, every platform; the binary's `tray.rs`
//! only calls them):
//!
//! - [`Known`]: each device's ARC settings, read again every
//!   [`SETTINGS_EVERY`] ticks, at once for a new device. A failed read
//!   keeps the settings before it and waits for the next turn, so a device
//!   that does not answer costs one read per turn, not one per tick, and a
//!   lost reply does not swap its latency menu out.
//! - [`Shape`], [`shapes_of`]: each device's part of the menu as built.
//! - [`needs_rebuild`]: whether the menu must be built again, or only its
//!   texts set.
//! - [`write_needed`]: whether a latency choice changes anything;
//!   [`output_needed`] the same for an app's output ([`crate::output`]);
//!   [`write_failed`] the text of a write that failed.

use std::collections::BTreeMap;
use std::net::SocketAddrV4;

use dstack::arc::control::Settings;

use crate::discover::{Local, Reading};
use crate::latency::{self, Choice};
use crate::output::{self, OutputChoice};

/// A device's settings are read again every this many ticks.
pub const SETTINGS_EVERY: u32 = 10;

/// One device's settings and when they were last tried.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    /// The last settings read, if any read yet.
    settings: Option<Settings>,
    /// The tick of the last read, answered or not.
    tried: u32,
}

/// Each device's ARC settings (see the module).
#[derive(Debug, Default)]
pub struct Known {
    entries: BTreeMap<SocketAddrV4, Entry>,
}

impl Known {
    /// Forgets every device whose ARC is not in `arcs`.
    pub fn forget_gone(&mut self, arcs: &[SocketAddrV4]) {
        self.entries.retain(|arc, _| arcs.contains(arc));
    }

    /// Of `arcs`, the ones to read at `tick`: never tried, or last tried
    /// [`SETTINGS_EVERY`] ticks ago or more (ticks wrap).
    pub fn due(&self, arcs: &[SocketAddrV4], tick: u32) -> Vec<SocketAddrV4> {
        arcs.iter()
            .filter(|arc| {
                self.entries
                    .get(arc)
                    .is_none_or(|e| tick.wrapping_sub(e.tried) >= SETTINGS_EVERY)
            })
            .copied()
            .collect()
    }

    /// A read of `arc` at `tick`: its settings, or `None` when it failed
    /// (the settings before it stay).
    pub fn record(&mut self, arc: SocketAddrV4, read: Option<Settings>, tick: u32) {
        let e = self.entries.entry(arc).or_insert(Entry {
            settings: None,
            tried: tick,
        });
        e.tried = tick;
        if read.is_some() {
            e.settings = read;
        }
    }

    /// `arc`'s last settings read.
    pub fn get(&self, arc: &SocketAddrV4) -> Option<&Settings> {
        self.entries.get(arc).and_then(|e| e.settings.as_ref())
    }
}

/// One device's part of the menu as built: its ARC, latency values and
/// the app's outputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shape {
    pub arc: SocketAddrV4,
    pub choices: Vec<Choice>,
    pub outputs: Vec<OutputChoice>,
}

/// Each device's shape: its latency menu from `known`'s settings, none
/// without them; the app's outputs from its reading in `readings` (the
/// reading of the same name at the same place; none without it).
pub fn shapes_of(devices: &[Local], known: &Known, readings: &[Reading]) -> Vec<Shape> {
    devices
        .iter()
        .enumerate()
        .map(|(i, d)| Shape {
            arc: d.arc,
            choices: known.get(&d.arc).map(latency::choices).unwrap_or_default(),
            outputs: match readings.get(i) {
                Some((name, Ok(line))) if *name == d.name => output::choices(line),
                _ => Vec::new(),
            },
        })
        .collect()
}

/// The menu as built, the parts [`needs_rebuild`] compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    /// Device submenus.
    pub devices: usize,
    /// Lines of expected devices not running.
    pub missing: usize,
}

/// Whether the menu must be built again: `forced` (a choice was clicked:
/// its item checked itself), a device's shape changed, or the number of
/// device or missing lines did. Otherwise only the texts are set. `lines`
/// is the view's line count, `devices` how many of them are devices'.
pub fn needs_rebuild(
    forced: bool,
    built: Counts,
    old: &[Shape],
    new: &[Shape],
    devices: usize,
    lines: usize,
) -> bool {
    forced
        || old != new
        || built
            != Counts {
                devices,
                missing: lines.saturating_sub(devices),
            }
}

/// Whether choosing `ns` for the device at `arc` writes anything: not when
/// it is the value already checked.
pub fn write_needed(shapes: &[Shape], arc: SocketAddrV4, ns: u32) -> bool {
    !shapes
        .iter()
        .filter(|s| s.arc == arc)
        .flat_map(|s| &s.choices)
        .any(|c| c.ns == ns && c.checked)
}

/// The text of a latency write of `ns` that failed with `err` (the tray's
/// error box, `dtray --set-latency`'s error line).
pub fn write_failed(ns: u32, err: &str) -> String {
    format!("the latency {} was not set: {err}", latency::label(ns))
}

/// Whether choosing output `index` of the device at `arc` sends
/// anything: not when it is the one already checked.
pub fn output_needed(shapes: &[Shape], arc: SocketAddrV4, index: usize) -> bool {
    !shapes
        .iter()
        .filter(|s| s.arc == arc)
        .flat_map(|s| &s.outputs)
        .any(|o| o.index == index && o.checked)
}

#[cfg(test)]
mod tests;
