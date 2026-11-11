// SPDX-License-Identifier: MIT OR Apache-2.0

//! dtray: one tray icon per PC for every dstack device living there
//! (dlink apps, dasio, dstack-dev), dstack #104. It reads; it writes only
//! what its menu chooses: a device's latency, and an app's output (dstack
//! #139: dlisten; [`output`], sent as the app's own command).
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
//! - [`arc`]: one exchange with a local device's ARC (the menu's settings
//!   reads and latency writes).
//! - [`menu`]: the tray menu's decisions (when to read settings, when to
//!   build the menu again, whether a choice writes).
//! - [`set`]: `dtray --set-latency`, the menu's latency write from the
//!   command line.
//! - [`output`]: an app's Output menu from its status part, and the
//!   request a choice sends.
//! - [`icon`]: the icon's pixels in a colour.
//! - [`desktop`]: the Linux desktop's files (the single-instance lock, the
//!   autostart entry and its text).
//!
//! The binary shows them as a tray icon on Windows and Linux, prints them
//! once with `dtray --print` on every platform, sets or removes its own
//! start at logon with `dtray --autostart on|off` (Windows and Linux), and
//! sets a device's latency as its menu does with
//! `dtray --set-latency <device> <ms>` on every platform (a PC with no
//! usable display, a script's check; [`set`]).
//!
//! On Windows `dtray.exe` is a GUI-subsystem program: a console does not
//! wait for it, and PowerShell's `&` neither waits nor sets
//! `$LASTEXITCODE`. An installer that runs `--autostart` starts it with
//! `Start-Process dtray.exe -ArgumentList '--autostart','on' -Wait -PassThru`
//! and reads the returned process's `ExitCode` (dasio's installer).

use std::path::Path;

pub mod arc;
#[cfg(unix)]
pub mod desktop;
pub mod discover;
pub mod expected;
pub mod icon;
pub mod latency;
pub mod menu;
pub mod model;
pub mod output;
pub mod probe;
pub mod set;

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// The tray icon (Windows and Linux).
    Tray,
    /// One discovery pass, the view printed to stdout.
    Print,
    /// Start the tray at the user's logon (`true`) or not (`false`)
    /// (Windows and Linux).
    Autostart(bool),
    /// The version line on stdout ([`version_line`]).
    Version,
    /// Set the latency of the local device named `device` to the value
    /// `ms` names, as its menu does ([`set::latency()`]).
    SetLatency { device: String, ms: String },
}

/// dtray's version as one line: the dstack release it was built from
/// (the tray's menu shows the same; `dtray --version`, `--print`).
pub fn version_line() -> String {
    format!("dtray {}", env!("CARGO_PKG_VERSION"))
}

/// The usage line.
pub const USAGE: &str =
    "usage: dtray [--print | --version | --autostart on|off | --set-latency <device> <ms>]";

/// The mode of the arguments after the program's name: none for the tray,
/// `--print` to print once, `--autostart on` or `off`,
/// `--set-latency <device> <ms>`; anything else is the usage error.
pub fn mode_of<I: IntoIterator<Item = String>>(args: I) -> Result<Mode, &'static str> {
    let args: Vec<String> = args.into_iter().collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] => Ok(Mode::Tray),
        ["--print"] => Ok(Mode::Print),
        ["--version"] => Ok(Mode::Version),
        ["--autostart", "on"] => Ok(Mode::Autostart(true)),
        ["--autostart", "off"] => Ok(Mode::Autostart(false)),
        ["--set-latency", device, ms] => Ok(Mode::SetLatency {
            device: device.to_string(),
            ms: ms.to_string(),
        }),
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
