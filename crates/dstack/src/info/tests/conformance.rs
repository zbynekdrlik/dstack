// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The heartbeat and the info replies against the hardware's format
//! (dstack #349), read with the strict walker written from hardware
//! captures (`tests/conformance/walk.rs`), never with `latency::parse` or
//! the writer's own assumptions. dstack #345 passed CI because its tests
//! used aligned channel counts and read back with our own parser; here
//! every TX and RX count from 0 to 128 is walked, with and without a
//! clock.

use super::*;

use crate::arc::flows::tests::hex;

#[path = "../../../tests/conformance/walk.rs"]
mod walk;

const HARDWARE: &str = include_str!("../../../tests/fixtures/heartbeat_hardware.json");

fn fixture() -> serde_json::Value {
    serde_json::from_str(HARDWARE).expect("the fixture is JSON")
}

/// A captured datagram: `group` (`heartbeats`, `malformed`,
/// `info_replies`, `channel_changes`), `name`.
fn captured(group: &str, name: &str) -> Vec<u8> {
    hex(fixture()[group][name]
        .as_str()
        .expect("the capture is in the fixture"))
}

/// Each record's offset in the datagram, by type, from a strict walk.
fn offsets(beat: &walk::Heartbeat) -> Vec<(u16, usize)> {
    let mut at = walk::HEADER;
    beat.records
        .iter()
        .map(|&(kind, len)| {
            let here = at;
            at += len;
            (kind, here)
        })
        .collect()
}

fn record_at(d: &[u8], beat: &walk::Heartbeat, kind: u16) -> usize {
    offsets(beat)
        .into_iter()
        .find(|&(k, _)| k == kind)
        .map(|(_, at)| at)
        .unwrap_or_else(|| panic!("no record {kind:04x} in {} bytes", d.len()))
}

/// Records for `tx` and `rx` channels, every value distinct.
fn records_for(tx: usize, rx: usize) -> Records {
    let lateness = std::array::from_fn(|k| 3 * k as u32 + 1);
    let late = std::array::from_fn(|k| 5 * k as u32 + 2);
    Records {
        freq_ppb: -4321,
        tx_peaks: (0..tx).map(|k| (7 * k % 251) as u8).collect(),
        rx_peaks: (0..rx).map(|k| (13 * k % 251 + 1) as u8).collect(),
        rate: 96_000,
        lateness,
        late,
    }
}

#[test]
fn the_walker_reads_every_hardware_heartbeat() {
    let walked = |name: &str| {
        walk::heartbeat(&captured("heartbeats", name)).unwrap_or_else(|e| panic!("{name}: {e}"))
    };
    let zero_two = walked("ultimo_0tx_2rx");
    assert_eq!(
        zero_two.records,
        [
            (0x8001, 28),
            (0x8000, 36),
            (0x8002, 28),
            (0x8003, 32),
            (0x8004, 28)
        ]
    );
    let (tx, rx) = zero_two.peaks.clone().expect("peaks");
    assert_eq!((tx.len(), rx.len()), (0, 2));
    let (rate, lateness) = zero_two.lateness.clone().expect("lateness");
    assert_eq!((rate, lateness.len()), (96_000, 2));
    assert_eq!(zero_two.late.map(|l| l.len()), Some(2));
    let two_two = walked("ultimo_2tx_2rx");
    let (tx, rx) = two_two.peaks.expect("peaks");
    assert_eq!((tx.len(), rx.len()), (2, 2));
    let card = walked("brooklyn_card");
    assert_eq!(card.records, [(0x8003, 280), (0x8004, 276)]);
    assert_eq!(card.lateness.map(|(r, l)| (r, l.len())), Some((96_000, 64)));
    assert_eq!(card.late.map(|l| l.len()), Some(64));
    assert_eq!(walked("yamaha_card").records, [(0x8000, 52), (0x8001, 16)]);
}

