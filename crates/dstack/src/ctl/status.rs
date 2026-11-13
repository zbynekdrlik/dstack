// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The status writer ([`status`]): the device's status line as one JSON
//! object, and the app's part of it ([`app_object`]).

use std::fmt::{Display, Write as _};

use super::clock::clock_part;
use super::{CheckKind, CheckStats, check_word, lock, source_word};
use crate::clock::latency_samples;
use crate::daemon::media::Media;
use crate::media::ring::Counters;
use crate::media::rx::RxCounts;
use crate::media::source::Source;
use crate::media::tx::{LaneCounts, MAX_FLOWS};
use crate::subscribe::SubStatus;

/// A JSON line built in place: keys are ours, values numbers, `null` or
/// fixed words, so nothing needs escaping; the one free text (the device's
/// name, [`Json::text`]) is escaped.
pub(super) struct Json {
    out: String,
    /// Nothing written in the open object or array yet.
    fresh: bool,
}

impl Json {
    fn new() -> Self {
        Json {
            out: String::with_capacity(4096),
            fresh: true,
        }
    }

    fn comma(&mut self) {
        if !self.fresh {
            self.out.push(',');
        }
        self.fresh = false;
    }

    fn key(&mut self, key: &str) {
        self.comma();
        let _ = write!(self.out, "\"{key}\":");
    }

    /// Opens an object or array, as `key`'s value or as an array item.
    pub(super) fn open(&mut self, key: Option<&str>, bracket: char) {
        match key {
            Some(k) => self.key(k),
            None => self.comma(),
        }
        self.out.push(bracket);
        self.fresh = true;
    }

    pub(super) fn close(&mut self, bracket: char) {
        self.out.push(bracket);
        self.fresh = false;
    }

    pub(super) fn num(&mut self, key: &str, v: impl Display) {
        self.key(key);
        let _ = write!(self.out, "{v}");
    }

    /// An array item.
    fn item(&mut self, v: impl Display) {
        self.comma();
        let _ = write!(self.out, "{v}");
    }

    pub(super) fn flag(&mut self, key: &str, v: bool) {
        self.num(key, u8::from(v));
    }

    pub(super) fn word(&mut self, key: &str, w: &'static str) {
        self.key(key);
        let _ = write!(self.out, "\"{w}\"");
    }

    /// A string value, escaped as JSON (a name comes from a controller's
    /// rename or the config).
    fn text(&mut self, key: &str, v: &str) {
        self.key(key);
        self.out.push_str(&serde_json::Value::from(v).to_string());
    }

    pub(super) fn opt(&mut self, key: &str, v: Option<impl Display>) {
        match v {
            Some(v) => self.num(key, v),
            None => {
                self.key(key);
                self.out.push_str("null");
            }
        }
    }
}

