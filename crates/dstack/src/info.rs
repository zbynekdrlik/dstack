// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.
// Protocol facts (byte layouts, opcodes, behaviour) checked against
// inferno (https://github.com/teodly/inferno) as a reference; the code is
// our own.

//! Dante's info requests and heartbeat (#72 phase A): Dante Controller
//! asks a device it found over mDNS and CMC for its board info on UDP
//! [`REQUEST_PORT`] (8700), and goes on to ARC only once that is answered.
//!
//! The server listens at the device's address only (as inferno: no group
//! joined) and sends everything to multicast groups: the replies to
//! [`INFO_GROUP`]:[`INFO_PORT`] (224.0.0.231:8702), where Dante Controller
//! listens, and a heartbeat every second to
//! [`HEARTBEAT_GROUP`]:[`HEARTBEAT_PORT`] (224.0.0.233:8708). At start it
//! sends the board and the product info unasked, as inferno does.
//!
//! Every packet has a 32-byte big-endian header, then its body:
//!
//! | bytes | field | |
//! |---|---|---|
//! | 0–1 | start code | 0xffff info, 0xfffe heartbeat |
//! | 2–3 | length | the whole datagram, header included |
//! | 4–5 | sequence | the sender's own: ours counts from 1 over every packet it sends |
//! | 6–7 | process id | 0 |
//! | 8–15 | device id | ours: [`crate::cmc::device_id`]; Dante Controller's: its MAC, then 0 0 |
//! | 16–23 | vendor | `Audinate` (inferno's "Audinate Dante-compatible", cut to 8) |
//! | 24–31 | opcode | |
//!
//! | request opcode | reply opcode | reply body |
//! |---|---|---|
//! | `07 xx 00 61 00 00 00 00` | `07 2a 00 60 00 00 00 00` | board info, 200 bytes ([`board_info_body`]) |
//! | `07 xx 00 c1 00 00 00 00` | `07 2a 00 c0 00 00 00 00` | product info, 336 bytes ([`product_info_body`]) |
//! | `07 xx 00 13 00 00 00 xx` | `07 2a 00 11 00 00 00 00` | network info, 64 bytes ([`network_info_body`]) |
//! | `07 xx 00 77 00 00 00 xx` | `07 2a 00 78 00 00 00 00` | `00 00 00 03 00 00 00 00` |
//! | `07 xx 00 81 00 00 00 xx` | `07 2a 00 80 00 00 00 00` | our sample rate, 20 bytes ([`sample_rate_body`]); a write gets the same answer, unchanged |
//! | `07 xx 00 83 00 00 00 xx` | `07 2a 00 82 00 00 00 00` | our encoding as the one option, 20 bytes ([`encoding_body`]); a write gets the same answer |
//! | `07 xx 00 85 00 00 00 xx` | `07 2a 00 84 00 00 00 00` | pull-up none as the one option, 44 bytes ([`PULLUP_BODY`]) |
//! | `07 xx 00 21 00 00 00 xx` | `07 2a 00 20 00 00 00 00` | clock stats as a follower, 188 bytes ([`clock_stats_body`]): only once the media clock is ready and dsync names a grandmaster (DSYX bytes 42–47) |
//!
//! Any other opcode gets no reply and is logged by its opcode; a datagram
//! shorter than the header is dropped. As inferno, neither the start code
//! nor the length field of a request is checked, and the replies carry the
//! device's own sequence, not the request's.
//!
//! Unlike inferno, the replies are bounded ([`limit`]): each goes to a group
//! every Dante device on the network hears, so at most
//! [`limit::PER_SPAN`] replies of a kind go out in any second; the rest are
//! dropped and counted in one warning a minute.
//!
//! **The channel-change notification** (`07 2a 01 02 00 00 00 00`, to the
//! info group, unasked): a 16-bit length, then a bitmask of RX channels
//! (bit `n mod 8` of byte `n / 8` for channel `n` from 0, at least one
//! byte; inferno's `make_channel_change_notification`). Dante Controller
//! reads ARC 0x3000 again for them. It goes out when an RX channel is
//! renamed, or its subscription or its status changes ([`Changes`]);
//! changes are coalesced, so one notification names every channel that
//! changed since the last, and it is bounded as a kind of its own (at most
//! [`limit::PER_SPAN`] a second; a change held back goes with the next).
//! inferno also sends an empty one after an RX rename that renamed
//! nothing; we send none without a channel.
//!
//! **The heartbeat**'s opcode is `00 08 00 01 10 00 00 00`. While the
//! media clock is not ready its body is empty, as inferno's before its
//! clock has an offset. Once it is ready ([`Beat::records`]), the body
//! holds inferno's records ([`heartbeat_body`]), each `length, type, 4,
//! content length, the packet's sequence, 0`, then:
//!
//! - 0x8001: dsync's frequency adjustment, ppb (`i32`): what our clock
//!   follows, never a lock of our own;
//! - 0x8002: the peaks, one byte a channel (`−40·log10` of the peak's part
//!   of full scale, 0 to 255: [`peak_byte`]), TX channels then RX channels,
//!   padded to 4 bytes;
//! - 0x8003: our rate and each RX flow slot's largest lateness since the
//!   last beat, samples (0 for a slot without a granted flow);
//! - 0x8004: each RX flow slot's late packets since the device started
//!   (`n, 0, 20, 0`, then the counts), as the hardware receivers send it:
//!   Dante Controller's Latency tab shows it ([`crate::latency`]).
//!
//! Clock stats (`07 xx 00 21`) are answered as a follower of dsync's
//! grandmaster ([`Beat::clock`]); before that, no reply. Dante Controller
//! keeps a device's config half-loaded until they come.
//!
//! Logs name opcodes, sequences and ports, never an address or a MAC.
//!
//! This module builds every packet's bytes and holds no socket
//! ([`InfoBuilder`], per device, pure); the loops that receive and send
//! them are in `server` ([`serve_info`], [`serve_shared`]).

