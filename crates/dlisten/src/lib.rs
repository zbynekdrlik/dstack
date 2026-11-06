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
//!   whose pump writes each block into a lock-free [`fifo::Fifo`].
//! - The output callback (cpal: WASAPI shared on Windows, ALSA on Linux,
//!   at the output's own default rate) pulls through the [`player::Player`]:
//!   `dstack-resample`'s adjustable converter, its ratio steered by the
//!   ASRC controller (SongPlayer's laws) on the FIFO's depth, about
//!   [`player::TARGET_S`] (40 ms).
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
//! ([`settings`]) and restored at start ([`outputs::choose`]: `--output`
//! wins; a saved output that is gone falls back to the default).
//!
//! Everything here is testable without audio hardware: the output is an
//! external clock, the only thing the tests simulate. The binary
//! (`src/main.rs`) is the glue: the command line ([`cli`]), cpal, dlink,
//! the log and `--autostart` ([`autostart`]).

pub mod autostart;
pub mod cli;
pub mod feed;
pub mod fifo;
pub mod map;
pub mod outputs;
pub mod player;
pub mod settings;
pub mod status;