/// The status line: one JSON object, numbers and fixed words, besides the
/// device's own `name` and the app's part; never an address. Channels and
/// flows are numbered from 1 (flow `n` is slot `n − 1`); every count is
/// cumulative since the device started.
///
/// - `clock`: `ready` (0/1), `epoch` (re-anchors the media threads saw),
///   `polls`, `re_anchors`, `holdovers` (runs of polls in holdover),
///   `holdover` (the last poll was), `freq_ppb` (dsync's), `source`
///   (`dsync`, or `system`: the TEST clock, `host::ClockSource::System`).
/// - `requests`: `[{port, peer, op, n}]`, every datagram the device
///   received (ARC 4440, flow control 4455, info 8700, CMC 8800; `op` as
///   [`header_op`](super::header_op) and [`info_op`](super::info_op) read
///   it); `sent`: `[{peer, op, n}]`, our RX's flow requests (256) and stops
///   (257), and its ARC reads of transmitters' channels (4096 once a
///   directory read, 8192 and 8208 once a page); `uncounted`: past the
///   tables.
/// - `tx`: each TX flow slot that has or had a flow: `flow`, `live` (a flow
///   that sends), `fpp`, `channels`, `packets`, `send_errors` (packets refused after
///   their retries: lost), `send_retries` (packets the NIC refused for a
///   moment that went out on a retry, `media::tx::send_retry`), `keepalives`,
///   `rebootstraps`, `holes` (echo samples the ring did not hold),
///   `late_bins` (packets by how late they left past due, six counts at
///   the edges of `media::tx::LATE_EDGES_NS`: under 0.25, 0.5, 1, 2 and 5
///   ms, then 5 ms or more), `late_max_us` and `send_max_us` (the largest lateness and
///   the longest single send since the device started: maxima).
/// - `tx_bytes`: every TX slot's bytes on the wire since the device started
///   (each datagram plus its Ethernet, IP and UDP headers,
///   [`crate::media::tx::TxShared::bytes_sent`]):
///   another device's traffic watch on the same PC subtracts it from the
///   NIC's own count (dstack #203, [`crate::traffic`]).
/// - `name` (dstack #209): the device's name as it is now, the one its ARC
///   record announces (the config's, or a controller's rename since), as a
///   JSON string: a local reader can tell which device answered. The
///   traffic watch counts a neighbour's `tx_bytes` only when this is the
///   name it found the neighbour under.
/// - `rx`: each RX flow slot that has or had a flow: `flow`, `device`
///   (dstack #266: the transmitter's Dante name as the subscription that
///   asked for the flow writes it; absent for a slot without a flow now),
///   `source` (the transmitter's `address:port` its flow's last packet came
///   from; absent before a packet and after the slot's flow changed),
///   `granted`, `fpp`, `channels`, `latency` (samples), `packets`, `refused`, `stray`,
///   `out_of_order`, `gaps` and `gap_frames` (packets that began past the
///   end of the one before, and the frames skipped: packets that never
///   came), `socket_drops` (Linux: the kernel's drops at the slot's
///   socket), `late` (packets past playout + block), `late_at_read`
///   (packets with a frame the pump had already read: its channels' holes
///   and late samples), `wire_late`, `wake_delay_us` and
///   `wake_delay_peak_us` (dstack #234, Linux only, absent on Windows where
///   the kernel stamps no receive time: packets already late by `late`'s
///   rule when the kernel received them, and the largest wake delay, the RX
///   thread's pass time less the kernel's receive time, in µs, of the
///   heartbeat's last second and since the device started),
///   `network_latency_min_us`, `network_latency_p99_us` and
///   `network_latency_max_us` (dstack #244, Linux only, and only for a
///   second with a stamped packet: each packet's arrival, the kernel's
///   receive time on our PTP clock, less its sender's media-clock stamp
///   (its first frame: transit plus the packet's own frames and the
///   sender's lateness, an upper bound on the transit),
///   the least, the 99th percentile in 20 µs steps and the largest of the
///   heartbeat's last second, in µs; absent on Windows), `unplaced`,
///   `lateness` (the largest in the heartbeat's last second, samples; the
///   status takes nothing),
///   `lateness_peak` (the largest since the device started, samples),
///   `lateness_min_ns` (the packets' judge, phase D: the least lateness of
///   that second in ns, our clock's now less the packets' own stamps; absent
///   for a second without a placed packet), `keepalives`,
///   `keepalive_errors`, `recv_errors`, `multicast` (a transmitter's
///   multicast flow the slot joined: no request, no keepalive), `joins` and
///   `join_errors` (its multicast groups joined, and the joins that
///   failed); `rx_poll_errors`.
/// - `channels`: each RX channel subscribed or whose ring counted anything
///   (written, or read by the echo or a check): `rx`, `status`
///   (ARC 0x3000's code, 0 without a subscription), its ring's `written`,
///   `holes`, `late`, `duplicate`, `early` (samples).
/// - `sources`: each TX channel that is not silent: `tx`, `source`
///   (`counter`, `sine` with `hz` and `dbfs`, `echo` with `rx` and
///   `offset`).
/// - `checks`: each check: `rx`, `check` (`counter` or `sine` with `hz`),
///   `skipped`, and once its channel played: the counter's `checked`,
///   `blind`, `mismatch`, `mismatch_frames`, `dropout`, `dropout_frames`,
///   `slips`, `last_slip`, `lag` (null before a lock), `no_return`; the
///   sine's `checked`, `blind`, `discontinuities`, `discontinuity_frames`;
///   else `waiting` 1.
/// - `engine` (phase C), only once an engine is attached
///   ([`Media::attach_engine`]): its pump's `media` (1 on the media clock, 0
///   pacing itself), `offset` (D) and `playout` (P) in samples, `blocks`,
///   `late`, `missed` (blocks skipped by re-bootstraps), `overruns` (blocks
///   longer than one period), `rebootstraps`, `max_process_us`
///   ([`crate::stream::PumpView`]).
/// - `app` (dstack #104), only once the app set a hook
///   ([`Media::set_app_status`]), its thread's first call returned, and
///   while it returns a JSON object: that object, compact (the cached part:
///   the status never calls the hook), with `"stale":true` once the part is
///   over [`crate::daemon::media::app::STALE`] old. It is the app's: dtray
///   reads its `line`, `state` and `stale`; beside `name`, it is the one
///   part that may carry names.
pub fn status(media: &Media) -> String {
    let mut j = Json::new();
    j.open(None, '{');
    clock_part(&mut j, media);
    requests_part(&mut j, media);
    tx_part(&mut j, media);
    name_part(&mut j, media);
    rx_part(&mut j, media);
    sources_part(&mut j, media);
    checks_part(&mut j, media);
    engine_part(&mut j, media);
    app_part(&mut j, media);
    j.close('}');
    j.out
}

