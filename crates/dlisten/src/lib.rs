// SPDX-License-Identifier: MIT OR Apache-2.0

//! dlisten (dstack #139): plays Dante audio on a local output, a PC's or a
//! notebook's speakers, or a monitor's audio. A small tool, an MVP on dlink:
//! no low latency, no GUI.
//!
//! The root problem is two clocks. Dante media runs on PTP; the output
//! device runs on its own crystal, often at another rate (44.1 or 48 kHz
//! on the device, 48 or 96 kHz on the network). A plain copy drifts into
//! underruns or overruns within minutes. So:
//!
//! - A dlink device (`<host>-dlisten`, RX only, 2 channels L/R by default)
//!   whose RX channels the output callback reads itself (dstack #307,
//!   `dlink::bridge::RxPull`): at each callback it drains the flow sockets
//!   into the RX rings and reads them by position into a lock-free
//!   [`fifo::Fifo`], so a late pump never empties it. While no output
//!   plays, nothing drains: the flow reads silent and is asked again once
//!   one plays.
//! - The output callback (cpal: WASAPI shared on Windows, ALSA on Linux,
//!   at the output's own default rate) then pulls through the
//!   [`player::Player`]:
//!   `dstack-resample`'s adjustable converter, its ratio steered by the
//!   ASRC controller (SongPlayer's laws) on the FIFO's depth, kept at the
//!   app buffer (dstack #247): the output's 10 ms WASAPI period and one
//!   pump block, 12.667 ms at 48 kHz, or `--app-buffer-ms`.
//! - [`map`] puts our channels on the output's: L/R on the first two, a
//!   mono source on both.
//!
//! dlisten has no window and no tray of its own (the owner's ROZHODNUTÉ of
//! 6.11.2026): dtray, the one tray for every Dante app on the PC, shows its
//! status part ([`status::app_status`]: the output and its rate, whether
//! audio flows, the depth, ppm and underruns, and the outputs it offers)
//! and its Output menu sends `app output <n>` ([`status::parse_command`])
//! to the device's app command hook (`dlink::Link::set_app_command`). The
//! choice is kept in a settings file next to the device's state file
//! ([`settings`]) and restored at start ([`follow::start`]: `--output`
//! wins; a saved output that is gone falls back to the default). A run
//! with `--output` plays on that output only ([`follow`], dstack #506):
//! while none matches it plays nothing and waits, and opens it when it
//! appears.
//!
//! Everything here is testable without audio hardware: the output is an
//! external clock, the only thing the tests simulate. The binary
//! (`src/main.rs`) is the glue: the command line ([`cli`]), cpal, dlink,
//! the log and `--autostart` ([`autostart`]).

pub mod autostart;
pub mod cli;
pub mod follow;
pub mod status;

// The endpoint bridge dlisten shares with dsend (dstack #164), under the
// paths dlisten had.
pub use dstack_endpoint::{alert, fifo, map, outputs, player, settings};
