// SPDX-License-Identifier: MIT OR Apache-2.0

//! Source switches (dstack #210): a menu item that moves a receiver's RX
//! channels between sources, one source at a time, for a receiver that is
//! not ours (another device on the network). The PC's switch file names
//! the receiver, its RX channels and the sources; nothing else is ever
//! written: the file is the owner's approval's scope, and it lives on the
//! PC, never in the code (no site data in the repo).
//!
//! The file is TOML, beside the expected devices' folder
//! ([`path_for`]: `/etc/dstack/switches.toml` on Linux,
//! `%ProgramData%\dstack\switches.toml` on Windows; [`PATH_VAR`] names
//! another):
//!
//! ```toml
//! [[switch]]
//! label = "Solo"                 # the menu item's name
//! device = "Monitor"             # the receiver
//! rx = [1, 2]                    # its RX channels, 1 up
//!
//! [[switch.sources]]
//! label = "band"
//! device = "Band PC"             # the transmitter
//! channels = ["Mix L", "Mix R"]  # its TX channels, one per RX channel
//!
//! [[switch.sources]]
//! label = "master"
//! device = "Master PC"
//! channels = ["Mix L", "Mix R"]
//! ```
//!
//! - [`parse`], [`load`]: the file, checked ([`parse`]'s list of what a
//!   switch must have); no file is no switch.
//! - [`current`]: which source the receiver's RX channels are on, from
//!   its 0x3000 read ([`dstack::arc::control::rx_channels`]): a source
//!   matches when every listed RX channel is subscribed to that source's
//!   device and channel (names case aside, as Dante compares them); else
//!   [`Current::Other`], or [`Current::Unread`] without a read.
//! - [`plan`]: the subscriptions a choice sends, one per listed RX
//!   channel and only those.
//! - [`set`]: a choice: a read, the subscriptions
//!   ([`dstack::arc::control::subscribe`]) unless the source is already
//!   on, and the read back that is the verdict ([`verdict`]): a device
//!   answers OK also when it changed nothing.
//! - [`items`]: the tray menu's part, a submenu per switch.
//! - [`read_live`], [`set_live`]: the receiver found by dtray's discovery
//!   ([`crate::discover::Scanner::remote`]) and reached through its ARC
//!   ([`crate::arc::call_from`]); glue over the above, run by hand.
//!
//! The tray reads each switch's receiver every settings round
//! ([`due`], [`crate::menu::SETTINGS_EVERY`]); `dtray --switch <label>
//! <source>` runs [`set`] from the command line, and `dtray --print`
//! prints each switch's [`line()`].

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use dstack::arc::control::{self, RxChannel};
use serde::Deserialize;

use crate::arc;
use crate::discover::Scanner;
use crate::menu::SETTINGS_EVERY;

/// The switch file's name, beside the expected devices' folder.
pub const FILE_NAME: &str = "switches.toml";
/// The environment variable that names another switch file (tests, a
/// check by hand).
pub const PATH_VAR: &str = "DSTACK_SWITCHES";

/// One switch: a receiver's RX channels and the sources they move between.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Switch {
    /// The menu item's name.
    pub label: String,
    /// The receiver's Dante name.
    pub device: String,
    /// Its RX channels (1 up), the only ones written.
    pub rx: Vec<u16>,
    pub sources: Vec<Source>,
}

/// One source: a transmitter and its TX channels, one per RX channel of
/// the switch, in the same order.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub label: String,
    pub device: String,
    pub channels: Vec<String>,
}

/// The file: its switches.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    switch: Vec<Switch>,
}

/// The switch file: `over` when set and not empty, else [`FILE_NAME`] in
/// the folder that holds `devices_dir` (the expected devices' folder,
/// [`crate::expected::dir_for`]).
pub fn path_for(devices_dir: &Path, over: Option<OsString>) -> PathBuf {
    if let Some(path) = over.filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    devices_dir
        .parent()
        .unwrap_or(Path::new(""))
        .join(FILE_NAME)
}

/// This PC's switch file ([`path_for`] with [`PATH_VAR`] and
/// [`crate::expected::devices_dir`]).
pub fn path() -> PathBuf {
    path_for(&crate::expected::devices_dir(), std::env::var_os(PATH_VAR))
}

