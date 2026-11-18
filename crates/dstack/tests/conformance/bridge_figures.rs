// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! A strict, independent reader of dstack's bridge-figures reply (dstack
//! #391, version 2 since #419), for tests only, written from the layout
//! tables in the issues' designs and `info::bridge`'s doc, never from its
//! writer or reader, and
//! calling neither (dstack #349: a test that reads our bytes with our own
//! parser cannot catch a mistake both share). No hardware sends this
//! packet, so there is no capture: the table is the contract dcontroller
//! reads.
//!
//! The reply: a 32-byte header (start code 0xffff, the datagram's length,
//! a sequence, a process id, the device id, `Audinate`, opcode
//! `07 2a d5 b0 00 00 00 00`), then 192 bytes, big-endian (version 2; a
//! reply is always version 2, so this walker reads no version 1):
//!
//! | offset | width | key |
//! |---|---|---|
//! | 0 | 2 | version (2) |
//! | 2 | 1 | to_asio_cause (0 none, 1 bridge, 2 driver, 3 unknown) |
//! | 3 | 1 | from_asio_cause |
//! | 4 | 4 | sequence |
//! | 8 | 8 | run_start_unix_ms |
//! | 16 | 4 | to_asio_target_us |
//! | 20 | 4 | from_asio_target_us |
//! | 24 | 4 | to_asio_converter_delay_us |
//! | 28 | 4 | from_asio_converter_delay_us |
//! | 32 | 8 | to_asio_underruns |
//! | 40 | 8 | from_asio_underruns |
//! | 48 | 8 | late_callbacks |
//! | 56 + 8k | 8 | late_250us .. late_2000us (k = 0..7) |
//! | 120 | 4 | asio_to_dante_us (0 = not known) |
//! | 124 | 4 | dante_to_asio_us (0 = not known) |
//! | 128 | 1 | driver_len (0 to 63) |
//! | 129 | 63 | the driver's name: driver_len bytes of UTF-8, then zeros |
//!
//! Shared by the unit tests (`info/tests/bridge.rs`, in the mutation gate)
//! and the whole-device test (`tests/bridge_figures.rs`) through `#[path]`.

#![allow(dead_code)]

/// The header's size.
pub const HEADER: usize = 32;
/// The body's size.
pub const BODY: usize = 192;
/// Where the driver's name sits, and its room.
pub const DRIVER: (usize, usize) = (129, 63);

/// Every numeric field of the body, by key, as `(key, offset, width)`.
pub const FIELDS: [(&str, usize, usize); 23] = [
    ("version", 0, 2),
    ("to_asio_cause", 2, 1),
    ("from_asio_cause", 3, 1),
    ("sequence", 4, 4),
    ("run_start_unix_ms", 8, 8),
    ("to_asio_target_us", 16, 4),
    ("from_asio_target_us", 20, 4),
    ("to_asio_converter_delay_us", 24, 4),
    ("from_asio_converter_delay_us", 28, 4),
    ("to_asio_underruns", 32, 8),
    ("from_asio_underruns", 40, 8),
    ("late_callbacks", 48, 8),
    ("late_250us", 56, 8),
    ("late_500us", 64, 8),
    ("late_750us", 72, 8),
    ("late_1000us", 80, 8),
    ("late_1250us", 88, 8),
    ("late_1500us", 96, 8),
    ("late_1750us", 104, 8),
    ("late_2000us", 112, 8),
    ("asio_to_dante_us", 120, 4),
    ("dante_to_asio_us", 124, 4),
    ("driver_len", 128, 1),
];

/// A walked reply: the device id, each numeric field's value, by key, and
/// the driver's name (empty when not known).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub device_id: [u8; 8],
    pub fields: Vec<(&'static str, u64)>,
    pub driver: String,
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
/// [`BODY`] bytes, version 2, each cause 0 to 3, the fields and the
/// driver's room tiling the body with no gap and no overlap, the driver's
/// name at most 63 bytes of UTF-8 and zeros after it.
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
    if d[24..32] != [0x07, 0x2a, 0xd5, 0xb0, 0, 0, 0, 0] {
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
    let (at, room) = DRIVER;
    for c in &mut covered[at..at + room] {
        if *c {
            return Err(format!("the driver's room overlaps at {at}"));
        }
        *c = true;
    }
    if let Some(gap) = covered.iter().position(|c| !c) {
        return Err(format!("byte {gap} is no field's"));
    }
    let n = fields
        .iter()
        .find(|(k, _)| *k == "driver_len")
        .map_or(0, |&(_, v)| v as usize);
    if n > room {
        return Err(format!("driver_len {n}"));
    }
    let name = &body[at..at + n];
    if body[at + n..at + room].iter().any(|&b| b != 0) {
        return Err("bytes past the driver's name".into());
    }
    let driver = String::from_utf8(name.to_vec()).map_err(|e| format!("driver: {e}"))?;
    let r = Reply {
        device_id: d[8..16].try_into().expect("8 bytes"),
        fields,
        driver,
    };
    if r.get("version") != 2 {
        return Err(format!("version {}", r.get("version")));
    }
    for key in ["to_asio_cause", "from_asio_cause"] {
        if r.get(key) > 3 {
            return Err(format!("{key} {}", r.get(key)));
        }
    }
    Ok(r)
}