use std::collections::BTreeSet;
use std::net::Ipv4Addr;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::Notify;
use tracing::{debug, info};

use crate::cmc::device_id;
use crate::device::DeviceConfig;
use crate::media::rx::MAX_FLOWS;
use limit::Limit;

pub mod client;
pub mod limit;
mod server;
pub mod share;

pub use server::{
    Levels, Multicast, Sources, follower_of, serve_info, serve_info_with, serve_member,
    serve_shared,
};

/// The info request port (UDP), at the device's address.
pub const REQUEST_PORT: u16 = 8700;
/// The group and port the info replies go to (Dante's control and
/// monitoring group).
pub const INFO_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 231);
pub const INFO_PORT: u16 = 8702;
/// The group and port the heartbeat goes to.
pub const HEARTBEAT_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 233);
pub const HEARTBEAT_PORT: u16 = 8708;
/// The heartbeat's period (inferno's).
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(1);
/// The header's size.
pub const HEADER_LEN: usize = 32;
/// The start code of the info packets.
pub const START_INFO: u16 = 0xffff;
/// The start code of the heartbeat.
pub const START_HEARTBEAT: u16 = 0xfffe;
/// The vendor field.
pub const VENDOR: [u8; 8] = *b"Audinate";

/// The opcodes the device sends.
pub mod op {
    pub const BOARD_INFO: [u8; 8] = [0x07, 0x2a, 0x00, 0x60, 0x00, 0x00, 0x00, 0x00];
    pub const PRODUCT_INFO: [u8; 8] = [0x07, 0x2a, 0x00, 0xc0, 0x00, 0x00, 0x00, 0x00];
    pub const NETWORK_INFO: [u8; 8] = [0x07, 0x2a, 0x00, 0x11, 0x00, 0x00, 0x00, 0x00];
    pub const SAMPLE_RATE: [u8; 8] = [0x07, 0x2a, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00];
    /// The answer to 0x77, whose meaning inferno does not name.
    pub const ANSWER_0077: [u8; 8] = [0x07, 0x2a, 0x00, 0x78, 0x00, 0x00, 0x00, 0x00];
    /// RX channels changed (sent unasked).
    pub const CHANNEL_CHANGE: [u8; 8] = [0x07, 0x2a, 0x01, 0x02, 0x00, 0x00, 0x00, 0x00];
    pub const HEARTBEAT: [u8; 8] = [0x00, 0x08, 0x00, 0x01, 0x10, 0x00, 0x00, 0x00];
    /// The clock stats (a follower's, [`super::clock_stats_body`]).
    pub const CLOCK_STATS: [u8; 8] = [0x07, 0x2a, 0x00, 0x20, 0x00, 0x00, 0x00, 0x00];
    /// The encoding options ([`super::encoding_body`]), the answer to 0x83.
    pub const ENCODING: [u8; 8] = [0x07, 0x2a, 0x00, 0x82, 0x00, 0x00, 0x00, 0x00];
    /// The sample-rate pull-up options ([`super::PULLUP_BODY`]), the
    /// answer to 0x85.
    pub const PULLUP: [u8; 8] = [0x07, 0x2a, 0x00, 0x84, 0x00, 0x00, 0x00, 0x00];
}