/// The walker refuses what a walk by length cannot read: dstack before
/// v0.6.155 (#345), captured, and the hardware's own bytes broken one
/// rule at a time.
#[test]
fn the_walker_refuses_a_malformed_heartbeat() {
    let old = captured("malformed", "dstack_0_6_150_2tx_0rx");
    let e = walk::heartbeat(&old).expect_err("#345's short peaks record");
    assert!(e.contains("record 8002 at 48: length 26"), "{e}");
    let good = captured("heartbeats", "ultimo_0tx_2rx");
    let beat = walk::heartbeat(&good).unwrap();
    let peaks = record_at(&good, &beat, 0x8002);
    let broken = |at: usize, value: u16| {
        let mut d = good.clone();
        d[at..at + 2].copy_from_slice(&value.to_be_bytes());
        walk::heartbeat(&d).expect_err("a broken heartbeat")
    };
    assert!(broken(2, 180).contains("length field"));
    assert!(broken(peaks + 6, 12).contains("content length 12"));
    assert!(broken(peaks + 4, 3).contains("head words 3"));
    assert!(broken(peaks + 2, 0x8005).contains("a type no device sends"));
    assert!(broken(peaks + 2, 0x8001).contains("a second one"));
    assert!(broken(peaks + 20, 26).contains("table head"));
    assert!(broken(peaks + 16, 6).contains("so the record is 32 long"));
    let mut padded = good.clone();
    padded[peaks + 27] = 1;
    assert!(walk::heartbeat(&padded).unwrap_err().contains("padding"));
    // 0x8004 (the last record) cut to one slot, beside 0x8003's two.
    let late = record_at(&good, &beat, 0x8004);
    let mut one_slot = good[..late + 24].to_vec();
    for (at, value) in [(2, late + 24), (late, 24), (late + 6, 12), (late + 12, 1)] {
        one_slot[at..at + 2].copy_from_slice(&u16::try_from(value).unwrap().to_be_bytes());
    }
    assert!(
        walk::heartbeat(&one_slot)
            .unwrap_err()
            .contains("2 slots of lateness, 1 of late counts")
    );
    let mut longer = good.clone();
    longer.extend([0; 4]);
    let n = u16::try_from(longer.len()).unwrap();
    longer[2..4].copy_from_slice(&n.to_be_bytes());
    assert!(walk::heartbeat(&longer).unwrap_err().contains("no room"));
}

/// Every TX and RX count from 0 to 128, with a clock: the walk ends at the
/// datagram's end and gives the values put in. Without a clock the
/// heartbeat is the header alone, which walks too.
#[test]
fn our_heartbeat_walks_for_every_channel_count() {
    let mut mc = InfoBuilder::new(identity());
    let bare = mc.heartbeat(None);
    assert_eq!(walk::heartbeat(&bare), Ok(walk::Heartbeat::default()));
    for tx in 0..=128 {
        for rx in 0..=128 {
            let r = records_for(tx, rx);
            let d = mc.heartbeat(Some(&r));
            let beat = walk::heartbeat(&d).unwrap_or_else(|e| panic!("{tx}+{rx}: {e}"));
            let peaks = (tx + rx > 0).then(|| (r.tx_peaks.clone(), r.rx_peaks.clone()));
            assert_eq!(beat.peaks, peaks, "{tx}+{rx}");
            assert_eq!(
                beat.lateness,
                Some((96_000, r.lateness.to_vec())),
                "{tx}+{rx}"
            );
            assert_eq!(beat.late, Some(r.late.to_vec()), "{tx}+{rx}");
        }
    }
}

