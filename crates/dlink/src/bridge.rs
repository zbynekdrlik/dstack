// SPDX-License-Identifier: MIT OR Apache-2.0

//! The bridge between an app's own clock and the device's (dstack #241,
//! moved here from dstack-endpoint, where dsend and dlisten use it since
//! dstack #139 and #164): an app whose audio runs on its own clock (OBS's
//! mix, a sound card) and a dlink device on the network's PTP clock.
//!
//! One side writes a lock-free [`Fifo`] on its clock; the other pulls
//! through the [`Player`] on its own: `dstack-resample`'s drifting
//! converter, its ratio steered by the ASRC controller on the FIFO's depth
//! at the device's [`AppBuffer`] (dstack #247). [`map`] puts the producer's
//! channels on the consumer's (L/R, mono on both).
//!
//! An app sending its audio into Dante (dsend's direction) makes the FIFO
//! with [`fifo_for`] at its own rate (the FIFO's producer), writes it on
//! its own audio thread with [`Fifo::push_with`], and in its
//! [`Process`](crate::Process) fills the device's TX block with
//! [`Player::fill_with`], made with [`Player::new`] for the app's rate and
//! the device's and the device's [`AppBuffer`] (`crate::Config::bridge_buffer`). The names come from dlisten's direction: in this API
//! `net_rate` is the rate of the FIFO's producer (here the app) and
//! `out_rate` the caller's (here the device). [`Stats`] carries the
//! player's counts to a status thread. Neither call takes a lock,
//! allocates or makes a system call. dlisten runs it the other way round:
//! the device's RX writes the FIFO and an output callback pulls.
//!
//! The bridge adds its FIFO's depth to the device's path (`dlink::split`):
//! the app buffer `app_buffer_ns` ([`buffer`], dstack #247, the owner's
//! ROZHODNUTÉ of 13.11.2026), a fixed term. Its default is the app's own
//! fixed chunk plus one pump block (OBS 1024 frames at 48 kHz + block:
//! 22.667 ms at a 96 kHz device; dsend and dlisten 10 ms + block:
//! 11.333 ms); a value the app sets wins and never changes by itself. A
//! dropout is counted ([`Stats::underruns`]), never absorbed by growing the
//! latency. The app states its chunk with `crate::Config::app_buffer`; the
//! device logs the term at its start beside dstack #244's.

pub mod buffer;
pub mod fifo;
pub mod map;
pub mod player;

pub use buffer::{AppBuffer, MAX_APP_BUFFER_NS, MIN_APP_BUFFER_NS, chunk_ns};
pub use fifo::Fifo;
pub use player::{Player, Stats, fifo_for};