/// The heartbeat records' types.
pub mod record {
    pub const FREQUENCY: u16 = 0x8001;
    pub const PEAKS: u16 = 0x8002;
    pub const RX_LATENESS: u16 = 0x8003;
    pub const RX_LATE: u16 = 0x8004;
}

pub const BOARD_INFO_LEN: usize = 200;
pub const PRODUCT_INFO_LEN: usize = 336;
pub const NETWORK_INFO_LEN: usize = 64;
pub const SAMPLE_RATE_LEN: usize = 20;
pub const ENCODING_LEN: usize = 20;
/// The pull-up answer: 0x28 bytes before its one option.
pub const PULLUP_LEN: usize = 44;
/// The sample-rate pull-up options, as a Tascam answers 0x85 (captured on
/// mbc, 2.11.2026) but with one option: `00 30` (the options at 0x28),
/// the count (1), none in effect (0), none pending, `00 02 00 00`, 1, then
/// zeros up to the options: none (0) alone.
pub const PULLUP_BODY: [u8; PULLUP_LEN] = {
    let mut c = [0u8; PULLUP_LEN];
    c[1] = 0x30;
    c[3] = 1;
    c[0x0d] = 2;
    c[0x13] = 1;
    c
};
/// inferno's answer to 0x77.
const ANSWER_0077_BODY: [u8; 8] = [0, 0, 0, 3, 0, 0, 0, 0];
/// The board info's firmware, hardware and boot versions (inferno's).
const FIRMWARE_VERSION: [u8; 4] = [4, 1, 0, 6];
const HARDWARE_VERSION: [u8; 4] = [4, 1, 0, 3];
const BOOT_VERSION: [u8; 4] = [1, 0, 0, 0];
/// The board info's feature flags, bytes 0x14-0x17 (decoded against Dante
/// Controller on mbc, 2.11.2026): in byte 0x16, 0x10 a manufacturer name
/// and 0x02 the pull-up; in byte 0x17, 0x10 the encoding and 0x08 the
/// sample rate. With them Dante Controller asks and shows each (0x85,
/// 0x83, 0x81). Left out: 0x17's 0x02 (reboot and factory reset), byte
/// 0x15 (clear config) and byte 0x14 (RTP, AES67).
const FEATURE_FLAGS: [u8; 4] = [0x00, 0x00, 0x12, 0x18];
/// The board info's byte 0xbb. At 0 the device receives info multicast
/// requests about once a second, so it is never 0.
const BYTE_BB: u8 = 0x1f;
/// A datagram read from the socket (inferno's MTU-sized buffer); only its
/// header is read.
const RECV_BUFFER: usize = 1500;

/// The interface's facts the network info carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetInfo {
    /// The interface's MAC address.
    pub mac: [u8; 6],
    /// The device's address on it.
    pub ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
    /// The default gateway in the interface's subnet, 0.0.0.0 for none.
    pub gateway: Ipv4Addr,
    /// The link's speed in Mbit/s, 0 when unknown.
    pub link_speed: u16,
}

/// What the info messages say about the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The device id, as CMC and the mDNS `id` entry name it.
    pub device_id: [u8; 8],
    /// The config's `manufacturer`.
    pub manufacturer: String,
    /// The config's `model`: the board name and the model name, as ARC
    /// 0x1003 and mDNS `router_info` name it.
    pub model: String,
    /// The product version: the workspace version's major, minor and
    /// patch (a big-endian u16), as inferno sends its crate's.
    pub version: [u8; 4],
    pub net: NetInfo,
    /// The process id (the config's; several devices on one address).
    pub process: u16,
    /// The config's sample rate: the one rate we run and take.
    pub sample_rate: u32,
    /// The config's encoding (bits per sample), the one we run.
    pub encoding: u8,
}

impl Identity {
    pub fn new(cfg: &DeviceConfig, net: NetInfo) -> Self {
        Self {
            device_id: device_id(net.ip, cfg.process_id),
            process: cfg.process_id,
            manufacturer: cfg.manufacturer.clone(),
            model: cfg.model.clone(),
            version: product_version(),
            sample_rate: cfg.sample_rate,
            encoding: cfg.encoding,
            net,
        }
    }
}

