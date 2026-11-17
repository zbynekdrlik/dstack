// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! A strict, independent reader of the length-walked packets a Dante
//! device sends on the info and heartbeat groups (dstack #349), for tests
//! only. It is written from hardware captures
//! (`tests/fixtures/heartbeat_hardware.json`: an UltimoX2 with 0+2 and
//! 2+2 channels, a Brooklyn card, a Yamaha card), never from dstack's own
//! reader or writer, and it calls neither: a test that reads our bytes
//! with it cannot share a writer's mistake (dstack #345: the peaks
//! record's length left out its padding, and our own `latency::parse`,
//! which shared that assumption, read it back fine).
//!
//! Shared by the unit tests (`info/tests/conformance.rs`, which the
//! mutation gate runs) and the whole-device test
//! (`tests/heartbeat_walk.rs`) through `#[path]`.
//!
//! The heartbeat as the hardware writes it: a 32-byte header (start code
//! 0xfffe, the datagram's length, a sequence, a process id, the device
//! id, `Audinate`, opcode `00 08 00 01 10 00 00 00`), then records, each
//! a 12-byte head of `u16`s (the record's length, its type, 4, the
//! content's length, a sequence, 0) and its content. Every record's length
//! is a multiple of 4, its content's length is the record's less 12
//! (0x8000 alone states 4, on both devices that send it), and the walk by
//! the lengths ends exactly at the datagram's end. Tabled
//! records state their values' count and offset from the record's start:
//!
//! - 0x8002, the peaks: `tx, 0, rx, 0, 24, 0`, then a byte a channel (TX
//!   then RX), zeros to the next multiple of 4, which the length counts;
//! - 0x8003, each flow slot's lateness: `n, 0, 24, 0`, the rate (`u32`),
//!   then `n` `u32`s, nothing after;
//! - 0x8004, each flow slot's late count: `n, 0, 20, 0`, then `n` `u32`s.
//!
//! 0x8000 and 0x8001 (clock records) are walked, not read. Each type
//! comes at most once; the order differs between devices; 0x8003 and
//! 0x8004 list the same number of slots.
//!
//! The info packets (start code 0xffff) are walked by the header alone,
//! the channel-change notification also by its mask's length, as an
//! UltimoX2 and a Yamaha card sent it (`channel_changes` in the fixture,
//! [`info`]).

// Each user (a unit test module, an integration test) takes what it needs.
#![allow(dead_code)]

/// The header's length.
pub const HEADER: usize = 32;
/// A record head's length.
pub const RECORD_HEAD: usize = 12;
/// The heartbeat's start code.
pub const HEARTBEAT_START: u16 = 0xfffe;
/// The info packets' start code.
pub const INFO_START: u16 = 0xffff;
/// The heartbeat's opcode.
pub const HEARTBEAT_OP: [u8; 8] = [0x00, 0x08, 0x00, 0x01, 0x10, 0x00, 0x00, 0x00];
/// The vendor field every device writes.
pub const VENDOR: [u8; 8] = *b"Audinate";
/// The record types the hardware sends.
pub const KNOWN: [u16; 5] = [0x8000, 0x8001, 0x8002, 0x8003, 0x8004];

/// What a strict walk read from one heartbeat.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Heartbeat {
    /// Each record's type and length, in order.
    pub records: Vec<(u16, usize)>,
    /// 0x8002: the TX and RX peak bytes.
    pub peaks: Option<(Vec<u8>, Vec<u8>)>,
    /// 0x8003: the rate and each slot's lateness.
    pub lateness: Option<(u32, Vec<u32>)>,
    /// 0x8004: each slot's late count.
    pub late: Option<Vec<u32>>,
}