/// The first of `items` seen before (case aside), if any.
fn twice<'a>(items: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let mut seen = BTreeSet::new();
    items.into_iter().find(|i| !seen.insert(i.to_lowercase()))
}

/// The longest Dante name in bytes (a device's or a channel's): the
/// subscription request carries the names as they are.
pub const MAX_NAME: usize = 31;

/// What is wrong with the Dante name `n` (a device's or a channel's), if
/// anything: empty, longer than [`MAX_NAME`], or not printable ASCII.
fn name_problem(n: &str) -> Option<String> {
    if n.trim().is_empty() {
        Some("empty".to_string())
    } else if n.len() > MAX_NAME {
        Some(format!("longer than {MAX_NAME} bytes"))
    } else if !n.bytes().all(|b| (b' '..=b'~').contains(&b)) {
        Some("not printable ASCII".to_string())
    } else {
        None
    }
}

/// What is wrong with one source of switch `sw`, if anything.
fn check_source(sw: &Switch, s: &Source) -> Result<(), String> {
    let name = &sw.label;
    if s.label.trim().is_empty() {
        return Err(format!("switch {name}: a source has no label"));
    }
    let at = format!("switch {name}, source {}", s.label);
    if let Some(why) = name_problem(&s.device) {
        return Err(format!("{at}: the device name is {why}"));
    }
    if s.channels.len() != sw.rx.len() {
        return Err(format!(
            "{at}: {} channels for {} RX channels",
            s.channels.len(),
            sw.rx.len()
        ));
    }
    for (i, c) in s.channels.iter().enumerate() {
        if let Some(why) = name_problem(c) {
            return Err(format!("{at}: channel {} name is {why}", i + 1));
        }
    }
    Ok(())
}

/// What is wrong with switch `sw`, if anything.
fn check(sw: &Switch) -> Result<(), String> {
    let name = &sw.label;
    if name.trim().is_empty() {
        return Err("a switch has no label".to_string());
    }
    if let Some(why) = name_problem(&sw.device) {
        return Err(format!("switch {name}: the device name is {why}"));
    }
    if sw.rx.is_empty() {
        return Err(format!("switch {name}: no RX channel"));
    }
    if sw.rx.contains(&0) {
        return Err(format!("switch {name}: RX channels count from 1"));
    }
    let rx: Vec<String> = sw.rx.iter().map(u16::to_string).collect();
    if let Some(n) = twice(rx.iter().map(String::as_str)) {
        return Err(format!("switch {name}: RX {n} is listed twice"));
    }
    if sw.sources.is_empty() {
        return Err(format!("switch {name}: no source"));
    }
    for s in &sw.sources {
        check_source(sw, s)?;
    }
    if let Some(l) = twice(sw.sources.iter().map(|s| s.label.as_str())) {
        return Err(format!("switch {name}: source {l} is listed twice"));
    }
    Ok(())
}

/// The switches of a file's `text`, checked: each has a label (one per
/// file, case aside), a receiver, its RX channels (1 up, each once) and a
/// source or more; each source a label (one per switch), a device and a
/// channel name per RX channel. Device and channel names are 1 to
/// [`MAX_NAME`] bytes of printable ASCII. Unknown keys are refused. The
/// reason otherwise.
pub fn parse(text: &str) -> Result<Vec<Switch>, String> {
    let file: File = toml::from_str(text).map_err(|e| e.message().to_string())?;
    for sw in &file.switch {
        check(sw)?;
    }
    if let Some(l) = twice(file.switch.iter().map(|s| s.label.as_str())) {
        return Err(format!("switch {l} is listed twice"));
    }
    Ok(file.switch)
}

/// The switches of the file at `path` ([`parse`]); no file is no switch.
/// The reason, with the path, when it cannot be read or is refused.
pub fn load(path: &Path) -> Result<Vec<Switch>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// The switch of `switches` labelled `label` (case aside); the reason
/// otherwise, with the labels there are.
pub fn find<'a>(switches: &'a [Switch], label: &str) -> Result<&'a Switch, String> {
    switches
        .iter()
        .find(|s| s.label.eq_ignore_ascii_case(label))
        .ok_or_else(|| {
            let all: Vec<&str> = switches.iter().map(|s| s.label.as_str()).collect();
            let all = if all.is_empty() {
                "none".to_string()
            } else {
                all.join(", ")
            };
            format!("no switch {label} (switches: {all})")
        })
}