/// The workspace version as the product info carries it.
fn product_version() -> [u8; 4] {
    let major = env!("CARGO_PKG_VERSION_MAJOR").parse().unwrap_or(0);
    let minor = env!("CARGO_PKG_VERSION_MINOR").parse().unwrap_or(0);
    let [hi, lo] = env!("CARGO_PKG_VERSION_PATCH")
        .parse::<u16>()
        .unwrap_or(0)
        .to_be_bytes();
    [major, minor, hi, lo]
}

/// What a request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 0x61: answered with the board info.
    BoardInfo,
    /// 0xc1: answered with the product info.
    ProductInfo,
    /// 0x13: answered with the network info.
    NetworkInfo,
    /// 0x77: answered with [`op::ANSWER_0077`].
    Op0077,
    /// 0x81: the sample rate, read or written; answered with our rate
    /// ([`sample_rate_body`]): we run the config's and take no other.
    SampleRate,
    /// 0x21: clock stats, which a device without a PTP daemon of its own
    /// does not send.
    ClockStats,
    /// 0x83: the encoding options, read or written; answered with our
    /// encoding alone ([`encoding_body`]). Dante Controller asks it of a
    /// device whose board info claims the encoding ([`board_info_body`]).
    Encoding,
    /// 0x85: the sample-rate pull-up options; answered with none alone
    /// ([`PULLUP_BODY`]).
    Pullup,
    /// Anything else: no reply.
    Unknown,
    /// No request: the channel-change notification, sent unasked and
    /// bounded as a kind of its own.
    ChannelChange,
}

/// A request received on [`REQUEST_PORT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    pub seq: u16,
    pub opcode: [u8; 8],
    pub kind: Kind,
}

/// The request in `datagram`, `None` when it is shorter than the header.
pub fn parse_request(datagram: &[u8]) -> Option<Request> {
    let (head, _) = datagram.split_first_chunk::<HEADER_LEN>()?;
    let seq = u16::from_be_bytes([head[4], head[5]]);
    let mut opcode = [0u8; 8];
    opcode.copy_from_slice(&head[24..32]);
    let kind = match opcode {
        [0x07, _, 0x00, 0x61, 0, 0, 0, 0] => Kind::BoardInfo,
        [0x07, _, 0x00, 0xc1, 0, 0, 0, 0] => Kind::ProductInfo,
        [0x07, _, 0x00, 0x21, 0, 0, 0, _] => Kind::ClockStats,
        [0x07, _, 0x00, 0x13, 0, 0, 0, _] => Kind::NetworkInfo,
        [0x07, _, 0x00, 0x77, 0, 0, 0, _] => Kind::Op0077,
        [0x07, _, 0x00, 0x81, 0, 0, 0, _] => Kind::SampleRate,
        [0x07, _, 0x00, 0x83, 0, 0, 0, _] => Kind::Encoding,
        [0x07, _, 0x00, 0x85, 0, 0, 0, _] => Kind::Pullup,
        _ => Kind::Unknown,
    };
    Some(Request { seq, opcode, kind })
}

/// inferno's `write_str_to_buffer`: at most `max` bytes of `s` at `at`,
/// no terminator (the buffer is zeros).
fn put_str(buf: &mut [u8], at: usize, max: usize, s: &str) {
    let bytes = &s.as_bytes()[..s.len().min(max)];
    buf[at..at + bytes.len()].copy_from_slice(bytes);
}

/// The board info: firmware, hardware and boot versions, the board name
/// (cut to 8 bytes at 0x0c and to 16 at 0x38) and inferno's flags.
pub fn board_info_body(id: &Identity) -> [u8; BOARD_INFO_LEN] {
    let mut c = [0u8; BOARD_INFO_LEN];
    c[0x00..0x04].copy_from_slice(&FIRMWARE_VERSION);
    c[0x04..0x08].copy_from_slice(&HARDWARE_VERSION);
    put_str(&mut c, 0x0c, 8, &id.model);
    c[0x14..0x18].copy_from_slice(&FEATURE_FLAGS);
    c[0x23] = 2;
    c[0x27] = 1;
    c[0x28..0x2c].copy_from_slice(&BOOT_VERSION);
    put_str(&mut c, 0x38, 16, &id.model);
    c[0xbb] = BYTE_BB;
    c
}

/// The product info: the manufacturer (8 bytes at 0, 16 at 0x2c), the
/// board name (8 at 8), the version at 0x1c (inferno's: the manufacturer
/// firmware version) and at 0x12c (the product version netaudio and
/// Dante Controller show, [`client`]), the model name (16 at 0xac).
pub fn product_info_body(id: &Identity) -> [u8; PRODUCT_INFO_LEN] {
    let mut c = [0u8; PRODUCT_INFO_LEN];
    put_str(&mut c, 0x00, 8, &id.manufacturer);
    put_str(&mut c, 0x08, 8, &id.model);
    c[0x1c..0x20].copy_from_slice(&id.version);
    put_str(&mut c, 0x2c, 16, &id.manufacturer);
    put_str(&mut c, 0xac, 16, &id.model);
    c[0x12c..0x130].copy_from_slice(&id.version);
    c
}

