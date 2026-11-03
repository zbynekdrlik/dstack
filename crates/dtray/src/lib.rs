// SPDX-License-Identifier: MIT OR Apache-2.0

//! dtray: one tray icon per PC for every dstack device living there
//! (dlink apps, dasio, dstack-dev), dstack #104. It reads; the one thing it
//! writes is a device's latency, chosen in its menu.
//!
//! - [`discover`]: the devices on this PC. mDNS discovery on each of the
//!   PC's own IPv4 addresses keeps the ARC records that sit on one of them
//!   and carry the local status port (`dstack_ctl`,
//!   [`dstack::announce::STATUS_PORT_KEY`]); only our devices have it.
//! - [`probe`]: one `status` request to a device's local endpoint on
//!   127.0.0.1 ([`dstack::ctl::local`]), its JSON line back.
//! - [`expected`]: the devices this PC should have, one registration file
//!   each in a folder its installers write.
//! - [`model`]: the readings as the tray shows them: a colour, a tooltip
//!   and a line per device ([`model::Tracker`]), the app's own part of the
//!   status (`app`: its `line` and `state`) and the expected devices not
//!   running included.
//! - [`latency`]: a device's latency menu from its ARC settings.
//! - [`menu`]: the Windows menu's decisions (when to read settings, when
//!   to build the menu again, whether a choice writes).
//! - [`icon`]: the icon's pixels in a colour.
//!
//! The binary shows them as a tray icon on Windows, prints them once with
//! `dtray --print` on every platform, and sets or removes its own start at
//! logon with `dtray --autostart on|off` (Windows).

use std::path::Path;

pub mod discover;
pub mod expected;
pub mod icon;
pub mod latency;
pub mod menu;
pub mod model;
pub mod probe;

/// What the command line asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The tray icon (Windows).
    Tray,
    /// One discovery pass, the view printed to stdout.
    Print,
    /// Start the tray at the user's logon (`true`) or not (`false`)
    /// (Windows).
    Autostart(bool),
}

/// The usage line.
pub const USAGE: &str = "usage: dtray [--print | --autostart on|off]";

/// The mode of the arguments after the program's name: none for the tray,
/// `--print` to print once, `--autostart on` or `off`; anything else is the
/// usage error.
pub fn mode_of<I: IntoIterator<Item = String>>(args: I) -> Result<Mode, &'static str> {
    let args: Vec<String> = args.into_iter().collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] => Ok(Mode::Tray),
        ["--print"] => Ok(Mode::Print),
        ["--autostart", "on"] => Ok(Mode::Autostart(true)),
        ["--autostart", "off"] => Ok(Mode::Autostart(false)),
        _ => Err(USAGE),
    }
}

/// The name of dtray's value under the user's
/// `Software\Microsoft\Windows\CurrentVersion\Run` key.
pub const RUN_VALUE_NAME: &str = "dtray";

/// The Run value's text for the program at `exe`: the path in double
/// quotes, so a path with spaces starts as one program; `None` for a path
/// that is not valid Unicode (never a lossy, wrong path).
pub fn run_value(exe: &Path) -> Option<String> {
    exe.to_str().map(|p| format!("\"{p}\""))
}

#[cfg(test)]
mod tests;
