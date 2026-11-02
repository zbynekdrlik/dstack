// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! A receiver's packet latency, as Dante Controller's Latency tab shows it
//! (dstack #47). Every Dante receiver says it in its heartbeat, once a
//! second, to [`HEARTBEAT_GROUP`]:[`HEARTBEAT_PORT`]; nothing asks, and
//! nothing resets it:
//!
//! - record 0x8003: the rate (`u32`), then for each RX flow slot the
//!   largest lateness of its packets in that second, samples (`u32`);
//! - record 0x8004: for each slot the late packets since the device
//!   started (`u32`), a count that starts again only with the device.
//!
//! Each record: `length, type, 4, content length, sequence, 0`, then `n, 0,
//! offset, 0` (`u16`s): `n` values at `offset` from the record's start. The
//! hardware receivers on the site (Yamaha cards, a Tascam) list 64 slots,
//! our own device [`crate::arc::MAX_FLOWS`] (both records since v0.4.20). No device sends a histogram:
//! Dante Controller bins the per-second peaks against each flow's latency
//! (ARC 0x3200, [`crate::arc::control::rx_flows`]), slot `n` at index
//! `n - 1`. [`listen`] gathers the records of the devices at one address
//! for a while, apart by the header's device id (bytes 8-15: several of
//! our devices share an address on their own ports, dasio 1.11);
//! [`histogram`] bins the peaks.

use std::collections::BTreeMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

use crate::info::{HEADER_LEN, HEARTBEAT_GROUP, HEARTBEAT_PORT, START_HEARTBEAT, op};

/// The per-second lateness record.
pub const LATENESS: u16 = 0x8003;
/// The cumulative late packets record.
pub const LATE: u16 = 0x8004;

/// One heartbeat's latency records.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Beat {
    /// The sender's device id (header bytes 8-15).
    pub device_id: [u8; 8],
    /// 0x8003: the rate and each slot's largest lateness that second,
    /// samples.
    pub lateness: Option<(u32, Vec<u32>)>,
    /// 0x8004: each slot's late packets since the device started.
    pub late: Option<Vec<u32>>,
}

/// A big-endian `u16` at `at`.
fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        d.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

/// A big-endian `u32` at `at`.
fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        d.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

/// The `n` values a record `r` lists at its offset; `None` when they pass
/// the record.
fn values(r: &[u8]) -> Option<Vec<u32>> {
    let n = usize::from(u16_at(r, 12)?);
    let at = usize::from(u16_at(r, 16)?);
    (0..n).map(|k| u32_at(r, at.checked_add(4 * k)?)).collect()
}

/// The latency records of the heartbeat `datagram`; `None` for anything
/// that is not a heartbeat (start code, opcode, shorter than the header).
/// A record that does not read is left out; the walk stops at a length
/// under 4 or past the datagram.
pub fn parse(datagram: &[u8]) -> Option<Beat> {
    let head = datagram.get(..HEADER_LEN)?;
    if u16_at(head, 0)? != START_HEARTBEAT || head[24..32] != op::HEARTBEAT {
        return None;
    }
    let mut beat = Beat {
        device_id: head[8..16].try_into().ok()?,
        ..Beat::default()
    };
    let mut at = HEADER_LEN;
    while let (Some(len), Some(kind)) = (u16_at(datagram, at), u16_at(datagram, at + 2)) {
        let len = usize::from(len);
        let Some(r) = datagram.get(at..at + len).filter(|_| len >= 4) else {
            break;
        };
        match kind {
            LATENESS => beat.lateness = u32_at(r, 20).zip(values(r)),
            LATE => beat.late = values(r),
            _ => {}
        }
        at += len;
    }
    Some(beat)
}

/// One slot's latency over a listen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotLatency {
    /// The slot, 1 up (ARC 0x3200's flow).
    pub slot: u16,
    /// Each heartbeat's largest lateness, µs, in order (a slot without a
    /// flow reads 0).
    pub peaks_us: Vec<u32>,
    /// Late packets during the listen.
    pub late: u64,
    /// Late packets since the device started, at the last heartbeat.
    pub late_total: u32,
}

/// What a listen heard from one device.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Latency {
    /// The device's id (its heartbeats' header bytes 8-15).
    pub device_id: [u8; 8],
    /// Heartbeats heard.
    pub beats: u32,
    /// The rate the last lateness record gave.
    pub rate: u32,
    /// Every slot either record listed, by slot.
    pub slots: Vec<SlotLatency>,
}