/// Our records' heads are the hardware's, byte for byte, for the same
/// channel counts (the record's sequence aside, the hardware's is not the
/// packet's): the peaks record of an UltimoX2 with 0+2 and 2+2 channels,
/// and the lateness and late records' type, 4 and table offsets.
#[test]
fn our_record_heads_are_the_hardware_s() {
    let mut mc = InfoBuilder::new(identity());
    for (name, tx, rx) in [("ultimo_0tx_2rx", 0, 2), ("ultimo_2tx_2rx", 2, 2)] {
        let hw = captured("heartbeats", name);
        let hw_beat = walk::heartbeat(&hw).unwrap();
        let ours = mc.heartbeat(Some(&records_for(tx, rx)));
        let our_beat = walk::heartbeat(&ours).unwrap();
        let (h, o) = (
            record_at(&hw, &hw_beat, 0x8002),
            record_at(&ours, &our_beat, 0x8002),
        );
        assert_eq!(ours[o..o + 8], hw[h..h + 8], "{name}: the peaks head");
        assert_eq!(
            ours[o + 10..o + 24],
            hw[h + 10..h + 24],
            "{name}: the table"
        );
        for kind in [0x8003, 0x8004] {
            let (h, o) = (
                record_at(&hw, &hw_beat, kind),
                record_at(&ours, &our_beat, kind),
            );
            assert_eq!(ours[o + 2..o + 6], hw[h + 2..h + 6], "{name} {kind:04x}");
            assert_eq!(
                ours[o + 10..o + 12],
                hw[h + 10..h + 12],
                "{name} {kind:04x}"
            );
            assert_eq!(
                ours[o + 14..o + 20],
                hw[h + 14..h + 20],
                "{name} {kind:04x}"
            );
        }
    }
}

/// An info request for `op` (the request opcode's byte 3), as Dante
/// Controller's captured one.
fn request(op: u8) -> Vec<u8> {
    asking([0x07, 0x2a, 0x00, op, 0, 0, 0, 0])
}

/// The clock stats' bytes that differ between the UltimoX2 and our reply
/// for a reason: the frequency, our own, the master's and the
/// grandmaster's ids (0x08-0x21, 0x98-0xaf, masked in the fixture), and a
/// count per clock port that is 2 on the UltimoX2 and 1 on the Brooklyn II
/// our template is (0x6b, 0x7b, 0x8b).
fn ours_by_device(at: usize) -> bool {
    matches!(at, 0x08..=0x21 | 0x6b | 0x7b | 0x8b | 0x98..=0xaf)
}

/// Every reply we send on the info group walks, and the network info and
/// clock stats have the UltimoX2's sizes and fixed words (one interface).
/// These replies hold no length-walked records: the header's length is
/// their walk, and the bodies are compared with the hardware's.
#[test]
fn our_info_replies_walk_and_take_the_hardware_s_sizes() {
    let mut mc = InfoBuilder::new(identity());
    mc.set_clock(Some(Follower {
        grandmaster: [2, 0, 0, 0, 0, 2],
        master: [2, 0, 0, 0, 0, 2],
        freq_ppb: -9,
    }));
    for op in [0x61, 0xc1, 0x13, 0x77, 0x81, 0x83, 0x85, 0x21] {
        let reply = mc.answer(&request(op)).expect("answered");
        walk::info(&reply).unwrap_or_else(|e| panic!("{op:02x}: {e}"));
    }
    let hw_net = walk::info(&captured("info_replies", "ultimo_network_info")).unwrap();
    let ours = walk::info(&mc.answer(&request(0x13)).unwrap()).unwrap();
    assert_eq!(ours.opcode[2..], hw_net.opcode[2..]);
    assert_eq!(ours.body.len(), hw_net.body.len());
    for (from, to) in [(0x00, 0x06), (0x08, 0x0a)] {
        assert_eq!(ours.body[from..to], hw_net.body[from..to], "{from:#x}");
    }
    assert_eq!(ours.body[0x20..], hw_net.body[0x20..]);
    let hw_clock = walk::info(&captured("info_replies", "ultimo_clock_stats")).unwrap();
    let ours = walk::info(&mc.answer(&request(0x21)).unwrap()).unwrap();
    assert_eq!(ours.opcode[2..], hw_clock.opcode[2..]);
    assert_eq!(ours.body.len(), hw_clock.body.len());
    let differ: Vec<usize> = (0..ours.body.len())
        .filter(|&at| !ours_by_device(at) && ours.body[at] != hw_clock.body[at])
        .collect();
    assert!(differ.is_empty(), "clock stats bytes {differ:x?}");
}