fn word(d: &[u8], at: usize) -> Result<u16, String> {
    d.get(at..at + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .ok_or_else(|| format!("no u16 at {at} of {}", d.len()))
}

fn long(d: &[u8], at: usize) -> Result<u32, String> {
    d.get(at..at + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| format!("no u32 at {at} of {}", d.len()))
}

/// The common header: `start`, the length field equal to the datagram's
/// size, the vendor. Returns the opcode.
fn header(d: &[u8], start: u16) -> Result<[u8; 8], String> {
    if d.len() < HEADER {
        return Err(format!("{} bytes: shorter than the header", d.len()));
    }
    if word(d, 0)? != start {
        return Err(format!("start code {:04x}, not {start:04x}", word(d, 0)?));
    }
    let stated = usize::from(word(d, 2)?);
    if stated != d.len() {
        return Err(format!(
            "the length field says {stated}, the datagram is {}",
            d.len()
        ));
    }
    if d[16..24] != VENDOR {
        return Err(format!("vendor {:02x?}", &d[16..24]));
    }
    Ok(d[24..32].try_into().expect("8 bytes"))
}

/// A tabled record's `n, 0, offset, 0` at its content's start; the
/// offset must be `offset`. Returns `n`.
fn table(r: &[u8], at: usize, kind: u16, offset: usize) -> Result<usize, String> {
    let n = usize::from(word(r, 12)?);
    let (zero_a, off, zero_b) = (word(r, 14)?, usize::from(word(r, 16)?), word(r, 18)?);
    if (zero_a, zero_b) != (0, 0) || off != offset {
        return Err(format!(
            "record {kind:04x} at {at}: table head {n}, {zero_a}, {off}, {zero_b} (offset {offset} wanted)"
        ));
    }
    Ok(n)
}

/// `n` `u32`s from `from`, which must end exactly at the record's end.
fn values(r: &[u8], at: usize, kind: u16, from: usize, n: usize) -> Result<Vec<u32>, String> {
    if from + 4 * n != r.len() {
        return Err(format!(
            "record {kind:04x} at {at}: {n} values from {from} end at {}, the record at {}",
            from + 4 * n,
            r.len()
        ));
    }
    (0..n).map(|k| long(r, from + 4 * k)).collect()
}

/// The peaks record `r` (at `at` in the datagram).
fn peaks(r: &[u8], at: usize) -> Result<(Vec<u8>, Vec<u8>), String> {
    let (tx, rx) = (usize::from(word(r, 12)?), usize::from(word(r, 16)?));
    let (z1, z2, off, z3) = (
        word(r, 14)?,
        word(r, 18)?,
        usize::from(word(r, 20)?),
        word(r, 22)?,
    );
    if (z1, z2, z3, off) != (0, 0, 0, 24) {
        return Err(format!(
            "record 8002 at {at}: table head {tx}, {z1}, {rx}, {z2}, {off}, {z3}"
        ));
    }
    let end = off + tx + rx;
    let padded = end.div_ceil(4) * 4;
    if r.len() != padded {
        return Err(format!(
            "record 8002 at {at}: {tx} TX and {rx} RX peaks end at {end}, so the record is {padded} long, not {}",
            r.len()
        ));
    }
    if r[end..].iter().any(|&b| b != 0) {
        return Err(format!("record 8002 at {at}: padding not zero"));
    }
    Ok((r[off..off + tx].to_vec(), r[off + tx..end].to_vec()))
}

/// Walks the heartbeat `d` strictly; the first rule it breaks is the
/// error.
pub fn heartbeat(d: &[u8]) -> Result<Heartbeat, String> {
    let op = header(d, HEARTBEAT_START)?;
    if op != HEARTBEAT_OP {
        return Err(format!("opcode {op:02x?}"));
    }
    let mut beat = Heartbeat::default();
    let mut at = HEADER;
    while at < d.len() {
        if d.len() - at < RECORD_HEAD {
            return Err(format!(
                "{} bytes left at {at}: no room for a record head",
                d.len() - at
            ));
        }
        let (len, kind) = (usize::from(word(d, at)?), word(d, at + 2)?);
        let (four, content, last) = (
            word(d, at + 4)?,
            usize::from(word(d, at + 6)?),
            word(d, at + 10)?,
        );
        if len < RECORD_HEAD || !len.is_multiple_of(4) {
            return Err(format!(
                "record {kind:04x} at {at}: length {len}, not a multiple of 4 of at least 12"
            ));
        }
        if at + len > d.len() {
            return Err(format!(
                "record {kind:04x} at {at}: length {len} passes the datagram's end {}",
                d.len()
            ));
        }
        // 0x8000 states 4 there on both devices that send it (a record of
        // 36 and of 52 bytes), every other record its content's length.
        let stated = if kind == 0x8000 { 4 } else { len - RECORD_HEAD };
        if content != stated || content > len - RECORD_HEAD {
            return Err(format!(
                "record {kind:04x} at {at}: content length {content} for a record of {len}"
            ));
        }
        if (four, last) != (4, 0) {
            return Err(format!(
                "record {kind:04x} at {at}: head words {four} and {last}, not 4 and 0"
            ));
        }
        if !KNOWN.contains(&kind) {
            return Err(format!("record {kind:04x} at {at}: a type no device sends"));
        }
        if beat.records.iter().any(|&(k, _)| k == kind) {
            return Err(format!("record {kind:04x} at {at}: a second one"));
        }
        let r = &d[at..at + len];
        match kind {
            0x8002 => beat.peaks = Some(peaks(r, at)?),
            0x8003 => {
                let n = table(r, at, kind, 24)?;
                beat.lateness = Some((long(r, 20)?, values(r, at, kind, 24, n)?));
            }
            0x8004 => {
                let n = table(r, at, kind, 20)?;
                beat.late = Some(values(r, at, kind, 20, n)?);
            }
            _ => {}
        }
        beat.records.push((kind, len));
        at += len;
    }
    if let (Some((_, lateness)), Some(late)) = (&beat.lateness, &beat.late)
        && lateness.len() != late.len()
    {
        return Err(format!(
            "{} slots of lateness, {} of late counts",
            lateness.len(),
            late.len()
        ));
    }
    Ok(beat)
}

/// What a strict walk read from one info packet (start code 0xffff).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    pub opcode: [u8; 8],
    pub body: Vec<u8>,
}

