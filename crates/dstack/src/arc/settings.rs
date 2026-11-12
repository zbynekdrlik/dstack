// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.
// Protocol facts (byte layouts, opcodes, behaviour) checked against
// inferno (https://github.com/teodly/inferno) as a reference; the code is
// our own.

//! The settings server (dstack #216, #231): 0x1100, the keyed settings
//! read ([`settings_body`]), and 0x1102, the settings key table
//! ([`key_table`]), as the hardware answers them. The dispatcher
//! ([`super::handle`]) picks the request; this module only builds the
//! reply.

use super::control;
use crate::device::DeviceConfig;
use crate::wire::{Body, HEADER_LEN, MAX_DATAGRAM, read_u16};

/// The latency default the settings read states: Dante's 1 ms, as the
/// hardware states it.
const LATENCY_DEFAULT_NS: u64 = 1_000_000;

/// The most keys one settings reply holds: each takes a pair and a value
/// (8 bytes), and the count is one byte.
const SETTINGS_MAX_KEYS: usize = 128;
const _: () = assert!(HEADER_LEN + 2 + 8 * SETTINGS_MAX_KEYS <= MAX_DATAGRAM);

/// Our value for a settings key (0x1100): the sample rate, no pull-up,
/// our latency as configured and in effect, a fixed default
/// ([`LATENCY_DEFAULT_NS`]) and the range
/// Dante Controller may set ([`DeviceConfig::latency_range`]). Dante
/// Controller greys its latency control and shows 0.0 ms without the
/// default and the range (mbc, 2.11.2026); every hardware device answers
/// them.
fn setting(key: u16, cfg: &DeviceConfig) -> Option<u32> {
    use control::key;
    let ns = |v: u64| u32::try_from(v).unwrap_or(u32::MAX);
    let (min, max) = cfg.latency_range();
    match key {
        key::SAMPLE_RATE => Some(cfg.sample_rate),
        key::PULLUP => Some(0),
        key::LATENCY_CONFIGURED | key::LATENCY_ACTIVE => Some(ns(cfg.latency_ns)),
        // A fixed default (Dante's 1 ms, within our range): it never moves
        // with a write, which netaudio's write check would read as another
        // setting changed.
        key::LATENCY_DEFAULT => Some(ns(LATENCY_DEFAULT_NS.clamp(min, max))),
        key::LATENCY_MIN => Some(ns(min)),
        key::LATENCY_MAX => Some(ns(max)),
        _ => None,
    }
}

/// The 0x1100 reply to a keyed request (`body`: a `u16` count, then the
/// keys), as the hardware answers it: the count twice (a byte each), then
/// a pair per key in the asked order, `key, offset` for a key we hold (the
/// offset, from the packet's start, of its `u32` after the pairs) and
/// `0, key` for one we do not, then the values. `None` when the count
/// passes the body or [`SETTINGS_MAX_KEYS`].
pub(super) fn settings_body(body: &[u8], cfg: &DeviceConfig) -> Option<Vec<u8>> {
    let n = usize::from(read_u16(body, 0)?);
    if n > SETTINGS_MAX_KEYS {
        return None;
    }
    let keys: Vec<u16> = (0..n)
        .map(|k| read_u16(body, 2 + 2 * k))
        .collect::<Option<_>>()?;
    let mut pairs = Vec::new();
    let mut values = Vec::new();
    pairs.extend_from_slice(&[n as u8, n as u8]);
    let values_at = HEADER_LEN + 2 + 4 * n;
    for k in keys {
        match setting(k, cfg) {
            Some(v) => {
                let at = (values_at + values.len()) as u16;
                pairs.extend_from_slice(&k.to_be_bytes());
                pairs.extend_from_slice(&at.to_be_bytes());
                values.extend_from_slice(&v.to_be_bytes());
            }
            None => {
                pairs.extend_from_slice(&[0, 0]);
                pairs.extend_from_slice(&k.to_be_bytes());
            }
        }
    }
    pairs.extend_from_slice(&values);
    Some(pairs)
}

/// A key Dante Controller may write, as the hardware marks it in its
/// key table (0x1102).
const KEY_READ_WRITE: u16 = 0x0003;
/// A key it may only read.
const KEY_READ_ONLY: u16 = 0x0001;

/// 0x1102, the settings key table (dstack #216): the count (`u16`), then
/// `key, access` (`u16` each) for each key our settings read holds
/// ([`control::SETTING_KEYS`] that [`setting`] answers), in that order.
/// The latency as configured and in effect (0x8205, 0x8301, the keys
/// netaudio's write sets) is writable when a write can move it
/// ([`latency_writable`]); every other key is read only. A real card
/// answers its 31 keys so (`tests/fixtures/arc_settings_card.json`);
/// with inferno's zeros Dante Controller marked every setting read only,
/// greyed out the rename and never sent the latency write.
pub(super) fn key_table(cfg: &DeviceConfig) -> Vec<u8> {
    use control::key;
    let keys: Vec<u16> = control::SETTING_KEYS
        .into_iter()
        .filter(|&k| setting(k, cfg).is_some())
        .collect();
    let mut out = Body::new();
    out.u16(keys.len() as u16);
    for k in keys {
        let access = match k {
            key::LATENCY_CONFIGURED | key::LATENCY_ACTIVE if latency_writable(cfg) => {
                KEY_READ_WRITE
            }
            _ => KEY_READ_ONLY,
        };
        out.u16(k);
        out.u16(access);
    }
    out.into_vec()
}

/// Whether a latency write (0x1101) can move the device's latency: the
/// range it states ([`DeviceConfig::latency_range`]) holds another value
/// than its own.
fn latency_writable(cfg: &DeviceConfig) -> bool {
    let (min, max) = cfg.latency_range();
    min < max
}