/// The sample rate as the hardware answers a 0x81 read (a Brooklyn
/// card's layout, [`client::parse_sample_rate`]): `00 18`, the count of
/// rates (1), our rate in effect, none pending (0), `00 02 00 00`, then the
/// rates: ours alone.
pub fn sample_rate_body(rate: u32) -> [u8; SAMPLE_RATE_LEN] {
    let mut c = [0u8; SAMPLE_RATE_LEN];
    c[0x00..0x02].copy_from_slice(&0x0018u16.to_be_bytes());
    c[0x02..0x04].copy_from_slice(&1u16.to_be_bytes());
    c[0x04..0x08].copy_from_slice(&rate.to_be_bytes());
    c[0x0c..0x0e].copy_from_slice(&2u16.to_be_bytes());
    c[0x10..0x14].copy_from_slice(&rate.to_be_bytes());
    c
}

/// The encoding options as a Tascam answers 0x83 (captured on mbc,
/// 2.11.2026) but with one option: `00 18` (the options at 0x10), the
/// count (1), our encoding in effect, none pending (0), `00 02 00 00`,
/// then the options: ours alone, so Dante Controller offers no change.
pub fn encoding_body(bits: u8) -> [u8; ENCODING_LEN] {
    let mut c = [0u8; ENCODING_LEN];
    c[0x00..0x02].copy_from_slice(&0x0018u16.to_be_bytes());
    c[0x02..0x04].copy_from_slice(&1u16.to_be_bytes());
    c[0x04..0x08].copy_from_slice(&u32::from(bits).to_be_bytes());
    c[0x0c..0x0e].copy_from_slice(&2u16.to_be_bytes());
    c[0x10..0x14].copy_from_slice(&u32::from(bits).to_be_bytes());
    c
}

/// The network info: `00 01 00 00 00 00`, the link speed, 1, the MAC, the
/// address, the netmask, the gateway twice (the second is DNS, which
/// inferno fills with the gateway), then `00 18 00 30` and 28 zeros.
pub fn network_info_body(net: &NetInfo) -> [u8; NETWORK_INFO_LEN] {
    let mut c = [0u8; NETWORK_INFO_LEN];
    c[0x00..0x06].copy_from_slice(&[0x00, 0x01, 0x00, 0x00, 0x00, 0x00]);
    c[0x06..0x08].copy_from_slice(&net.link_speed.to_be_bytes());
    c[0x08..0x0a].copy_from_slice(&1u16.to_be_bytes());
    c[0x0a..0x10].copy_from_slice(&net.mac);
    c[0x10..0x14].copy_from_slice(&net.ip.octets());
    c[0x14..0x18].copy_from_slice(&net.netmask.octets());
    c[0x18..0x1c].copy_from_slice(&net.gateway.octets());
    c[0x1c..0x20].copy_from_slice(&net.gateway.octets());
    c[0x20..0x24].copy_from_slice(&[0x00, 0x18, 0x00, 0x30]);
    c
}

/// The channel-change notification's body for the RX channels `rx` (from
/// 0): the bitmask's length in bytes (at least 1), then the bitmask, bit
/// `ch % 8` of byte `ch / 8` for channel `ch`.
pub fn channel_change_body(rx: &[usize]) -> Vec<u8> {
    let len = rx.iter().map(|&ch| ch / 8 + 1).max().unwrap_or(1);
    let mut mask = vec![0u8; len];
    for &ch in rx {
        mask[ch / 8] |= 1 << (ch % 8);
    }
    let mut out = u16::try_from(len)
        .unwrap_or(u16::MAX)
        .to_be_bytes()
        .to_vec();
    out.append(&mut mask);
    out
}

/// A channel's peak as the heartbeat carries it (inferno's
/// `peaks_of_buffers`): `−40·log10(peak / i32::MAX)`, rounded, 0 to 255
/// (silence 255, full scale 0), in `f32` as inferno computes it.
pub fn peak_byte(peak: u32) -> u8 {
    let peak = peak.min(i32::MAX as u32) as i32;
    let lin = peak as f32 / i32::MAX as f32;
    let log = lin.log10() * 40.0;
    (-log).round().clamp(0.0, 255.0) as u8
}