/// The source of `sw` labelled `label` (case aside); the reason otherwise,
/// with its sources.
pub fn find_source<'a>(sw: &'a Switch, label: &str) -> Result<&'a Source, String> {
    sw.sources
        .iter()
        .find(|s| s.label.eq_ignore_ascii_case(label))
        .ok_or_else(|| {
            let all: Vec<&str> = sw.sources.iter().map(|s| s.label.as_str()).collect();
            format!(
                "switch {} has no source {label} (sources: {})",
                sw.label,
                all.join(", ")
            )
        })
}

/// Which source a switch's RX channels are on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Current {
    /// The source at this index of the switch's.
    Source(usize),
    /// Read, but on no source of the switch (some or all elsewhere, or
    /// not subscribed).
    Other,
    /// Not read: the reason.
    Unread(String),
}

/// Whether every RX channel of `sw` is subscribed, in `channels` (the
/// receiver's 0x3000 read), to `source`'s device and its channel for it;
/// never for a source without one channel per RX channel ([`parse`]
/// refuses it, a switch built in code may not).
pub fn matches(sw: &Switch, source: &Source, channels: &[RxChannel]) -> bool {
    source.channels.len() == sw.rx.len()
        && sw.rx.iter().zip(&source.channels).all(|(rx, tx)| {
            channels.iter().find(|c| c.channel == *rx).is_some_and(|c| {
                c.tx_device
                    .as_deref()
                    .is_some_and(|d| d.eq_ignore_ascii_case(&source.device))
                    && c.tx_channel
                        .as_deref()
                        .is_some_and(|t| t.eq_ignore_ascii_case(tx))
            })
        })
}

/// The source `sw`'s RX channels are on, from the receiver's read: the
/// first that [`matches()`], [`Current::Other`] when none does,
/// [`Current::Unread`] without a read.
pub fn current(sw: &Switch, read: &Result<Vec<RxChannel>, String>) -> Current {
    match read {
        Err(e) => Current::Unread(e.clone()),
        Ok(channels) => sw
            .sources
            .iter()
            .position(|s| matches(sw, s, channels))
            .map_or(Current::Other, Current::Source),
    }
}

/// `current` as the menu names it: the source's label, `other` or
/// `unread`.
pub fn state(sw: &Switch, current: &Current) -> String {
    match current {
        Current::Source(i) => sw
            .sources
            .get(*i)
            .map_or_else(|| "other".to_string(), |s| s.label.clone()),
        Current::Other => "other".to_string(),
        Current::Unread(_) => "unread".to_string(),
    }
}

/// The switch's line: `<label>: <state>` ([`state`]), the reason after
/// an unread one (`dtray --print`; the tray shows it inside the item).
pub fn line(sw: &Switch, current: &Current) -> String {
    let base = format!("{}: {}", sw.label, state(sw, current));
    match current {
        Current::Unread(why) => format!("{base} ({why})"),
        _ => base,
    }
}

/// One subscription a choice sends: the receiver's RX channel and the TX
/// channel and device it asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step<'a> {
    pub rx: u16,
    pub tx_channel: &'a str,
    pub tx_device: &'a str,
}

/// The subscriptions choosing `source` sends: each RX channel of `sw`, in
/// order, to `source`'s device and its channel for it; no other channel.
pub fn plan<'a>(sw: &'a Switch, source: &'a Source) -> Vec<Step<'a>> {
    sw.rx
        .iter()
        .zip(&source.channels)
        .map(|(&rx, tx)| Step {
            rx,
            tx_channel: tx,
            tx_device: &source.device,
        })
        .collect()
}

