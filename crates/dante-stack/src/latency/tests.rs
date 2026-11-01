use super::*;
use crate::info::{Records, heartbeat_body};

/// A heartbeat datagram with `body`, from device id 1.
fn datagram(body: &[u8]) -> Vec<u8> {
    let mut d = Vec::new();
    d.extend_from_slice(&START_HEARTBEAT.to_be_bytes());
    d.extend_from_slice(
        &u16::try_from(HEADER_LEN + body.len())
            .unwrap()
            .to_be_bytes(),
    );
    d.extend_from_slice(&[0, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
    d.extend_from_slice(b"Audinate");
    d.extend_from_slice(&op::HEARTBEAT);
    d.extend_from_slice(body);
    d
}

/// A hardware receiver's two records as heard on the site (a Yamaha
/// card, 64 slots, 96 kHz), cut to their first 12 values.
fn hardware_records(lateness: &[u32], late: &[u32]) -> Vec<u8> {
    let mut out = Vec::new();
    let n = 64usize;
    // 0x8003: head, n 0 24 0, the rate, n values.
    out.extend_from_slice(&u16::try_from(24 + 4 * n).unwrap().to_be_bytes());
    out.extend_from_slice(&[0x80, 0x03, 0x00, 0x04, 0x01, 0x0c, 0xae, 0x38, 0, 0]);
    out.extend_from_slice(&[0x00, 0x40, 0, 0, 0x00, 0x18, 0, 0]);
    out.extend_from_slice(&96_000u32.to_be_bytes());
    for k in 0..n {
        out.extend_from_slice(&lateness.get(k).copied().unwrap_or(0).to_be_bytes());
    }
    // 0x8004: head, n 0 20 0, n values.
    out.extend_from_slice(&u16::try_from(20 + 4 * n).unwrap().to_be_bytes());
    out.extend_from_slice(&[0x80, 0x04, 0x00, 0x04, 0x01, 0x08, 0xae, 0x38, 0, 0]);
    out.extend_from_slice(&[0x00, 0x40, 0, 0, 0x00, 0x14, 0, 0]);
    for k in 0..n {
        out.extend_from_slice(&late.get(k).copied().unwrap_or(0).to_be_bytes());
    }
    out
}

#[test]
fn a_hardware_receiver_s_records_read() {
    // The values the FOH card sent on 1.11 (slots 1 to 8).
    let lateness = [0x177, 0, 0x26, 7, 7, 0xa, 0x24, 0x50];
    let late = [0x5e, 0, 0, 0, 0x2c];
    let beat = parse(&datagram(&hardware_records(&lateness, &late))).expect("a heartbeat");
    let (rate, got) = beat.lateness.expect("0x8003");
    assert_eq!(rate, 96_000);
    assert_eq!(got.len(), 64);
    assert_eq!(got[..8], lateness);
    let got = beat.late.expect("0x8004");
    assert_eq!(got.len(), 64);
    assert_eq!(got[..5], late);
    assert!(got[5..].iter().all(|&v| v == 0));
}

#[test]
fn our_own_heartbeat_reads_back() {
    let mut lateness = [0u32; crate::arc::MAX_FLOWS as usize];
    lateness[0] = 48;
    lateness[31] = 9_600;
    let body = heartbeat_body(
        5,
        &Records {
            freq_ppb: -7,
            tx_peaks: vec![1, 2, 3],
            rx_peaks: vec![4],
            rate: 48_000,
            lateness,
            late: std::array::from_fn(|slot| slot as u32 * 3),
        },
    );
    let beat = parse(&datagram(&body)).expect("a heartbeat");
    assert_eq!(beat.lateness, Some((48_000, lateness.to_vec())));
    let late: Vec<u32> = (0..32).map(|slot| slot * 3).collect();
    assert_eq!(beat.late, Some(late));
}

#[test]
fn only_a_heartbeat_parses_and_a_bad_record_is_left_out() {
    let good = datagram(&hardware_records(&[1], &[2]));
    assert!(parse(&good).is_some());
    assert_eq!(
        parse(&good[..HEADER_LEN - 1]),
        None,
        "shorter than the header"
    );
    let mut info = good.clone();
    info[1] = 0xff;
    assert_eq!(parse(&info), None, "an info packet");
    let mut other = good.clone();
    other[31] = 1;
    assert_eq!(parse(&other), None, "another opcode");
    assert_eq!(parse(&datagram(&[])), Some(Beat::default()), "no records");
    // A record whose values pass it: left out, the next one still read.
    let mut records = hardware_records(&[1], &[2]);
    records[12..14].copy_from_slice(&0x7fu16.to_be_bytes());
    let beat = parse(&datagram(&records)).unwrap();
    assert_eq!(beat.lateness, None);
    assert_eq!(beat.late.unwrap()[0], 2);
    // A record longer than the datagram ends the walk.
    let mut long = hardware_records(&[1], &[2]);
    long[0..2].copy_from_slice(&0x7fffu16.to_be_bytes());
    assert_eq!(parse(&datagram(&long)), Some(Beat::default()));
    // A length under 4 ends it too.
    let mut tiny = hardware_records(&[1], &[2]);
    tiny[0..2].copy_from_slice(&3u16.to_be_bytes());
    assert_eq!(parse(&datagram(&tiny)), Some(Beat::default()));
    // An unknown record is stepped over.
    let mut skip = vec![0x00, 0x08, 0x80, 0x01, 0, 0, 0, 0];
    skip.extend(hardware_records(&[3], &[4]));
    let beat = parse(&datagram(&skip)).unwrap();
    assert_eq!(beat.lateness.unwrap().1[0], 3);
}

#[test]
fn a_gather_turns_samples_into_us_and_counts_late_packets_in_the_window() {
    let mut g = Gather::default();
    let beat = |lateness: Vec<u32>, late: Vec<u32>| Beat {
        lateness: Some((96_000, lateness)),
        late: Some(late),
    };
    g.add(&beat(vec![96, 24], vec![1_000, 5]));
    g.add(&beat(vec![960, 0], vec![1_024, 5]));
    // The device started again: its count went down, all of it is new.
    g.add(&beat(vec![9_600, 1], vec![3, 5]));
    let got = g.result();
    assert_eq!((got.beats, got.rate), (3, 96_000));
    assert_eq!(
        got.slots,
        [
            SlotLatency {
                slot: 1,
                peaks_us: vec![1_000, 10_000, 100_000],
                late: 27,
                late_total: 3,
            },
            SlotLatency {
                slot: 2,
                peaks_us: vec![250, 0, 10],
                late: 0,
                late_total: 5,
            },
        ]
    );
    // A beat with one record only: the other slots' lists are untouched.
    let mut g = Gather::default();
    g.add(&Beat {
        lateness: None,
        late: Some(vec![7, 0, 1]),
    });
    let got = g.result();
    assert_eq!((got.beats, got.rate, got.slots.len()), (1, 0, 3));
    assert!(
        got.slots
            .iter()
            .all(|s| s.peaks_us.is_empty() && s.late == 0)
    );
    assert_eq!(got.slots[0].late_total, 7);
    // A rate of 0 does not divide by zero.
    let mut g = Gather::default();
    g.add(&Beat {
        lateness: Some((0, vec![5])),
        late: None,
    });
    assert_eq!(g.result().slots[0].peaks_us, [5_000_000]);
}

#[test]
fn a_histogram_bins_each_peak_once() {
    let edges = [250, 500, 1_000];
    assert_eq!(histogram(&[], &edges), [0, 0, 0, 0]);
    assert_eq!(
        histogram(&[0, 250, 251, 500, 999, 1_000, 1_001, 40_000], &edges),
        [2, 2, 2, 2]
    );
    assert_eq!(histogram(&[3, 9], &[]), [2]);
}