/// What the heartbeat reports once the media clock is ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Records {
    /// dsync's frequency adjustment, ppb.
    pub freq_ppb: i32,
    /// Each TX channel's peak byte ([`peak_byte`]).
    pub tx_peaks: Vec<u8>,
    /// Each RX channel's peak byte.
    pub rx_peaks: Vec<u8>,
    /// Our sample rate.
    pub rate: u32,
    /// Each RX flow slot's largest lateness since the last beat, samples.
    pub lateness: [u32; MAX_FLOWS],
    /// Each RX flow slot's late packets since the device started (the low
    /// 32 bits).
    pub late: [u32; MAX_FLOWS],
}

/// Appends one heartbeat record of type `kind` with `payload`: a 12-byte
/// head (the record's length, its type, 4, the payload's length, the
/// packet's sequence `seq`, 0), the payload, then zeros up to a multiple
/// of 4 (the head states the length without them).
fn push_record(out: &mut Vec<u8>, seq: u16, kind: u16, payload: &[u8]) {
    let word = |v: usize| u16::try_from(v).unwrap_or(u16::MAX);
    let head = [
        word(12 + payload.len()),
        kind,
        4,
        word(payload.len()),
        seq,
        0,
    ];
    out.extend(head.iter().flat_map(|f| f.to_be_bytes()));
    out.extend_from_slice(payload);
    out.resize(out.len().next_multiple_of(4), 0);
}

/// `u16` words big-endian, then `u32` words big-endian.
fn words(short: &[u16], long: &[u32]) -> Vec<u8> {
    short
        .iter()
        .flat_map(|w| w.to_be_bytes())
        .chain(long.iter().flat_map(|w| w.to_be_bytes()))
        .collect()
}

/// The heartbeat's body with `records` for the packet of sequence `seq`:
/// the frequency (0x8001), the peaks (0x8002) when there are channels,
/// then each RX flow slot's lateness (0x8003) and late count (0x8004).
pub fn heartbeat_body(seq: u16, r: &Records) -> Vec<u8> {
    let count = |n: usize| u16::try_from(n).unwrap_or(u16::MAX);
    let slots = count(MAX_FLOWS);
    let mut out = Vec::new();
    push_record(&mut out, seq, record::FREQUENCY, &r.freq_ppb.to_be_bytes());
    if !(r.tx_peaks.is_empty() && r.rx_peaks.is_empty()) {
        // The TX and RX counts, each with a zero word, then the peaks'
        // offset in the record (24) and a zero word, then the peak bytes.
        let mut peaks = words(
            &[
                count(r.tx_peaks.len()),
                0,
                count(r.rx_peaks.len()),
                0,
                24,
                0,
            ],
            &[],
        );
        peaks.extend_from_slice(&r.tx_peaks);
        peaks.extend_from_slice(&r.rx_peaks);
        push_record(&mut out, seq, record::PEAKS, &peaks);
    }
    let mut lateness = vec![r.rate];
    lateness.extend_from_slice(&r.lateness);
    push_record(
        &mut out,
        seq,
        record::RX_LATENESS,
        &words(&[slots, 0, 24, 0], &lateness),
    );
    push_record(
        &mut out,
        seq,
        record::RX_LATE,
        &words(&[slots, 0, 20, 0], &r.late),
    );
    out
}

/// Where the heartbeat's records come from.
pub trait Beat: Send + Sync {
    /// The records now; `None` while the media clock is not ready (the
    /// heartbeat is the header alone then, as inferno's without a clock).
    fn records(&self) -> Option<Records>;

    /// What the clock stats state now: `None` (no reply) while the media
    /// clock is not ready or no grandmaster is known.
    fn clock(&self) -> Option<Follower> {
        None
    }
}

/// Our clock as a PTP follower states it (the clock-stats reply): the
/// grandmaster dsync follows and dsync's frequency adjustment. Our clock
/// follows dsync's; we send no PTP of our own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Follower {
    /// The grandmaster's PTPv1 UUID (dsync's DSYX bytes 42–47).
    pub grandmaster: [u8; 6],
    /// The master's PTPv1 UUID, the clock whose Sync dsync follows (DSYX
    /// bytes 112–117, dsync 1.26 on); the grandmaster when dsync names
    /// none.
    pub master: [u8; 6],
    /// dsync's frequency adjustment, ppb.
    pub freq_ppb: i32,
}

