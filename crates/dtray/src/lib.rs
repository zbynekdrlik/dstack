// SPDX-License-Identifier: MIT OR Apache-2.0

//! dtray: one tray icon per PC for every dstack device living there
//! (dlink apps, dasio, dstack-dev), dstack #104. It only reads; no device
//! or app changes for it.
//!
//! - [`discover`]: the devices on this PC. mDNS discovery on each of the
//!   PC's own IPv4 addresses keeps the ARC records that sit on one of them
//!   and carry the local status port (`dstack_ctl`,
//!   [`dstack::announce::STATUS_PORT_KEY`]); only our devices have it.
//! - [`probe`]: one `status` request to a device's local endpoint on
//!   127.0.0.1 ([`dstack::ctl::local`]), its JSON line back.
//! - [`model`]: the readings as the tray shows them: a colour, a tooltip
//!   and a line per device ([`model::Tracker`]).
//! - [`icon`]: the icon's pixels in a colour.
//!
//! The binary shows them as a tray icon on Windows, and prints them once
//! with `dtray --print` on every platform.

pub mod discover;
pub mod icon;
pub mod model;
pub mod probe;

/// What the command line asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The tray icon (Windows).
    Tray,
    /// One discovery pass, the view printed to stdout.
    Print,
}

/// The usage line.
pub const USAGE: &str = "usage: dtray [--print]";

/// The mode of the arguments after the program's name: none for the tray,
/// `--print` to print once; anything else is the usage error.
pub fn mode_of<I: IntoIterator<Item = String>>(args: I) -> Result<Mode, &'static str> {
    let args: Vec<String> = args.into_iter().collect();
    match args.as_slice() {
        [] => Ok(Mode::Tray),
        [one] if one == "--print" => Ok(Mode::Print),
        _ => Err(USAGE),
    }
}

#[cfg(test)]
mod tests;