/// Gathers one device's heartbeats.
#[derive(Debug, Default)]
pub struct Gather {
    device_id: [u8; 8],
    beats: u32,
    rate: u32,
    peaks: BTreeMap<u16, Vec<u32>>,
    late: BTreeMap<u16, (u32, u64)>,
}

impl Gather {
    /// Adds one heartbeat's records.
    pub fn add(&mut self, beat: &Beat) {
        self.device_id = beat.device_id;
        self.beats += 1;
        if let Some((rate, lateness)) = &beat.lateness {
            self.rate = *rate;
            for (slot, &samples) in (1u16..).zip(lateness) {
                let us = u64::from(samples) * 1_000_000 / u64::from((*rate).max(1));
                self.peaks
                    .entry(slot)
                    .or_default()
                    .push(u32::try_from(us).unwrap_or(u32::MAX));
            }
        }
        for (slot, &total) in (1u16..).zip(beat.late.iter().flatten()) {
            self.late
                .entry(slot)
                .and_modify(|(last, late)| {
                    // A count that went down started again (the device).
                    *late += u64::from(if total >= *last { total - *last } else { total });
                    *last = total;
                })
                .or_insert((total, 0));
        }
    }

    /// What was gathered.
    pub fn result(&self) -> Latency {
        let mut slots: Vec<u16> = self.peaks.keys().chain(self.late.keys()).copied().collect();
        slots.sort_unstable();
        slots.dedup();
        Latency {
            device_id: self.device_id,
            beats: self.beats,
            rate: self.rate,
            slots: slots
                .into_iter()
                .map(|slot| {
                    let (late_total, late) = self.late.get(&slot).copied().unwrap_or((0, 0));
                    SlotLatency {
                        slot,
                        peaks_us: self.peaks.get(&slot).cloned().unwrap_or_default(),
                        late,
                        late_total,
                    }
                })
                .collect(),
        }
    }
}

/// Gathers the heartbeats of several devices, apart by device id.
#[derive(Debug, Default)]
pub struct Heard {
    devices: BTreeMap<[u8; 8], Gather>,
}

impl Heard {
    /// Adds one heartbeat to its device's gather.
    pub fn add(&mut self, beat: &Beat) {
        self.devices.entry(beat.device_id).or_default().add(beat);
    }

    /// Each device's gather, by device id.
    pub fn result(&self) -> Vec<Latency> {
        self.devices.values().map(Gather::result).collect()
    }
}

/// The peaks `peaks_us` binned by `edges_us` (ascending): bin `k` counts
/// the peaks at most `edges_us[k]` and over the edge before it; the last
/// bin, the peaks over the last edge.
pub fn histogram(peaks_us: &[u32], edges_us: &[u32]) -> Vec<u64> {
    let mut bins = vec![0u64; edges_us.len() + 1];
    for &p in peaks_us {
        bins[edges_us.partition_point(|&e| e < p)] += 1;
    }
    bins
}

/// Listens on `our` address's network for `window` to the heartbeats from
/// the address `device`, gathering each device's latency records there
/// (one entry per device id, in id order). The socket shares the heartbeat
/// port with any other listener on the host (our own device sends there,
/// it does not listen). An error only when the socket cannot be made or
/// joined; an address that sends nothing gives no entry.
pub fn listen(our: Ipv4Addr, device: Ipv4Addr, window: Duration) -> io::Result<Vec<Latency>> {
    let sock = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    sock.set_reuse_address(true)?;
    #[cfg(unix)]
    sock.set_reuse_port(true)?;
    sock.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, HEARTBEAT_PORT).into())?;
    sock.join_multicast_v4(&HEARTBEAT_GROUP, &our)?;
    let sock: UdpSocket = sock.into();
    let mut heard = Heard::default();
    let end = Instant::now() + window;
    let mut buf = [0u8; 2048];
    while let Some(left) = end
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
    {
        sock.set_read_timeout(Some(left))?;
        match sock.recv_from(&mut buf) {
            Ok((n, SocketAddr::V4(from))) if *from.ip() == device => {
                if let Some(beat) = parse(&buf[..n]) {
                    heard.add(&beat);
                }
            }
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(heard.result())
}

#[cfg(test)]
mod tests;
