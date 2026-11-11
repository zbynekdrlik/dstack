// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.
// Protocol facts (byte layouts, opcodes, behaviour) checked against
// inferno (https://github.com/teodly/inferno) as a reference; the code is
// our own.

//! iemmixer's own Dante device (Gen 3 design §5.3, phase A): a box that
//! Dante Controller lists under its own name, with its RX and TX channels,
//! and whose channel renames stick across restarts. Phase B (plan
//! `docs/superpowers/plans/2026-10-26-gen3-phase-b-audio-through.md`) adds
//! the media clock and the audio path, piece by piece; the device never
//! sends PTP.
//!
//! - [`device`]: the config (a private TOML on the box) and the persisted
//!   names and RX subscriptions, with Dante's name rules.
//! - [`discover`]: every Dante device on one interface, for a controller
//!   (`_netaudio-arc` and `_netaudio-dbc` records, live changes), not tied
//!   to our own device.
//! - [`wire`]: the request/response datagram ARC and CMC share.
//! - [`arc`]: the control server on UDP 4440: names, channel lists,
//!   renames, the TX and RX flow lists, our RX subscriptions (set on our
//!   own device, kept in the state file); every opcode inferno answers is
//!   answered as inferno answers it, the rest get no reply and are counted
//!   in one warning a minute.
//! - [`cmc`]: the advertisement on UDP 8800.
//! - [`info`]: the info requests on UDP 8700 (replies to the info group
//!   224.0.0.231:8702), the channel-change notification there, and the
//!   heartbeat every second to 224.0.0.233:8708 (with the frequency, peak
//!   and RX lateness records once the media clock is ready); Dante
//!   Controller goes on to ARC only once its board info request is
//!   answered.
//! - [`announce`]: the mDNS records Dante Controller browses for, and their
//!   announcer.
//! - [`clock`]: the media clock: PTP time as `CLOCK_MONOTONIC` + k, k from
//!   dsync's time server on the box (the box's one PTP owner), and the
//!   packets' stamps.
//! - [`flows`]: flow control on UDP 4455: a receiver's request for a flow
//!   of our TX channels, answered as inferno answers it, its stop and its
//!   update; and our own requests to a transmitter (the RX side).
//! - [`subscribe`]: our RX subscriptions: which transmitter's channel each
//!   RX channel receives, resolved from its mDNS record, and when to ask
//!   for a flow or stop one (at most one request per transmitter per 9 s).
//! - [`media`]: the audio packet codec, the jitter ring of an RX channel,
//!   the TX test sources and the checkers of what comes back, the TX flows
//!   with the thread that sends them, and the RX flows with the thread that
//!   receives them.
//! - [`daemon`]: the `iem-dante-dev` binary's command line, its interface
//!   and address, the loop that serves the device and announces it again
//!   after renames, and the running audio side ([`daemon::media`]: the TX
//!   and RX threads, our RX's manager driven every 250 ms, the stop).
//! - [`ctl`]: the local control socket: the status (numbers and the
//!   device's name), the TX test sources and the RX checks, set at
//!   runtime; the request counts.
//! - [`stream`] (phase C, plan
//!   `docs/superpowers/plans/2026-10-26-gen3-phase-c-engine-on-linux.md`):
//!   the engine's pump on the media clock: it reads the RX rings at the
//!   playout point, calls the engine's `Process` every 32 frames and writes
//!   the engine's TX rings, which the TX thread sends as
//!   [`media::source::Source::Engine`]. It knows nothing about mixing.
//! - `host` (phase C, Linux): the device as a library: its setup
//!   (`host::prepare`), its serving on a thread of its own
//!   (`host::Prepared::start`) and its bounded stop (`host::Host::stop`);
//!   `iem-dante-dev` and the engine host it the same way.
//! - [`health`]: the device's health as an application reads it: one
//!   control-socket status line (`health::parse_status`,
//!   `health::read_status`) and the device's own reasons from two reads
//!   (`health::device_reasons`); an application builds its box verdict on
//!   them.
//!
//! The device answers for itself only: it never sends a command that
//! changes another Dante device.

// The rtsan CI job (nightly, `--cfg iem_rtsan`) marks `stream::Pump::block`
// as a real-time context (I7), as iem-engine marks `Processor::process`.
#![cfg_attr(iem_rtsan, feature(sanitize))]

pub mod announce;
pub mod arc;
pub mod clock;
pub mod cmc;
pub mod ctl;
pub mod daemon;
pub mod device;
pub mod discover;
pub mod embed;
pub mod flows;
pub mod health;
#[cfg(any(target_os = "linux", windows))]
pub mod host;
pub mod info;
pub mod latency;
pub mod lso;
pub mod media;
pub mod meter;
pub mod qdisc;
pub mod stream;
pub mod subscribe;

#[cfg(test)]
mod test_log;
pub mod traffic;
pub mod winsock;
pub mod wire;