/// The channel-change notification's opcode bytes 2-3 (`07 xx 01 02 00
/// 00 00 00`; the second byte differs per device: 0x38 on the UltimoX2,
/// 0x24 on the Yamaha card, a reader does not match it).
pub const CHANNEL_CHANGE: [u8; 2] = [0x01, 0x02];

/// Walks the info packet `d` strictly: the header (start code, the length
/// field equal to the datagram's size, the vendor) and a reply's opcode
/// `07 xx 0y yy 00 00 00 00`. The replies to 8700 hold no records a reader
/// steps through by length: the header's length is their one length to
/// walk, and their bodies are compared with the hardware's in the tests.
/// For a channel-change notification also its mask, as the hardware sends
/// it (captured 17.11.2026, dstack #349): a `u16` length n (at least 1),
/// then n mask bytes, ending exactly at the datagram's end (no padding to
/// 4: the hardware's are 35 and 36 bytes long), and the mask's last byte
/// names a channel (the hardware sends the shortest mask that holds the
/// highest channel: 1 byte for channel 7 of 128).
pub fn info(d: &[u8]) -> Result<Info, String> {
    let opcode = header(d, INFO_START)?;
    if opcode[0] != 0x07 || opcode[4..] != [0, 0, 0, 0] {
        return Err(format!("opcode {opcode:02x?}: not a reply's"));
    }
    let body = d[HEADER..].to_vec();
    if opcode[2..4] == CHANNEL_CHANGE {
        let n = usize::from(word(&body, 0)?);
        if n == 0 || 2 + n != body.len() {
            return Err(format!(
                "channel change: a mask of {n} bytes in a body of {}",
                body.len()
            ));
        }
        if body[body.len() - 1] == 0 {
            return Err(format!(
                "channel change: the mask's last byte of {n} names no channel"
            ));
        }
    }
    Ok(Info { opcode, body })
}

/// The channels (from 0) a channel-change body names.
pub fn changed_channels(body: &[u8]) -> Vec<usize> {
    body[2..]
        .iter()
        .enumerate()
        .flat_map(|(byte, &bits)| {
            (0..8)
                .filter(move |b| bits & (1 << b) != 0)
                .map(move |b| 8 * byte + b)
        })
        .collect()
}