/// A Brooklyn II follower's clock stats, captured (its ids synthetic): the
/// layout [`clock_stats_body`] fills.
const CLOCK_TEMPLATE: [u8; 188] = [
    0x00, 0x03, 0x00, 0x03, 0x00, 0x00, 0x00, 0x9f, 0xff, 0xff, 0xf6, 0x75, 0x02, 0x00, 0x00, 0x00,
    0x00, 0x02, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
    0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x34, 0x00, 0x09, 0x00, 0x00, 0x02, 0x94, 0x00, 0x00,
    0x00, 0x03, 0x0d, 0x40, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x60, 0x0c, 0x00,
    0x00, 0x00, 0x00, 0x0c, 0x00, 0x98, 0x00, 0x20, 0x00, 0x03, 0x00, 0x00, 0x00, 0x68, 0x10, 0x00,
    0x00, 0x00, 0x00, 0x01, 0x01, 0x02, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x09, 0x00, 0x07,
    0x00, 0x01, 0x00, 0x02, 0x02, 0x02, 0x02, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x03, 0x00, 0x03,
    0x00, 0x01, 0x00, 0x03, 0x02, 0x02, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x03, 0x00, 0x07,
    0x00, 0x03, 0x00, 0x07, 0x00, 0xb8, 0x00, 0x04, 0x02, 0x00, 0x00, 0xff, 0xfe, 0x00, 0x00, 0x02,
    0x02, 0x00, 0x00, 0xff, 0xfe, 0x00, 0x00, 0x01, 0x02, 0x00, 0x00, 0xff, 0xfe, 0x00, 0x00, 0x01,
    0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
];

/// The clock stats of a follower with the clock UUID `own` (our MAC) that
/// follows `f`: disciplined, synchronized, one port following (state 9),
/// as a hardware follower answers them; the ids and the frequency ours.
/// Dante Controller keeps a device's config half-loaded until this reply
/// comes ("Retrieving model information", mbc 2.11.2026).
pub fn clock_stats_body(own: [u8; 6], f: &Follower) -> Vec<u8> {
    let mut c = CLOCK_TEMPLATE.to_vec();
    c[0x08..0x0c].copy_from_slice(&f.freq_ppb.to_be_bytes());
    c[0x0c..0x12].copy_from_slice(&own);
    c[0x14..0x1a].copy_from_slice(&f.master);
    c[0x1c..0x22].copy_from_slice(&f.grandmaster);
    c
}

/// RX channels whose name, subscription or status changed, waiting for
/// the next channel-change notification: coalesced, so one notification
/// names every channel that changed since the last, and none is lost while
/// the bound holds it back. The ARC server and the manager's loop add to
/// it; the info server takes from it.
#[derive(Debug, Default)]
pub struct Changes {
    pending: Mutex<BTreeSet<usize>>,
    added: Notify,
}

impl Changes {
    /// RX channels `rx` (from 0) changed.
    pub fn add(&self, rx: impl IntoIterator<Item = usize>) {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let before = pending.len();
        pending.extend(rx);
        if pending.len() > before {
            self.added.notify_one();
        }
    }

    /// Whether a change waits.
    pub fn is_empty(&self) -> bool {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    }

    /// Every change waiting, in channel order; none waits after.
    pub fn take(&self) -> Vec<usize> {
        std::mem::take(&mut *self.pending.lock().unwrap_or_else(PoisonError::into_inner))
            .into_iter()
            .collect()
    }

    /// Until a change is added (at once when one was added since the last
    /// wait).
    pub async fn added(&self) {
        self.added.notified().await;
    }
}

/// Builds the device's info packets: its identity and the sequence number
/// its next packet carries.
#[derive(Debug, Clone)]
pub struct InfoBuilder {
    id: Identity,
    seq: u16,
    /// What the clock stats state ([`InfoBuilder::set_clock`]).
    clock: Option<Follower>,
}

impl InfoBuilder {
    /// The first packet carries sequence 1, as inferno's.
    pub fn new(id: Identity) -> Self {
        Self {
            id,
            seq: 1,
            clock: None,
        }
    }

    /// What the next clock-stats reply states (the server sets it from
    /// [`Beat::clock`] before each request); none: no reply.
    pub fn set_clock(&mut self, clock: Option<Follower>) {
        self.clock = clock;
    }

