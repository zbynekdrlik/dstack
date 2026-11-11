// SPDX-License-Identifier: MIT OR Apache-2.0

//! `dtray --set-latency <device> <ms>` (dstack #104): the tray menu's own
//! latency write from the command line, for a PC with no usable display
//! and for a script that checks it. The device is one [`crate::discover`]
//! lists on this PC, by name (case-insensitive, exact: [`find`]); the
//! value is one of its menu's ([`crate::latency::pick`] on the choices
//! [`menu::shapes_of`] builds from its ARC settings); a value already
//! configured writes nothing ([`menu::write_needed`]); a write goes through
//! the same call as a menu click ([`dstack::arc::control::set_latency`]),
//! and the configured latency read back after it is the verdict
//! ([`crate::latency::read_back`]).
//!
//! [`latency()`] takes the ARC's settings read and its write as closures, so
//! a test drives every outcome without a device; the binary passes
//! [`crate::arc::call`] with `control::settings` and `control::set_latency`.

use std::net::SocketAddrV4;

use dstack::arc::control::Settings;

use crate::discover::Local;
use crate::latency;
use crate::menu::{self, Known};

/// The device of `devices` named `name`, case-insensitive and exact; the
/// reason otherwise (none of that name, with the names found; or more than
/// one).
pub fn find<'a>(devices: &'a [Local], name: &str) -> Result<&'a Local, String> {
    let wanted = name.to_lowercase();
    let matches: Vec<&Local> = devices
        .iter()
        .filter(|d| d.name.to_lowercase() == wanted)
        .collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => {
            let found: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();
            let found = if found.is_empty() {
                "none".to_string()
            } else {
                found.join(", ")
            };
            Err(format!(
                "no dstack device named {name} on this PC (found: {found})"
            ))
        }
        more => Err(format!(
            "{} dstack devices on this PC are named {name}; nothing written",
            more.len()
        )),
    }
}

/// Sets the latency of the device of `devices` named `name` to the value
/// `ms` names (see the module): `read` reads a device's ARC settings,
/// `write` sets its latency. The line to print when the latency is (or
/// already was) the value asked; the reason otherwise: no such device, its
/// settings unreadable, a value it does not offer, the write refused, or a
/// read-back that is not the value.
pub fn latency(
    devices: &[Local],
    name: &str,
    ms: &str,
    mut read: impl FnMut(SocketAddrV4) -> Result<Settings, String>,
    write: impl FnOnce(SocketAddrV4, u32) -> Result<Settings, String>,
) -> Result<String, String> {
    let device = find(devices, name)?;
    let name = &device.name;
    let before =
        read(device.arc).map_err(|e| format!("{name}: its settings were not read: {e}"))?;
    let mut known = Known::default();
    known.record(device.arc, Some(before), 0);
    let shapes = menu::shapes_of(std::slice::from_ref(device), &known, &[]);
    let choices: &[latency::Choice] = shapes.first().map_or(&[], |s| &s.choices);
    let ns = latency::pick(choices, ms).map_err(|e| format!("{name}: {e}"))?;
    if !menu::write_needed(&shapes, device.arc, ns) {
        return Ok(format!(
            "{name}: the latency is already {}; nothing written",
            latency::label(ns)
        ));
    }
    write(device.arc, ns).map_err(|e| format!("{name}: {}", menu::write_failed(ns, &e)))?;
    let after = read(device.arc)
        .map_err(|e| format!("{name}: the latency was written, but not read back: {e}"))?;
    latency::read_back(ns, after.latency_configured_ns)
        .map(|line| format!("{name}: {line}"))
        .map_err(|e| format!("{name}: {e}"))
}

#[cfg(test)]
mod tests;
