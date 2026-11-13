// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The control side's reads of a flow slot's minute (dstack #234): the
//! wake-delay, write-delay and network-latency windows the RX minute watch
//! takes once a minute (`media/late.rs`), and the ring counts of the RX
//! channels a slot feeds.

use std::sync::atomic::Ordering;

use super::{Counters, NetLatency, RxRoute, RxShared, WakeDelay};

impl RxShared {
    /// Ends flow slot `slot`'s minute (dstack #234; the RX minute watch,
    /// once a minute): its packets' wake delays and network latencies
    /// since the last call, each `None` for no stamped packet (always on
    /// Windows). The status keeps showing the wake percentiles taken
    /// ([`RxShared::last_wake_minute`]).
    pub fn take_minute(&self, slot: usize) -> (Option<WakeDelay>, Option<NetLatency>) {
        self.lanes.get(slot).map_or((None, None), |l| {
            (l.wake_minute.take(), l.network_minute.take())
        })
    }

    /// Ends flow slot `slot`'s minute of write delays (dstack #234; the RX
    /// minute watch, beside [`RxShared::take_minute`]): each stamped,
    /// placed packet's kernel receive time to the end of its ring writes,
    /// since the last call; `None` for no such packet (always on Windows).
    /// The status keeps showing the minute taken
    /// ([`RxShared::last_write_minute`]).
    pub fn take_write_minute(&self, slot: usize) -> Option<WakeDelay> {
        self.lanes.get(slot)?.write_minute.take()
    }

    /// The 99.9th percentile and the largest of flow slot `slot`'s write
    /// delays in the minute [`RxShared::take_write_minute`] took last, µs;
    /// `None` before the first take or for a minute without a stamped
    /// packet.
    pub fn last_write_minute(&self, slot: usize) -> Option<[u64; 2]> {
        let w = &self.lanes.get(slot)?.write_minute;
        Some([w.last()?[2], w.last_max_us()?])
    }

    /// The ring counts of the RX channels routed to flow slot `slot` now,
    /// summed (dstack #234: its `holes`, `late` and `start_holes` for the
    /// status and the RX minute watch). A channel routed elsewhere since
    /// takes its counts along.
    pub fn flow_rings(&self, slot: usize) -> Counters {
        let mut sum = Counters::default();
        for (rx, ring) in self.rings.iter().enumerate() {
            if self.route(rx).is_some_and(|r| r.flow == slot) {
                sum += ring.counters();
            }
        }
        sum
    }

    /// Where RX channel `rx` (from 0) is routed now (its route atomic,
    /// `Relaxed`); `None` for no route or no such channel.
    pub fn route(&self, rx: usize) -> Option<RxRoute> {
        RxRoute::from_word(self.routes.get(rx)?.load(Ordering::Relaxed))
    }

    /// The 50th, 99th and 99.9th percentile of flow slot `slot`'s wake
    /// delays in the minute [`RxShared::take_minute`] took last (the first
    /// from the start, shorter), µs; `None` before the first take or for a
    /// minute without a stamped packet.
    /// The status reads it and takes nothing.
    pub fn last_wake_minute(&self, slot: usize) -> Option<[u64; 3]> {
        self.lanes.get(slot)?.wake_minute.last()
    }
}
