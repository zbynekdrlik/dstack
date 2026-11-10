// SPDX-License-Identifier: MIT OR Apache-2.0

//! A PC's audio endpoint (an output device: speakers, a monitor's audio, a
//! sound card) bridged to a dlink device: what dlisten (Dante into an
//! output, dstack #139) and dsend (an output's sound into Dante, dstack
//! #164) share, so the two directions run one ASRC path, never two copies.
//!
//! The root problem is two clocks: Dante media runs on PTP, the endpoint on
//! its own crystal, often at another rate. One side writes a lock-free
//! [`fifo::Fifo`] on its clock; the other pulls through the
//! [`player::Player`] on its own: `dstack-resample`'s drifting converter,
//! its ratio steered by the ASRC controller on the FIFO's depth (about
//! [`player::TARGET_S`], 40 ms). In dlisten the network writes and the
//! output callback pulls; in dsend the endpoint's loopback capture writes
//! and the device's pump pulls.
//!
//! - [`map`]: the producer's channels on the consumer's (L/R, mono on both).
//! - [`outputs`] and [`settings`]: the endpoint's choice (`--output`, the
//!   one dtray's menu saved, the default) and the file that keeps it.
//! - [`status`]: the shared parts of the app status dtray shows and of its
//!   `app output <n>` command.
//! - [`cli`]: the command line's shared parts (ports, the default name, the
//!   state file's folder).
//! - [`alert`]: where a failure the user must see goes (Windows).
//! - [`endpoints`] and [`host`]: thin glue (cpal's device list, the PC's
//!   name and routed address, the log, Ctrl+C), run by the binaries.

pub mod alert;
pub mod cli;
pub mod endpoints;
pub mod fifo;
pub mod host;
pub mod map;
pub mod outputs;
pub mod player;
pub mod settings;
pub mod status;
