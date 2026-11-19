// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! A strict, independent reader of dstack's clock-figures reply (dstack
//! #429), for tests only, written from the layout table in the issue's
//! design and `info::clock_figures`' doc, never from its writer or reader,
//! and calling neither (dstack #349). No hardware sends this packet: the
//! table is the contract dcontroller reads.
//!
//! The reply: a 32-byte header (start code 0xffff, the datagram's length,
//! a sequence, a process id, the device id, `Audinate`, opcode
//! `07 2a d5 b2 00 00 00 00`), then 66 bytes, big-endian, version 1:
//!
//! | offset | width | key |
//! |---|---|---|
//! | 0 | 2 | version (1) |
//! | 2 | 1 | source (0 none, 1 internal, 2 service, 3 system) |
//! | 3 | 1 | ready (0, 1) |
//! | 4 | 4 | sequence |
//! | 8 | 8 | follower_error_p90_ns (i64; i64::MIN = not known) |
//! | 16 | 8 | follower_offset_from_service_ns (i64; i64::MIN = not known) |
//! | 24 | 4 | follower_freq_ppb (i32) |
//! | 28 | 4 | follower_holdovers |
//! | 32 | 4 | service_freq_ppb (i32) |
//! | 36 | 4 | service_holdovers |
//! | 40 | 4 | service_reply_age_ms (u32::MAX = no reply) |
//! | 44 | 6 | follower_grandmaster |
//! | 50 | 6 | service_grandmaster |
//! | 56 | 1 | follower (0, 1) |
//! | 57 | 1 | follower_locked (0, 1) |
//! | 58 | 1 | follower_settled (0, 1) |
//! | 59 | 1 | follower_holdover (0, 1) |
//! | 60 | 1 | follower_waiting (0 to 5) |
//! | 61 | 1 | service_poll (0 to 6) |
//! | 62 | 1 | service_locked (0, 1) |
//! | 63 | 1 | service_settled (0, 1) |
//! | 64 | 1 | service_holdover (0, 1) |
//! | 65 | 1 | service_mode (any byte) |
//!
//! A part that is absent (`follower` 0, `service_poll` 0) has every one of
//! its fields 0. Shared by the unit tests (`info/tests/clock_figures.rs`,
//! in the mutation gate) and the whole-device test
//! (`tests/clock_figures.rs`) through `#[path]`.

#![allow(dead_code)]

/// The header's size.
pub const HEADER: usize = 32;
/// The body's size.
pub const BODY: usize = 66;

/// Every field of the body, by key, as `(key, offset, width)`.
pub const FIELDS: [(&str, usize, usize); 23] = [
    ("version", 0, 2),
    ("source", 2, 1),
    ("ready", 3, 1),
    ("sequence", 4, 4),
    ("follower_error_p90_ns", 8, 8),
    ("follower_offset_from_service_ns", 16, 8),
    ("follower_freq_ppb", 24, 4),
    ("follower_holdovers", 28, 4),
    ("service_freq_ppb", 32, 4),
    ("service_holdovers", 36, 4),
    ("service_reply_age_ms", 40, 4),
    ("follower_grandmaster", 44, 6),
    ("service_grandmaster", 50, 6),
    ("follower", 56, 1),
    ("follower_locked", 57, 1),
    ("follower_settled", 58, 1),
    ("follower_holdover", 59, 1),
    ("follower_waiting", 60, 1),
    ("service_poll", 61, 1),
    ("service_locked", 62, 1),
    ("service_settled", 63, 1),
    ("service_holdover", 64, 1),
    ("service_mode", 65, 1),
];

/// The flag fields: 0 or 1.
const FLAGS: [&str; 8] = [
    "ready",
    "follower",
    "follower_locked",
    "follower_settled",
    "follower_holdover",
    "service_locked",
    "service_settled",
    "service_holdover",
];

/// The code fields and their largest value.
const CODES: [(&str, u64); 3] = [("source", 3), ("follower_waiting", 5), ("service_poll", 6)];

/// A walked reply: the device id and each field's raw big-endian value
/// (a signed field as its two's complement), by key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub device_id: [u8; 8],
    pub fields: Vec<(&'static str, u64)>,
}

impl Reply {
    /// The value of `key`; panics for a key the table has not.
    pub fn get(&self, key: &str) -> u64 {
        self.fields
            .iter()
            .find(|(k, _)| *k == key)
            .map(|&(_, v)| v)
            .unwrap_or_else(|| panic!("no field {key}"))
    }
}

/// A big-endian value of `width` bytes, byte by byte.
fn value(b: &[u8]) -> u64 {
    let mut v = 0u64;
    for &x in b {
        v = v * 256 + u64::from(x);
    }
    v
}

/// Walks `d` strictly: the header as the table says, the body exactly
/// [`BODY`] bytes, version 1, the fields tiling the body with no gap and
/// no overlap, each flag 0 or 1, each code within its table, and an
/// absent part's fields all 0.
pub fn reply(d: &[u8]) -> Result<Reply, String> {
    if d.len() < HEADER {
        return Err(format!("{} bytes: shorter than the header", d.len()));
    }
    if d[0] != 0xff || d[1] != 0xff {
        return Err(format!("start code {:02x}{:02x}", d[0], d[1]));
    }
    let stated = usize::from(d[2]) * 256 + usize::from(d[3]);
    if stated != d.len() {
        return Err(format!("length {stated}, datagram {}", d.len()));
    }
    if &d[16..24] != b"Audinate" {
        return Err("vendor".into());
    }
    if d[24..32] != [0x07, 0x2a, 0xd5, 0xb2, 0, 0, 0, 0] {
        return Err(format!("opcode {:02x?}", &d[24..32]));
    }
    let body = &d[HEADER..];
    if body.len() != BODY {
        return Err(format!("body {} bytes, not {BODY}", body.len()));
    }
    let mut covered = [false; BODY];
    let mut fields = Vec::new();
    for &(key, at, width) in &FIELDS {
        for c in &mut covered[at..at + width] {
            if *c {
                return Err(format!("{key} overlaps at {at}"));
            }
            *c = true;
        }
        fields.push((key, value(&body[at..at + width])));
    }
    if let Some(gap) = covered.iter().position(|c| !c) {
        return Err(format!("byte {gap} is no field's"));
    }
    let r = Reply {
        device_id: d[8..16].try_into().expect("8 bytes"),
        fields,
    };
    if r.get("version") != 1 {
        return Err(format!("version {}", r.get("version")));
    }
    for key in FLAGS {
        if r.get(key) > 1 {
            return Err(format!("{key} {}", r.get(key)));
        }
    }
    for (key, max) in CODES {
        if r.get(key) > max {
            return Err(format!("{key} {}", r.get(key)));
        }
    }
    for (part, present) in [("follower_", "follower"), ("service_", "service_poll")] {
        if r.get(present) != 0 {
            continue;
        }
        for &(key, _, _) in &FIELDS {
            if key.starts_with(part) && r.get(key) != 0 {
                return Err(format!("{key} set while {present} is 0"));
            }
        }
    }
    Ok(r)
}