/// The longest app part the status carries, bytes of compact JSON: a
/// reader (dtray) refuses a whole status past its own cap, so an app's
/// part never makes its device's status unreadable.
pub const APP_MAX: usize = 4096;

/// `text` as one compact JSON object (on one line, whatever the app wrote)
/// of at most [`APP_MAX`] bytes; `None` for anything else.
pub fn app_object(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let compact = v.to_string();
    (v.is_object() && compact.len() <= APP_MAX).then_some(compact)
}

/// The device's name now: our RX manager's host, which follows every
/// rename ([`crate::arc::Flows::renamed`]).
fn name_part(j: &mut Json, media: &Media) {
    let name = lock(&media.manager).host().to_string();
    j.text("name", &name);
}

fn app_part(j: &mut Json, media: &Media) {
    let Some(part) = media.app_status() else {
        return;
    };
    j.key("app");
    j.out.push_str(&part);
}

fn requests_part(j: &mut Json, media: &Media) {
    let r = media.requests.counts();
    j.open(Some("requests"), '[');
    for e in &r.received {
        j.open(None, '{');
        j.num("port", e.port);
        j.num("peer", e.peer);
        j.num("op", e.op);
        j.num("n", e.n);
        j.close('}');
    }
    j.close(']');
    j.open(Some("sent"), '[');
    for e in &r.sent {
        j.open(None, '{');
        j.num("peer", e.peer);
        j.num("op", e.op);
        j.num("n", e.n);
        j.close('}');
    }
    j.close(']');
    j.num("uncounted", r.uncounted);
}

fn tx_part(j: &mut Json, media: &Media) {
    let flows: Vec<Option<(u16, usize, bool)>> = {
        let c = media.tx.lock();
        (0..MAX_FLOWS)
            .map(|s| {
                c.table
                    .flow(s)
                    .map(|f| (f.fpp, f.channels.len(), f.expired))
            })
            .collect()
    };
    j.open(Some("tx"), '[');
    for (slot, flow) in flows.into_iter().enumerate() {
        let lane = media.tx.lane(slot);
        if flow.is_none() && lane == LaneCounts::default() {
            continue;
        }
        let (fpp, channels, expired) = flow.unwrap_or((0, 0, true));
        j.open(None, '{');
        j.num("flow", slot + 1);
        j.flag("live", !expired);
        j.num("fpp", fpp);
        j.num("channels", channels);
        lane_fields(j, &lane);
        j.close('}');
    }
    j.close(']');
    j.num("tx_bytes", media.tx.bytes_sent());
}