    /// The next packet: the header, then `body`; the sequence moves on.
    fn packet(&mut self, start: u16, opcode: [u8; 8], body: &[u8]) -> Vec<u8> {
        let len = u16::try_from(HEADER_LEN + body.len()).expect("an info body under 64 KiB");
        let mut out = Vec::with_capacity(usize::from(len));
        out.extend_from_slice(&start.to_be_bytes());
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.extend_from_slice(&self.id.process.to_be_bytes());
        out.extend_from_slice(&self.id.device_id);
        out.extend_from_slice(&VENDOR);
        out.extend_from_slice(&opcode);
        out.extend_from_slice(body);
        self.seq = self.seq.wrapping_add(1);
        out
    }

    pub fn board_info(&mut self) -> Vec<u8> {
        let body = board_info_body(&self.id);
        self.packet(START_INFO, op::BOARD_INFO, &body)
    }

    pub fn product_info(&mut self) -> Vec<u8> {
        let body = product_info_body(&self.id);
        self.packet(START_INFO, op::PRODUCT_INFO, &body)
    }

    /// The heartbeat: the header alone without `records` (the clock is not
    /// ready), else with them ([`heartbeat_body`], each record carrying
    /// this packet's sequence).
    pub fn heartbeat(&mut self, records: Option<&Records>) -> Vec<u8> {
        let body = records.map_or_else(Vec::new, |r| heartbeat_body(self.seq, r));
        self.packet(START_HEARTBEAT, op::HEARTBEAT, &body)
    }

    /// The channel-change notification for the RX channels `rx` (from 0).
    pub fn channel_change(&mut self, rx: &[usize]) -> Vec<u8> {
        self.packet(START_INFO, op::CHANNEL_CHANGE, &channel_change_body(rx))
    }

    /// The channel-change notification for every change waiting in
    /// `changes`, when one waits and `limit` has room for one now; `None`
    /// (the changes kept for later, no sequence used) otherwise.
    pub fn notify_within(
        &mut self,
        limit: &mut Limit,
        now: Instant,
        changes: &Changes,
    ) -> Option<Vec<u8>> {
        if changes.is_empty() || !limit.room(now, Kind::ChannelChange) {
            return None;
        }
        limit.allow(now, Kind::ChannelChange);
        let rx = changes.take();
        debug!(channels = rx.len(), "RX channels changed: notified");
        Some(self.channel_change(&rx))
    }

    /// The reply to a datagram received on [`REQUEST_PORT`], for the info
    /// group; `None` (and no sequence used) when there is none.
    pub fn answer(&mut self, datagram: &[u8]) -> Option<Vec<u8>> {
        let Some(req) = parse_request(datagram) else {
            debug!(
                len = datagram.len(),
                "info datagram shorter than its header: dropped"
            );
            return None;
        };
        let opcode = format_args!("{:016x}", u64::from_be_bytes(req.opcode));
        match req.kind {
            Kind::BoardInfo => Some(self.board_info()),
            Kind::ProductInfo => Some(self.product_info()),
            Kind::NetworkInfo => {
                let body = network_info_body(&self.id.net);
                Some(self.packet(START_INFO, op::NETWORK_INFO, &body))
            }
            Kind::Op0077 => Some(self.packet(START_INFO, op::ANSWER_0077, &ANSWER_0077_BODY)),
            Kind::SampleRate => {
                let body = sample_rate_body(self.id.sample_rate);
                Some(self.packet(START_INFO, op::SAMPLE_RATE, &body))
            }
            Kind::Encoding => {
                let body = encoding_body(self.id.encoding);
                Some(self.packet(START_INFO, op::ENCODING, &body))
            }
            Kind::Pullup => Some(self.packet(START_INFO, op::PULLUP, &PULLUP_BODY)),
            Kind::ClockStats => match self.clock {
                Some(f) => {
                    let body = clock_stats_body(self.id.net.mac, &f);
                    Some(self.packet(START_INFO, op::CLOCK_STATS, &body))
                }
                None => {
                    debug!(op = %opcode, seq = req.seq, "clock stats asked: the clock is not ready, no reply");
                    None
                }
            },
            Kind::Unknown | Kind::ChannelChange => {
                info!(op = %opcode, seq = req.seq, "info request not answered");
                None
            }
        }
        .inspect(|_| debug!(op = %opcode, seq = req.seq, "info request answered"))
    }

    /// [`InfoBuilder::answer`] within `limit` ([`limit`]): a reply over the
    /// bound is dropped (counted there) and uses no sequence.
    pub fn answer_within(
        &mut self,
        limit: &mut Limit,
        now: Instant,
        datagram: &[u8],
    ) -> Option<Vec<u8>> {
        if let Some(req) = parse_request(datagram)
            && !limit.allow(now, req.kind)
        {
            return None;
        }
        self.answer(datagram)
    }
}

#[cfg(test)]
mod tests;