/// The hardware's channel-change notifications walk and name the channel
/// renamed: an UltimoX2's channel 1 (of 2), a Yamaha card's 7 and 8 (of
/// 128).
#[test]
fn the_walker_reads_the_hardware_channel_changes() {
    for (name, channel, len) in [
        ("ultimo_rx1", 1, 35),
        ("yamaha_rx7", 7, 35),
        ("yamaha_rx8", 8, 36),
    ] {
        let d = captured("channel_changes", name);
        assert_eq!(d.len(), len, "{name}");
        let info = walk::info(&d).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(info.opcode[2..4], walk::CHANNEL_CHANGE, "{name}");
        assert_eq!(walk::changed_channels(&info.body), [channel], "{name}");
    }
}

/// The hardware's notification broken one rule at a time: a mask length
/// past the body, two zero bytes after the mask, a zero length, a mask
/// whose last byte names no channel (each with the header's length set
/// right).
#[test]
fn the_walker_refuses_a_malformed_channel_change() {
    let good = captured("channel_changes", "yamaha_rx8");
    let refused = |mut d: Vec<u8>, n: u16| {
        d[walk::HEADER..walk::HEADER + 2].copy_from_slice(&n.to_be_bytes());
        let len = u16::try_from(d.len()).unwrap();
        d[2..4].copy_from_slice(&len.to_be_bytes());
        walk::info(&d).expect_err("a broken channel change")
    };
    assert!(refused(good.clone(), 3).contains("a mask of 3 bytes in a body of 4"));
    let mut padded = good.clone();
    padded.extend([0; 2]);
    assert!(refused(padded.clone(), 2).contains("a mask of 2 bytes in a body of 6"));
    assert!(refused(good.clone(), 0).contains("a mask of 0 bytes"));
    padded.truncate(good.len() + 1);
    let e = refused(padded, 3);
    assert!(
        e.contains("the mask's last byte of 3 names no channel"),
        "{e}"
    );
}

/// Our notification for the channels the hardware named is the hardware's
/// body and length byte for byte; the opcode's second byte is the
/// device's own (ours 0x2a, inferno's).
#[test]
fn our_channel_change_is_the_hardware_s() {
    let mut mc = InfoBuilder::new(identity());
    for (name, channel) in [("ultimo_rx1", 1), ("yamaha_rx7", 7), ("yamaha_rx8", 8)] {
        let hw = captured("channel_changes", name);
        let ours = mc.channel_change(&[channel]);
        assert_eq!(ours.len(), hw.len(), "{name}");
        assert_eq!(ours[26..], hw[26..], "{name}");
    }
}

/// The channel-change notification for any set of RX channels, an odd
/// mask length too, walks to the datagram's end and names the channels.
#[test]
fn our_channel_change_walks_for_every_channel() {
    let mut mc = InfoBuilder::new(identity());
    let mut sets: Vec<Vec<usize>> = (0..=128).map(|ch| vec![ch]).collect();
    sets.extend([
        vec![0, 9],
        vec![2, 5, 130],
        (0..64).collect(),
        vec![7, 8, 23],
    ]);
    for rx in sets {
        let d = mc.channel_change(&rx);
        let info = walk::info(&d).unwrap_or_else(|e| panic!("{rx:?}: {e}"));
        assert_eq!(info.opcode[2..4], walk::CHANNEL_CHANGE);
        assert_eq!(walk::changed_channels(&info.body), rx);
    }
}