/// The verdict of choosing `wanted` on `sw` from the receiver's read after
/// it: the line to show when every RX channel reads back on it; the
/// reason otherwise (the read failed, or what it reads back).
pub fn verdict(
    sw: &Switch,
    wanted: &Source,
    after: Result<Vec<RxChannel>, String>,
) -> Result<String, String> {
    let (name, label) = (&sw.label, &wanted.label);
    let after = after.map_err(|e| format!("{name}: {label} was asked, but not read back: {e}"))?;
    if matches(sw, wanted, &after) {
        Ok(format!(
            "{name}: switched to {label}; it reads back {label}"
        ))
    } else {
        let back = state(sw, &current(sw, &Ok(after)));
        Err(format!("{name}: {label} was asked; it reads back {back}"))
    }
}

/// Chooses `wanted` on `sw` (see the module): `read` reads the receiver's
/// RX channels, `subscribe` subscribes one RX channel. Nothing is written
/// when the read shows `wanted` on already; the subscriptions stop at the
/// first refused. The line to show, or the reason.
pub fn set(
    sw: &Switch,
    wanted: &Source,
    mut read: impl FnMut() -> Result<Vec<RxChannel>, String>,
    mut subscribe: impl FnMut(Step<'_>) -> Result<(), String>,
) -> Result<String, String> {
    let (name, label) = (&sw.label, &wanted.label);
    let before = read().map_err(|e| format!("{name}: the receiver was not read: {e}"))?;
    if matches(sw, wanted, &before) {
        return Ok(format!("{name}: already {label}; nothing written"));
    }
    for step in plan(sw, wanted) {
        subscribe(step)
            .map_err(|e| format!("{name}: RX {} was not subscribed to {label}: {e}", step.rx))?;
    }
    verdict(sw, wanted, read())
}

/// The RX channels of `sw`'s receiver, found by `scanner`'s discovery,
/// read through its ARC from the PC's address that found it (the tray's
/// round, `dtray --print`).
pub fn read_live(scanner: &Scanner, sw: &Switch) -> Result<Vec<RxChannel>, String> {
    let at = scanner.remote(&sw.device)?;
    arc::call_from(at.from, at.arc, |c| {
        control::rx_channels(c).map_err(|e| e.to_string())
    })
}

/// [`set`] on `sw`'s receiver, found by `scanner`'s discovery, through
/// its ARC (the tray's click, `dtray --switch`).
pub fn set_live(scanner: &Scanner, sw: &Switch, wanted: &Source) -> Result<String, String> {
    let at = scanner
        .remote(&sw.device)
        .map_err(|e| format!("{}: {e}", sw.label))?;
    set(
        sw,
        wanted,
        || {
            arc::call_from(at.from, at.arc, |c| {
                control::rx_channels(c).map_err(|e| e.to_string())
            })
        },
        |s| {
            arc::call_from(at.from, at.arc, |c| {
                control::subscribe(c, s.rx, s.tx_channel, s.tx_device).map_err(|e| e.to_string())
            })
        },
    )
}

/// Whether the switches are read at `tick`: never read yet (`last` is
/// `None`), or last read [`SETTINGS_EVERY`] ticks ago or more (ticks
/// wrap), as the devices' settings are.
pub fn due(last: Option<u32>, tick: u32) -> bool {
    last.is_none_or(|t| tick.wrapping_sub(t) >= SETTINGS_EVERY)
}

/// One switch's part of the tray menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The submenu's text ([`state`]'s `<label>: <state>`).
    pub text: String,
    /// Each source's label and whether it is the current one.
    pub sources: Vec<(String, bool)>,
    /// Why the receiver is unread, when it is.
    pub note: Option<String>,
}

/// Each switch's menu part, with its current source (`currents`, in the
/// same order; a switch without one is unread).
pub fn items(switches: &[Switch], currents: &[Current]) -> Vec<Item> {
    switches
        .iter()
        .enumerate()
        .map(|(i, sw)| {
            let c = currents
                .get(i)
                .cloned()
                .unwrap_or_else(|| Current::Unread("not read yet".to_string()));
            Item {
                text: format!("{}: {}", sw.label, state(sw, &c)),
                sources: sw
                    .sources
                    .iter()
                    .enumerate()
                    .map(|(j, s)| (s.label.clone(), c == Current::Source(j)))
                    .collect(),
                note: match c {
                    Current::Unread(why) => Some(why),
                    _ => None,
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