/// A TX lane's counts as the status shows them (µs for the times).
fn lane_fields(j: &mut Json, lane: &LaneCounts) {
    j.num("packets", lane.packets);
    j.num("send_errors", lane.send_errors);
    j.num("send_retries", lane.send_retries);
    j.num("keepalives", lane.keepalives);
    j.num("rebootstraps", lane.rebootstraps);
    j.num("holes", lane.holes);
    j.open(Some("late_bins"), '[');
    for n in lane.late_bins {
        j.item(n);
    }
    j.close(']');
    j.num("late_max_us", lane.late_max_ns / 1_000);
    j.num("send_max_us", lane.send_max_ns / 1_000);
    j.num("age_max_us", lane.age_max_ns / 1_000);
    j.num("over_latency", lane.over_latency);
    j.num("receiver_latency_us", lane.receiver_latency_ns / 1_000);
    j.num("direct", lane.direct);
}

/// An RX flow as the status shows it.
#[derive(Default)]
struct RxFlow {
    granted: bool,
    /// A transmitter's multicast flow, joined (dstack #148).
    multicast: bool,
    fpp: u16,
    channels: usize,
    /// The negotiated latency (its transmitter's or ours), samples.
    latency: u64,
    /// The playout point: the latency, at least the fpp (`RxPlan::latency`).
    playout: u32,
    /// Read this much later than the device latency.
    delay: u16,
    /// A packet counts late past this lateness (playout + block).
    late_after: u64,
    /// The transmitter's advertised latency, samples (0: none).
    advertised: u64,
    /// The playout is below it (the owner's per-device override).
    below_advertised: bool,
    /// The transmitter's Dante device name (dstack #266).
    device: String,
}

fn rx_part(j: &mut Json, media: &Media) {
    let rings = media.rx.rings();
    let (flows, statuses): (Vec<Option<RxFlow>>, Vec<Option<SubStatus>>) = {
        let m = lock(&media.manager);
        (
            (0..MAX_FLOWS)
                .map(|s| {
                    m.flow(s).map(|f| {
                        let plan = m.rx_plan(s);
                        RxFlow {
                            granted: f.handle.is_some(),
                            multicast: f.group.is_some(),
                            fpp: f.fpp,
                            channels: f.channels.len(),
                            latency: latency_samples(f.latency_ns, media.tx.cfg.sample_rate),
                            playout: plan.latency,
                            delay: plan.delay,
                            late_after: u64::from(plan.latency) + u64::from(plan.block),
                            advertised: latency_samples(f.advertised_ns, media.tx.cfg.sample_rate),
                            below_advertised: f.below_advertised,
                            device: f.device,
                        }
                    })
                })
                .collect(),
            (0..rings.len()).map(|rx| m.status(rx)).collect(),
        )
    };
    let drops = media.rx.socket_drops();
    j.open(Some("rx"), '[');
    for (slot, flow) in flows.into_iter().enumerate() {
        let lane = media.rx.lane(slot);
        if flow.is_none() && lane == RxCounts::default() {
            continue;
        }
        let named = flow.is_some();
        let f = flow.unwrap_or_default();
        j.open(None, '{');
        j.num("flow", slot + 1);
        if named {
            j.text("device", &f.device);
        }
        if let Some(source) = media.rx.source(slot) {
            j.text("source", &source.to_string());
        }
        j.flag("granted", f.granted);
        j.flag("multicast", f.multicast);
        j.num("fpp", f.fpp);
        j.num("channels", f.channels);
        j.num("latency", f.latency);
        j.num("playout", f.playout);
        j.num("delay", f.delay);
        j.num("late_after", f.late_after);
        j.num("advertised", f.advertised);
        j.flag("below_advertised", f.below_advertised);
        j.num("packets", lane.packets);
        j.num("refused", lane.refused);
        j.num("stray", lane.stray);
        j.num("out_of_order", lane.out_of_order);
        j.num("gaps", lane.gaps);
        j.num("gap_frames", lane.gap_frames);
        if let Some(drops) = drops {
            j.num("socket_drops", drops[slot]);
        }
        j.num("late", lane.late);
        j.num("late_at_read", lane.late_at_read);
        if media.rx.stamps() {
            j.num("wire_late", lane.wire_late);
            j.num("wake_delay_us", media.rx.last_wake_delay_us(slot));
            j.num("wake_delay_peak_us", lane.wake_delay_peak_us);
            if let Some(n) = media.rx.last_network_latency(slot) {
                j.num("network_latency_min_us", n.min_us);
                j.num("network_latency_p99_us", n.p99_us);
                j.num("network_latency_max_us", n.max_us);
            }
        }
        j.num("unplaced", lane.unplaced);
        j.num("lateness", media.rx.last_lateness(slot));
        j.num("lateness_peak", lane.lateness_peak);
        if let Some(ns) = media.rx.last_min_lateness_ns(slot) {
            j.num("lateness_min_ns", ns);
        }
        j.num("keepalives", lane.keepalives);
        j.num("keepalive_errors", lane.keepalive_errors);
        j.num("recv_errors", lane.recv_errors);
        j.num("joins", lane.joins);
        j.num("join_errors", lane.join_errors);
        j.close('}');
    }
    j.close(']');
    j.num("rx_poll_errors", media.rx.poll_errors());
    j.open(Some("channels"), '[');
    for (rx, (ring, status)) in rings.iter().zip(statuses).enumerate() {
        let c = ring.counters();
        if status.is_none() && c == Counters::default() {
            continue;
        }
        j.open(None, '{');
        j.num("rx", rx + 1);
        j.num("status", status.map_or(0, SubStatus::code));
        j.num("written", c.written);
        j.num("holes", c.holes);
        j.num("late", c.late);
        j.num("duplicate", c.duplicate);
        j.num("early", c.early);
        j.close('}');
    }
    j.close(']');
}

