// SPDX-License-Identifier: MIT OR Apache-2.0

//! dsend (dstack #164): sends a PC's sound into Dante, the inverse of
//! dlisten. A small tool on dlink: no low latency, no GUI.
//!
//! The same two clocks as dlisten, the other way round: the captured
//! output (a Windows render endpoint, its loopback through WASAPI shared
//! mode) runs on its own crystal at its mix rate (44.1 or 48 kHz); Dante
//! media runs on PTP at the network's rate. So:
//!
//! - The capture callback (cpal: an output device opened as an input
//!   stream is WASAPI loopback) writes each packet into a lock-free
//!   [`dstack_endpoint::fifo::Fifo`] ([`capture::push_interleaved`]).
//! - A dlink device (`<host>-dsend`, TX only, 2 channels L and R) whose
//!   pump pulls every block through the [`dstack_endpoint::player::Player`]
//!   ([`source::Source`]): `dstack-resample`'s drifting converter, its ratio
//!   steered by the ASRC controller on the FIFO's depth, kept at the app
//!   buffer (dstack #247): WASAPI's 10 ms period and one pump block,
//!   11.333 ms at 96 kHz, or `--app-buffer-ms`.
//! - [`dstack_endpoint::map`] puts the endpoint's channels on ours: L/R
//!   from the first two (a 5.1 or 7.1 endpoint's front pair), a mono
//!   endpoint on both.
//! - WASAPI loopback delivers nothing while nothing plays; the binary keeps
//!   a silent render stream open on the captured output, so the capture
//!   runs on the endpoint's clock all the time and an idle output is plain
//!   silence on Dante. Should the capture stop anyway, the pump sends
//!   silence, counts the underrun and primes again.
//! - A busy desktop must not starve the capture, whatever the process's
//!   priority class (dstack #198): the capture thread joins MMCSS "Pro
//!   Audio" on its first callback, as dlink's pump and media threads do at
//!   their start; the loopback buffer rides out a 200 ms stall; a
//!   discontinuity WASAPI still flags is logged in ms
//!   ([`capture::GapWatch`]).
//!
//! dsend has no window and no tray of its own: dtray shows its status
//! ([`status::app_status`]: the captured output and its rate, whether the
//! capture runs, the depth, ppm and underruns, and the outputs it offers)
//! and its Output menu sends `app output <n>`
//! ([`status::parse_command`]); the choice is kept next to the device's
//! state file and restored at start, as dlisten's. With no `--output` and
//! no saved choice dsend takes its own output, "Speakers (dsend)" from the
//! dsend-audio driver (`drivers/dsend-audio`, dstack #184), when it exists
//! ([`dstack_endpoint::outputs::DSEND`]), else the default output; dlisten
//! never takes it by default.
//!
//! The capture is Windows only: Linux has no loopback of an output in
//! cpal's ALSA host (a monitor source is PulseAudio's or PipeWire's), so
//! the binary refuses to run there with a clear error
//! ([`cli::capture_check`]). Everything here but the binary's glue is
//! cross-platform and tested without audio hardware.

pub mod capture;
pub mod cli;
pub mod source;
pub mod status;
