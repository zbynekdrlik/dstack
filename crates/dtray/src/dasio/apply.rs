// SPDX-License-Identifier: MIT OR Apache-2.0

//! One apply of the settings window (dstack #246), in order: the live RX
//! latency through the device's ARC; for restart changes, the wait until
//! the DAW releases the driver (only when the shared memory changes shape:
//! [`super::Plan::release`]), then the elevated helper (the file written,
//! the service restarted); last, the device read back until it states
//! every value, and the verdict ([`super::verify`]).
//!
//! The steps that touch the PC are [`Ops`]: the window passes the real
//! ones (its worker thread), a test passes fakes.

use super::{Fields, Plan, ReadBack, verify};

/// The PC's side of an apply.
pub trait Ops {
    /// Writes the device's RX latency through its ARC.
    fn set_latency(&mut self, ns: u32) -> Result<(), String>;
    /// Whether the DAW holds the driver now, from the device's status
    /// (`None`: not stated, or the device not running).
    fn daw_attached(&mut self) -> Option<bool>;
    /// One poll period (a second).
    fn wait(&mut self);
    /// The elevated helper: the file written and the service restarted;
    /// its line.
    fn install(&mut self, config: &str) -> Result<String, String>;
    /// One read of the device's values (it may still be starting).
    fn read_back(&mut self) -> ReadBack;
}

/// How many polls the DAW gets to release the driver (10 minutes).
pub const RELEASE_POLLS: u32 = 600;
/// How many polls the restarted device gets to state every value.
pub const READ_POLLS: u32 = 60;

/// Whether `back` states every value [`verify`] checks for `want`.
pub fn complete(want: &Fields, back: &ReadBack) -> bool {
    back.block.is_some()
        && back.rx.is_some()
        && back.tx.is_some()
        && back.latency_ns.is_some()
        && (want.iface.is_none() || back.iface.is_some())
}

/// Runs `plan` (for the values `want`) on `ops`, each step's text given
/// to `say`: the verdict line, or the reason it stopped (which says what
/// was already done).
pub fn run(
    plan: &Plan,
    want: &Fields,
    ops: &mut impl Ops,
    mut say: impl FnMut(&str),
) -> Result<String, String> {
    if let Some(ns) = plan.latency {
        say("Setting the RX latency…");
        ops.set_latency(ns)
            .map_err(|e| format!("the RX latency was not set: {e}"))?;
    }
    if let Some(config) = &plan.config {
        if plan.release {
            let mut polls = 0;
            while ops.daw_attached() == Some(true) {
                if polls == 0 {
                    say(
                        "Waiting for the DAW to release the driver (Ableton: Options → Audio → Driver: None, then Apply)…",
                    );
                }
                if polls == RELEASE_POLLS {
                    return Err(
                        "the DAW did not release the driver within 10 minutes; nothing written"
                            .to_string(),
                    );
                }
                ops.wait();
                polls += 1;
            }
        }
        say(
            "Writing the settings and restarting the instance (Windows asks for administrator rights)…",
        );
        ops.install(config)?;
        say("Restarted; reading the values back…");
    }
    let mut back = ops.read_back();
    let mut polls = 0;
    while !complete(want, &back) && polls < READ_POLLS {
        ops.wait();
        polls += 1;
        back = ops.read_back();
    }
    verify(want, &back)
}

#[cfg(test)]
mod tests;