fn sources_part(j: &mut Json, media: &Media) {
    j.open(Some("sources"), '[');
    for tx in 0..media.tx.cfg.tx.len() {
        let source = match media.tx.source(tx) {
            None | Some(Source::Silence) => continue,
            Some(s) => s,
        };
        j.open(None, '{');
        j.num("tx", tx + 1);
        j.word("source", source_word(source));
        match source {
            Source::Sine { hz, dbfs } => {
                j.num("hz", hz);
                j.num("dbfs", dbfs);
            }
            Source::Echo { rx, offset } => {
                j.num("rx", rx + 1);
                j.num("offset", offset);
            }
            Source::Silence | Source::Counter | Source::Engine => {}
        }
        j.close('}');
    }
    j.close(']');
}

fn checks_part(j: &mut Json, media: &Media) {
    let views = lock(&media.checks).views();
    j.open(Some("checks"), '[');
    for v in views {
        j.open(None, '{');
        j.num("rx", v.rx + 1);
        j.word("check", check_word(Some(v.kind)));
        if let CheckKind::Sine { hz } = v.kind {
            j.num("hz", hz);
        }
        j.num("skipped", v.skipped);
        match v.stats {
            CheckStats::Waiting => j.flag("waiting", true),
            CheckStats::Counter(s) => {
                j.num("checked", s.checked);
                j.num("blind", s.blind);
                j.num("mismatch", s.mismatch);
                j.num("mismatch_frames", s.mismatch_frames);
                j.num("dropout", s.dropout);
                j.num("dropout_frames", s.dropout_frames);
                j.num("slips", s.slips);
                j.num("last_slip", s.last_slip);
                j.opt("lag", s.lag);
                j.flag("no_return", s.no_return);
            }
            CheckStats::Sine(s) => {
                j.num("checked", s.checked);
                j.num("blind", s.blind);
                j.num("discontinuities", s.discontinuities);
                j.num("discontinuity_frames", s.discontinuity_frames);
            }
        }
        j.close('}');
    }
    j.close(']');
}

fn engine_part(j: &mut Json, media: &Media) {
    let Some(v) = media.engine() else { return };
    j.open(Some("engine"), '{');
    j.flag("media", v.media);
    j.num("offset", v.offset);
    j.num("playout", v.playout);
    j.num("blocks", v.blocks);
    j.num("late", v.late);
    j.num("missed", v.missed);
    j.num("overruns", v.overruns);
    j.num("rebootstraps", v.rebootstraps);
    j.num("max_process_us", v.max_process_us);
    j.close('}');
}

#[cfg(test)]
mod tests;
