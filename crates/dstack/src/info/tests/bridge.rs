// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack's bridge-figures read (dstack #391, version 2 since #419): the
//! request's opcode, the codec's every field and bound (read back with the
//! independent walker, `tests/conformance/bridge_figures.rs`, never only
//! with our parser), a version 1 body still read, the driver name's cut,
//! the reply's bound and sequence, the slot, the share messages, and the
//! server answering the asker alone (a member's reply through its
//! manager).

use super::*;

use crate::info::bridge::{
    BridgeFigures, DRIVER_MAX, FIGURES_LEN, FIGURES_LEN_V2, FiguresError, LATE_BIN_KEYS,
    OwnFigures, REPLY_OPCODE, UnderrunCause, driver_cut, figures_body, parse_figures,
};

#[path = "../../../tests/conformance/bridge_figures.rs"]
mod walk;

const REQUEST: [u8; 8] = [0x07, 0x31, 0xd5, 0xb1, 0, 0, 0, 0];

/// Figures whose every field differs from every other in every byte (odd
/// bytes, no two fields alike), so a field written at another's offset
/// shows.
fn distinct() -> BridgeFigures {
    BridgeFigures {
        sequence: 0x0305_0709,
        run_start_unix_ms: 0x0b0d_0f11_1315_1719,
        to_asio_target_us: 0x1b1d_1f21,
        from_asio_target_us: 0x2325_2729,
        to_asio_converter_delay_us: 0x2b2d_2f31,
        from_asio_converter_delay_us: 0x3335_3739,
        to_asio_underruns: 0x3b3d_3f41_4345_4749,
        from_asio_underruns: 0x4b4d_4f51_5355_5759,
        to_asio_cause: UnderrunCause::Driver,
        from_asio_cause: UnderrunCause::Unknown,
        late_callbacks: 0x5b5d_5f61_6365_6769,
        late_bins: std::array::from_fn(|k| {
            let b = 0x6b + 16 * k as u64;
            u64::from_be_bytes(std::array::from_fn(|i| (b + 2 * i as u64) as u8))
        }),
        asio_to_dante_us: Some(0xebed_eff1),
        dante_to_asio_us: Some(0xf3f5_f7f9),
        // 0x24 bytes, a length no other field's byte holds (theirs are odd).
        driver: Some("ASIO Fireface USB \u{2014} kanál 1+2 Ž!".to_string()),
    }
}

/// `f`'s fields by the walker's keys, as `f` holds them.
fn keyed(f: &BridgeFigures) -> Vec<(&'static str, u64)> {
    let mut v = vec![
        ("version", 2),
        ("to_asio_cause", u64::from(f.to_asio_cause.code())),
        ("from_asio_cause", u64::from(f.from_asio_cause.code())),
        ("sequence", u64::from(f.sequence)),
        ("run_start_unix_ms", f.run_start_unix_ms),
        ("to_asio_target_us", u64::from(f.to_asio_target_us)),
        ("from_asio_target_us", u64::from(f.from_asio_target_us)),
        (
            "to_asio_converter_delay_us",
            u64::from(f.to_asio_converter_delay_us),
        ),
        (
            "from_asio_converter_delay_us",
            u64::from(f.from_asio_converter_delay_us),
        ),
        ("to_asio_underruns", f.to_asio_underruns),
        ("from_asio_underruns", f.from_asio_underruns),
        ("late_callbacks", f.late_callbacks),
    ];
    for (k, n) in LATE_BIN_KEYS.iter().zip(f.late_bins) {
        v.push((k, n));
    }
    let driver = f.driver.as_deref().map_or("", driver_cut);
    v.push((
        "asio_to_dante_us",
        u64::from(f.asio_to_dante_us.unwrap_or(0)),
    ));
    v.push((
        "dante_to_asio_us",
        u64::from(f.dante_to_asio_us.unwrap_or(0)),
    ));
    v.push(("driver_len", driver.len() as u64));
    v
}

/// `f`'s driver name as the walker reads it (empty when not known).
fn named(f: &BridgeFigures) -> String {
    f.driver.as_deref().map_or("", driver_cut).to_string()
}

/// A reply datagram for `body` as a device sends it (the header the
/// builder writes).
fn datagram(body: &[u8]) -> Vec<u8> {
    let mut d = header(0xffff, 32 + body.len(), 9, REPLY_OPCODE);
    d.extend_from_slice(body);
    d
}

/// Figures with every field at `v` (each field saturated to its width; at
/// 0 the latencies and the driver not known, else a 63-byte name).
fn all_at(v: u64) -> BridgeFigures {
    let w = u32::try_from(v).unwrap_or(u32::MAX);
    let known = (v != 0).then_some(w);
    BridgeFigures {
        sequence: w,
        run_start_unix_ms: v,
        to_asio_target_us: w,
        from_asio_target_us: w,
        to_asio_converter_delay_us: w,
        from_asio_converter_delay_us: w,
        to_asio_underruns: v,
        from_asio_underruns: v,
        to_asio_cause: UnderrunCause::None,
        from_asio_cause: UnderrunCause::None,
        late_callbacks: v,
        late_bins: [v; 8],
        asio_to_dante_us: known,
        dante_to_asio_us: known,
        driver: (v != 0).then(|| "é".repeat(31) + "x"),
    }
}

#[test]
fn every_field_sits_at_its_offset_for_the_independent_walker() {
    let f = distinct();
    let body = figures_body(&f);
    assert_eq!(body.len(), FIGURES_LEN_V2);
    let walked = walk::reply(&datagram(&body)).expect("the walker reads our reply");
    assert_eq!(walked.fields, keyed(&f));
    assert_eq!(walked.driver, named(&f));
    assert_eq!(walked.driver.len(), 0x24);
    assert_eq!(parse_figures(&body), Ok(f));
    // The keys are dasio's status names, the walker's too.
    let names: Vec<&str> = walk::FIELDS[12..20].iter().map(|f| f.0).collect();
    assert_eq!(names, LATE_BIN_KEYS);
}

#[test]
fn every_field_reads_back_at_both_of_its_bounds() {
    for v in [0, 1, u64::MAX - 1, u64::MAX] {
        let mut f = all_at(v);
        for cause in [UnderrunCause::None, UnderrunCause::Unknown] {
            f.to_asio_cause = cause;
            f.from_asio_cause = cause;
            let body = figures_body(&f);
            assert_eq!(parse_figures(&body), Ok(f.clone()), "{v:x}");
            let walked = walk::reply(&datagram(&body)).expect("walked");
            assert_eq!(walked.fields, keyed(&f), "{v:x}");
            assert_eq!(walked.driver, named(&f), "{v:x}");
        }
    }
}

#[test]
fn each_cause_has_its_code_both_ways() {
    let all = [
        (UnderrunCause::None, 0),
        (UnderrunCause::Bridge, 1),
        (UnderrunCause::Driver, 2),
        (UnderrunCause::Unknown, 3),
    ];
    for (cause, code) in all {
        assert_eq!(cause.code(), code);
        assert_eq!(UnderrunCause::of_code(code), Some(cause));
        let mut f = distinct();
        f.to_asio_cause = cause;
        f.from_asio_cause = all[(usize::from(code) + 1) % 4].0;
        assert_eq!(parse_figures(&figures_body(&f)), Ok(f.clone()));
    }
    for code in [4, 5, 0x80, 0xff] {
        assert_eq!(UnderrunCause::of_code(code), None);
    }
    assert_eq!(UnderrunCause::default(), UnderrunCause::None);
}

#[test]
fn a_body_of_another_length_is_refused() {
    let body = figures_body(&distinct());
    for n in [0, 1, FIGURES_LEN, FIGURES_LEN + 1, FIGURES_LEN_V2 - 1] {
        assert_eq!(parse_figures(&body[..n]), Err(FiguresError::Length(n)));
    }
    let mut long = body.to_vec();
    long.push(0);
    assert_eq!(
        parse_figures(&long),
        Err(FiguresError::Length(FIGURES_LEN_V2 + 1))
    );
    assert!(walk::reply(&datagram(&long)).is_err());
    assert!(walk::reply(&datagram(&body[..FIGURES_LEN_V2 - 1])).is_err());
    assert!(walk::reply(&datagram(&body[..FIGURES_LEN])).is_err());
    // A version 1 body takes version 1's length only.
    let mut v1 = version_1(&distinct());
    assert!(parse_figures(&v1).is_ok());
    for n in [FIGURES_LEN - 1, 2] {
        assert_eq!(parse_figures(&v1[..n]), Err(FiguresError::Length(n)));
    }
    v1.push(0);
    assert_eq!(
        parse_figures(&v1),
        Err(FiguresError::Length(FIGURES_LEN + 1))
    );
    let mut as_v1 = body;
    as_v1[0..2].copy_from_slice(&[0, 1]);
    assert_eq!(
        parse_figures(&as_v1),
        Err(FiguresError::Length(FIGURES_LEN_V2))
    );
}

#[test]
fn another_version_is_refused() {
    for (bytes, v) in [([0, 0], 0), ([0, 3], 3), ([1, 0], 0x100), ([2, 2], 0x202)] {
        let mut body = figures_body(&distinct());
        body[0..2].copy_from_slice(&bytes);
        assert_eq!(parse_figures(&body), Err(FiguresError::Version(v)));
        assert!(walk::reply(&datagram(&body)).is_err());
        let mut v1 = version_1(&distinct());
        v1[0..2].copy_from_slice(&bytes);
        assert_eq!(parse_figures(&v1), Err(FiguresError::Version(v)));
    }
}

/// A version 1 body of `f` (dstack #391), written byte by byte from the
/// walker's own offset table (its first 20 rows, the version 1 layout),
/// never by our writer.
fn version_1(f: &BridgeFigures) -> Vec<u8> {
    let mut body = vec![0u8; FIGURES_LEN];
    let mut values = keyed(f);
    values[0].1 = 1;
    for (&(key, at, width), (k, v)) in walk::FIELDS[..20].iter().zip(values) {
        assert_eq!(key, k);
        for i in 0..width {
            body[at + i] = (v >> (8 * (width - 1 - i))) as u8;
        }
    }
    body
}

#[test]
fn a_version_1_body_still_reads_with_the_new_fields_none() {
    for f in [distinct(), all_at(u64::MAX), all_at(0)] {
        let want = BridgeFigures {
            asio_to_dante_us: None,
            dante_to_asio_us: None,
            driver: None,
            ..f.clone()
        };
        assert_eq!(parse_figures(&version_1(&f)), Ok(want));
    }
    // A cause past 3 is refused in a version 1 body too.
    let mut v1 = version_1(&distinct());
    v1[3] = 9;
    assert_eq!(parse_figures(&v1), Err(FiguresError::Cause(9)));
}

#[test]
fn a_zero_latency_and_an_empty_driver_read_as_not_known() {
    let mut f = distinct();
    f.asio_to_dante_us = None;
    f.dante_to_asio_us = None;
    f.driver = None;
    let body = figures_body(&f);
    assert!(
        body[0x78..].iter().all(|&b| b == 0),
        "{:02x?}",
        &body[0x78..]
    );
    assert_eq!(parse_figures(&body), Ok(f.clone()));
    // A 0 written as known, an empty name too, read back as not known.
    for (to, from) in [(Some(0), Some(7)), (Some(7), Some(0))] {
        let mut g = f.clone();
        g.asio_to_dante_us = to;
        g.dante_to_asio_us = from;
        g.driver = Some(String::new());
        let read = parse_figures(&figures_body(&g)).unwrap();
        assert_eq!(
            (read.asio_to_dante_us, read.dante_to_asio_us, read.driver),
            (to.filter(|&v| v != 0), from.filter(|&v| v != 0), None)
        );
    }
}

#[test]
fn the_driver_name_is_cut_at_a_char_boundary_to_63_bytes() {
    let ascii = "a".repeat(70);
    assert_eq!(driver_cut(&ascii), &ascii[..DRIVER_MAX]);
    assert_eq!(driver_cut(&ascii[..DRIVER_MAX]), &ascii[..DRIVER_MAX]);
    assert_eq!(
        driver_cut(&ascii[..DRIVER_MAX - 1]),
        &ascii[..DRIVER_MAX - 1]
    );
    assert_eq!(driver_cut(""), "");
    // A two-byte char across byte 63: cut before it.
    let two = format!("{}é", "a".repeat(62));
    assert_eq!(driver_cut(&two), &two[..62]);
    // A four-byte char over bytes 61..65: cut before it.
    let four = format!("{}\u{1F3B5}tail", "a".repeat(61));
    assert_eq!(driver_cut(&four), &four[..61]);
    // A four-byte char ending at byte 63 stays.
    let whole = format!("{}\u{1F3B5}b", "a".repeat(59));
    assert_eq!(driver_cut(&whole), &whole[..63]);
    // Written cut, read back cut, by the walker too.
    for name in [ascii.as_str(), two.as_str(), four.as_str(), whole.as_str()] {
        let f = BridgeFigures {
            driver: Some(name.to_string()),
            ..distinct()
        };
        let body = figures_body(&f);
        let read = parse_figures(&body).unwrap();
        assert_eq!(read.driver.as_deref(), Some(driver_cut(name)));
        let walked = walk::reply(&datagram(&body)).expect("walked");
        assert_eq!(walked.driver, driver_cut(name));
        assert_eq!(walked.get("driver_len"), driver_cut(name).len() as u64);
    }
}

#[test]
fn a_driver_name_over_63_bytes_or_no_utf_8_is_refused() {
    for n in [64u8, 0x80, 0xff] {
        let mut body = figures_body(&distinct());
        body[0x80] = n;
        assert_eq!(parse_figures(&body), Err(FiguresError::DriverLength(n)));
        assert!(walk::reply(&datagram(&body)).is_err());
    }
    // 63 is the most: a name filling the room reads.
    let mut body = figures_body(&distinct());
    body[0x80] = 63;
    body[0x81..].fill(b'z');
    assert_eq!(
        parse_figures(&body).unwrap().driver,
        Some("z".repeat(DRIVER_MAX))
    );
    // A cut UTF-8 sequence (the first byte of "é") is no text.
    let mut body = figures_body(&distinct());
    body[0x80] = 1;
    body[0x81..].fill(0);
    body[0x81] = 0xc3;
    assert_eq!(parse_figures(&body), Err(FiguresError::DriverText));
    assert!(walk::reply(&datagram(&body)).is_err());
}

#[test]
fn a_cause_byte_past_3_is_refused_on_either_side() {
    for (at, code) in [(2, 4), (3, 4), (2, 0xff), (3, 0x83)] {
        let mut body = figures_body(&distinct());
        body[at] = code;
        assert_eq!(parse_figures(&body), Err(FiguresError::Cause(code)));
        assert!(walk::reply(&datagram(&body)).is_err());
    }
}

#[test]
fn the_request_is_known_by_its_opcode_whatever_its_second_byte() {
    for second in [0x00, 0x31, 0x2a, 0xff] {
        let mut op = REQUEST;
        op[1] = second;
        assert_eq!(
            parse_request(&asking(op)).unwrap().kind,
            Kind::BridgeFigures
        );
        assert!(!never_answered(&asking(op)));
    }
    // Every other byte is the request's own.
    for (at, v) in [(0, 0x08), (2, 0xd4), (2, 0x00), (3, 0xb0), (3, 0xb2)] {
        let mut op = REQUEST;
        op[at] = v;
        assert_eq!(parse_request(&asking(op)).unwrap().kind, Kind::Unknown);
    }
    for at in 4..8 {
        let mut op = REQUEST;
        op[at] = 1;
        assert_eq!(
            parse_request(&asking(op)).unwrap().kind,
            Kind::Unknown,
            "{at}"
        );
    }
}

#[test]
fn a_device_without_figures_answers_nothing_and_uses_no_sequence_or_bound() {
    let mut mc = InfoBuilder::new(identity());
    let mut limit = Limit::default();
    let now = Instant::now();
    let empty = FigureSlot::started_at(1);
    for slot in [None, Some(&empty)] {
        for _ in 0..20 {
            assert_eq!(
                mc.bridge_within(&mut limit, now, &asking(REQUEST), slot),
                None
            );
        }
    }
    // The info group's path never answers it either.
    assert_eq!(mc.answer(&asking(REQUEST)), None);
    assert_eq!(mc.answer_within(&mut limit, now, &asking(REQUEST)), None);
    // No packet sequence was used, and the bound is whole: ten replies
    // with figures now.
    assert_eq!(mc.board_info()[4..6], [0, 1]);
    let held = bridged();
    for n in 1..=10 {
        let p = mc
            .bridge_within(&mut limit, now, &asking(REQUEST), Some(&held))
            .unwrap_or_else(|| panic!("reply {n}"));
        assert_eq!(walk::reply(&p).unwrap().get("sequence"), n);
    }
}

/// A slot whose source counts its reads (or panics, `panics`).
fn counted(panics: bool) -> (FigureSlot, Arc<std::sync::atomic::AtomicU64>) {
    let slot = FigureSlot::started_at(1);
    let reads = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let r = Arc::clone(&reads);
    slot.set_source(move || {
        assert!(!panics, "figures read for another request");
        r.fetch_add(1, Ordering::Relaxed);
        OwnFigures::default()
    });
    (slot, reads)
}

#[test]
fn other_datagrams_never_read_the_figures() {
    let mut mc = InfoBuilder::new(identity());
    let mut limit = Limit::default();
    let now = Instant::now();
    let (never, _) = counted(true);
    assert_eq!(
        mc.bridge_within(&mut limit, now, &captured(), Some(&never)),
        None
    );
    assert_eq!(
        mc.bridge_within(&mut limit, now, &captured()[..31], Some(&never)),
        None
    );
}

/// A request over the bound reads no figures: a flood takes the runner's
/// lock at most ten times a second.
#[test]
fn a_request_over_the_bound_reads_no_figures() {
    let mut mc = InfoBuilder::new(identity());
    let mut limit = Limit::default();
    let now = Instant::now();
    let (slot, reads) = counted(false);
    let replies = (0..25)
        .filter(|_| {
            mc.bridge_within(&mut limit, now, &asking(REQUEST), Some(&slot))
                .is_some()
        })
        .count();
    assert_eq!((replies, reads.load(Ordering::Relaxed)), (10, 10));
}

#[test]
fn a_reply_carries_the_figures_its_own_header_and_a_sequence_from_1() {
    let mut mc = InfoBuilder::new(identity());
    let mut limit = Limit::default();
    let now = Instant::now();
    let held = bridged();
    let first = mc
        .bridge_within(&mut limit, now, &asking(REQUEST), Some(&held))
        .expect("a reply");
    assert_eq!(
        first[..32],
        header(0xffff, 32 + FIGURES_LEN_V2, 1, REPLY_OPCODE)
    );
    let walked = walk::reply(&first).expect("walked");
    assert_eq!(walked.device_id, identity().device_id);
    let want = BridgeFigures {
        sequence: 1,
        ..distinct()
    };
    assert_eq!(walked.fields, keyed(&want));
    // The next reply: sequence 2, and the packet sequence moves on with
    // every other packet.
    assert_eq!(mc.board_info()[4..6], [0, 2]);
    let second = mc
        .bridge_within(&mut limit, now, &asking(REQUEST), Some(&held))
        .expect("a reply");
    assert_eq!(second[4..6], [0, 3]);
    assert_eq!(walk::reply(&second).unwrap().get("sequence"), 2);
}

#[test]
fn the_sequence_wraps_after_u32_max() {
    let mut mc = InfoBuilder::new(identity());
    mc.bridge_replies = u32::MAX - 1;
    let mut limit = Limit::default();
    let now = Instant::now();
    let held = bridged();
    let seqs: Vec<u64> = (0..3)
        .map(|_| {
            let p = mc
                .bridge_within(&mut limit, now, &asking(REQUEST), Some(&held))
                .unwrap();
            walk::reply(&p).unwrap().get("sequence")
        })
        .collect();
    assert_eq!(seqs, [u64::from(u32::MAX), 0, 1]);
}

/// The server asks both paths for every datagram: the group's bound never
/// takes a bridge reply's room, and a burst over ten a second is cut.
#[test]
fn ten_bridge_replies_a_second_and_the_group_replies_keep_their_own_bound() {
    let mut mc = InfoBuilder::new(identity());
    let mut limit = Limit::default();
    let t0 = Instant::now();
    let held = bridged();
    let mut replies = 0;
    for _ in 0..15 {
        assert_eq!(mc.answer_within(&mut limit, t0, &asking(REQUEST)), None);
        replies += usize::from(
            mc.bridge_within(&mut limit, t0, &asking(REQUEST), Some(&held))
                .is_some(),
        );
    }
    assert_eq!(replies, 10);
    // The board info still has its ten.
    let boards = (0..12)
        .filter(|_| mc.answer_within(&mut limit, t0, &captured()).is_some())
        .count();
    assert_eq!(boards, 10);
    let later = t0 + limit::SPAN;
    assert!(
        mc.bridge_within(&mut limit, later, &asking(REQUEST), Some(&held))
            .is_some()
    );
    let report = limit
        .flush(t0 + limit::REPORT_EVERY)
        .expect("drops counted");
    assert_eq!(
        report.to_string(),
        "0x61 board info 2, 0xd5b1 bridge figures 5"
    );
}

#[test]
fn a_slot_answers_only_once_a_bridge_set_its_source() {
    let slot = FigureSlot::started_at(1_700_000_000_123);
    assert_eq!(slot.figures(), None);
    assert!(!slot.has_source());
    slot.set_underruns(4, 5, UnderrunCause::Bridge, UnderrunCause::Driver);
    assert_eq!(slot.figures(), None, "underruns alone answer nothing");
    let late = Arc::new(std::sync::atomic::AtomicU64::new(3));
    let seen = Arc::clone(&late);
    slot.set_source(move || OwnFigures {
        to_asio_target_us: 4000,
        from_asio_target_us: 1333,
        to_asio_converter_delay_us: 250,
        from_asio_converter_delay_us: 167,
        late_callbacks: seen.load(Ordering::Relaxed),
        late_bins: [1, 2, 3, 4, 5, 6, 7, 8],
    });
    assert!(slot.has_source());
    let want = BridgeFigures {
        sequence: 0,
        run_start_unix_ms: 1_700_000_000_123,
        to_asio_target_us: 4000,
        from_asio_target_us: 1333,
        to_asio_converter_delay_us: 250,
        from_asio_converter_delay_us: 167,
        to_asio_underruns: 4,
        from_asio_underruns: 5,
        to_asio_cause: UnderrunCause::Bridge,
        from_asio_cause: UnderrunCause::Driver,
        late_callbacks: 3,
        late_bins: [1, 2, 3, 4, 5, 6, 7, 8],
        asio_to_dante_us: None,
        dante_to_asio_us: None,
        driver: None,
    };
    assert_eq!(slot.figures(), Some(want));
    // Read at each request: the source's change and the app's last
    // hand-over show.
    late.store(9, Ordering::Relaxed);
    slot.set_underruns(6, 7, UnderrunCause::None, UnderrunCause::Unknown);
    let f = slot.figures().unwrap();
    assert_eq!(
        (f.late_callbacks, f.to_asio_underruns, f.from_asio_underruns),
        (9, 6, 7)
    );
    assert_eq!(
        (f.to_asio_cause, f.from_asio_cause),
        (UnderrunCause::None, UnderrunCause::Unknown)
    );
    assert!(format!("{slot:?}").contains("source: true"));
}

/// The app's through-latency and driver name (dstack #419) reach the
/// slot's figures and the reply, the last hand-over each; the underruns
/// stay as handed.
#[test]
fn the_path_and_the_driver_reach_the_figures_and_the_reply() {
    let slot = FigureSlot::started_at(1);
    slot.set_source(OwnFigures::default);
    slot.set_underruns(4, 5, UnderrunCause::Bridge, UnderrunCause::Driver);
    let read = |slot: &FigureSlot| {
        let f = slot.figures().unwrap();
        (f.asio_to_dante_us, f.dante_to_asio_us, f.driver)
    };
    assert_eq!(read(&slot), (None, None, None));
    slot.set_path(5_333, 4_667);
    slot.set_driver("ASIO MADIface USB");
    assert_eq!(
        read(&slot),
        (Some(5_333), Some(4_667), Some("ASIO MADIface USB".into()))
    );
    let mut mc = InfoBuilder::new(identity());
    let reply = mc
        .bridge_within(
            &mut Limit::default(),
            Instant::now(),
            &asking(REQUEST),
            Some(&slot),
        )
        .expect("a reply");
    let walked = walk::reply(&reply).expect("walked");
    assert_eq!(
        (
            walked.get("asio_to_dante_us"),
            walked.get("dante_to_asio_us")
        ),
        (5_333, 4_667)
    );
    assert_eq!(walked.driver, "ASIO MADIface USB");
    assert_eq!(
        (
            walked.get("to_asio_underruns"),
            walked.get("from_asio_cause")
        ),
        (4, 2)
    );
    // Each side not known on its own; a long name kept cut; an empty
    // name not known.
    slot.set_path(0, 4_000);
    let long = format!("{}é", "d".repeat(62));
    slot.set_driver(&long);
    assert_eq!(read(&slot), (None, Some(4_000), Some("d".repeat(62))));
    slot.set_path(3_000, 0);
    slot.set_driver("");
    assert_eq!(read(&slot), (Some(3_000), None, None));
    // The underruns were not touched by the other setters.
    let f = slot.figures().unwrap();
    assert_eq!((f.to_asio_underruns, f.from_asio_underruns), (4, 5));
    assert_eq!(
        (f.to_asio_cause, f.from_asio_cause),
        (UnderrunCause::Bridge, UnderrunCause::Driver)
    );
    // And set_underruns leaves the path and the driver.
    slot.set_driver("x");
    slot.set_underruns(6, 7, UnderrunCause::None, UnderrunCause::None);
    assert_eq!(read(&slot), (Some(3_000), None, Some("x".into())));
}

#[test]
fn a_new_slot_s_run_starts_now() {
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let slot = FigureSlot::default();
    slot.set_source(OwnFigures::default);
    let after = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let at = slot.figures().unwrap().run_start_unix_ms;
    assert!((before..=after).contains(&at), "{before} {at} {after}");
}

/// A beat with bridge figures (and no clock).
struct Bridged(Arc<FigureSlot>);

impl Beat for Bridged {
    fn records(&self) -> Option<Records> {
        None
    }

    fn bridge(&self) -> Option<&FigureSlot> {
        Some(&self.0)
    }
}

/// A slot whose source and hand-over give `distinct()`'s figures.
fn bridged() -> Arc<FigureSlot> {
    let slot = Arc::new(FigureSlot::started_at(distinct().run_start_unix_ms));
    let f = distinct();
    let own = OwnFigures {
        to_asio_target_us: f.to_asio_target_us,
        from_asio_target_us: f.from_asio_target_us,
        to_asio_converter_delay_us: f.to_asio_converter_delay_us,
        from_asio_converter_delay_us: f.from_asio_converter_delay_us,
        late_callbacks: f.late_callbacks,
        late_bins: f.late_bins,
    };
    slot.set_source(move || own);
    slot.set_underruns(
        f.to_asio_underruns,
        f.from_asio_underruns,
        f.to_asio_cause,
        f.from_asio_cause,
    );
    slot.set_path(f.asio_to_dante_us.unwrap(), f.dante_to_asio_us.unwrap());
    slot.set_driver(f.driver.as_deref().unwrap());
    slot
}

#[test]
fn the_running_device_s_beat_gives_its_slot_s_figures() {
    use crate::daemon::media::tests::{Unready, device_at, media_on};
    let dir = tempfile::tempdir().unwrap();
    let media = media_on(&device_at(dir.path()), Arc::new(Unready));
    let beat = media.beat();
    let held = Beat::bridge(&beat).expect("the device's slot");
    assert_eq!(held.figures(), None);
    let slot = media.bridge_figures();
    slot.set_source(OwnFigures::default);
    slot.set_underruns(1, 2, UnderrunCause::Bridge, UnderrunCause::None);
    let f = held.figures().expect("the slot's figures");
    assert_eq!((f.to_asio_underruns, f.from_asio_underruns), (1, 2));
    assert!(NoClock.bridge().is_none(), "a beat holds none by default");
}

#[tokio::test]
async fn the_server_answers_bridge_figures_to_the_asker_alone() {
    let (at, info_rx, beat_rx, to, sock) = loopback(NEVER).await;
    let task = tokio::spawn(serve_info(
        sock,
        InfoBuilder::new(identity()),
        to,
        Arc::new(Bridged(bridged())),
        Arc::new(Changes::default()),
        Arc::new(Requests::default()),
    ));
    recv(&info_rx).await;
    recv(&info_rx).await;
    recv(&beat_rx).await;
    let asker = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    asker.send_to(&asking(REQUEST), at).await.unwrap();
    let mut buf = [0u8; 2048];
    let (n, from) = tokio::time::timeout(BOUND, asker.recv_from(&mut buf))
        .await
        .expect("the reply to the asker")
        .unwrap();
    assert_eq!(from, at, "from the info port");
    let walked = walk::reply(&buf[..n]).expect("walked");
    let want = BridgeFigures {
        sequence: 1,
        ..distinct()
    };
    assert_eq!(walked.fields, keyed(&want));
    assert_eq!(walked.driver, named(&want));
    // Nothing went to the info group.
    let mut group = [0u8; 64];
    let to_group =
        tokio::time::timeout(Duration::from_millis(200), info_rx.recv_from(&mut group)).await;
    assert!(to_group.is_err(), "the info group heard nothing");
    task.abort();
}

#[tokio::test]
async fn a_device_without_figures_leaves_the_request_unanswered() {
    let (at, info_rx, beat_rx, to, sock) = loopback(NEVER).await;
    let task = tokio::spawn(serve_info(
        sock,
        InfoBuilder::new(identity()),
        to,
        Arc::new(NoClock),
        Arc::new(Changes::default()),
        Arc::new(Requests::default()),
    ));
    recv(&info_rx).await;
    recv(&info_rx).await;
    recv(&beat_rx).await;
    let asker = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    asker.send_to(&asking(REQUEST), at).await.unwrap();
    // A board-info request after it: its reply proves the first was
    // handled.
    asker.send_to(&captured(), at).await.unwrap();
    assert_eq!(recv(&info_rx).await[24..32], BOARD_INFO);
    let mut buf = [0u8; 64];
    let got = tokio::time::timeout(Duration::from_millis(200), asker.recv_from(&mut buf)).await;
    assert!(got.is_err(), "no reply to the asker");
    task.abort();
}

/// Several dasio instances on one PC: the request reaches a member with
/// its sender, and the member's reply goes back to that sender through the
/// manager, with the member's id; the manager (no figures) answers
/// nothing.
#[tokio::test]
async fn a_member_s_bridge_figures_go_to_the_asker_through_the_manager() {
    let (at, info_rx, beat_rx, to, sock) = loopback(NEVER).await;
    let share = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let manager_at = share.local_addr().unwrap();
    let mut b_cfg = cfg();
    b_cfg.process_id = 1;
    let b = Identity::new(&b_cfg, net());
    let manager = tokio::spawn(serve_info_with(
        sock,
        Some(share),
        InfoBuilder::new(identity()),
        to,
        Arc::new(NoClock),
        Arc::new(Changes::default()),
        Arc::new(Requests::default()),
    ));
    recv(&info_rx).await;
    recv(&info_rx).await;
    recv(&beat_rx).await;
    let member_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let member_at = member_sock.local_addr().unwrap();
    let member = tokio::spawn(serve_member(
        member_sock,
        manager_at,
        v4(at),
        InfoBuilder::new(b.clone()),
        to,
        Arc::new(Bridged(bridged())),
        Arc::new(Changes::default()),
        Arc::new(Requests::default()),
    ));
    // The member is registered once its start went out.
    recv(&info_rx).await;
    recv(&info_rx).await;
    recv(&beat_rx).await;
    let asker = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    asker.send_to(&asking(REQUEST), at).await.unwrap();
    let mut buf = [0u8; 2048];
    let (n, from) = tokio::time::timeout(BOUND, asker.recv_from(&mut buf))
        .await
        .expect("the member's reply")
        .unwrap();
    assert_eq!(from, at, "sent from the address's info port");
    let walked = walk::reply(&buf[..n]).expect("walked");
    assert_eq!(walked.device_id, b.device_id);
    assert_eq!(walked.get("sequence"), 1);
    // One reply only: the manager holds no figures.
    let again = tokio::time::timeout(Duration::from_millis(200), asker.recv_from(&mut buf)).await;
    assert!(again.is_err(), "one reply");
    // A board-info request still reaches the member as before.
    asker.send_to(&captured(), at).await.unwrap();
    let mut ids = vec![id_of(&recv(&info_rx).await), id_of(&recv(&info_rx).await)];
    ids.sort();
    let mut want = vec![identity().device_id, b.device_id];
    want.sort();
    assert_eq!(ids, want);
    // A forged hand-over from a stranger is dropped.
    let stranger = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let forged = share::Msg::SendTo(v4(asker.local_addr().unwrap()), vec![9; 40]).encode();
    stranger.send_to(&forged, manager_at).await.unwrap();
    let got = tokio::time::timeout(Duration::from_millis(200), asker.recv_from(&mut buf)).await;
    assert!(got.is_err(), "nothing sent for a stranger");
    // A forged request to the member from a stranger is never answered.
    let ask = share::Msg::Ask(v4(asker.local_addr().unwrap()), asking(REQUEST)).encode();
    stranger.send_to(&ask, member_at).await.unwrap();
    let got = tokio::time::timeout(Duration::from_millis(200), asker.recv_from(&mut buf)).await;
    assert!(got.is_err(), "a member answers its manager's relays only");
    member.abort();
    manager.abort();
}

/// A member's hand-over goes out only as a bridge reply, to an asker the
/// manager relayed lately: no process on the PC can send from the info
/// port to an address it names (a registered member here, the test).
#[tokio::test]
async fn the_manager_sends_a_member_s_reply_only_to_a_recent_asker() {
    let (at, info_rx, beat_rx, to, sock) = loopback(NEVER).await;
    let share = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let manager_at = share.local_addr().unwrap();
    let manager = tokio::spawn(serve_info_with(
        sock,
        Some(share),
        InfoBuilder::new(identity()),
        to,
        Arc::new(NoClock),
        Arc::new(Changes::default()),
        Arc::new(Requests::default()),
    ));
    recv(&info_rx).await;
    recv(&info_rx).await;
    recv(&beat_rx).await;
    let fake = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    fake.send_to(&share::Msg::Register([5; 8]).encode(), manager_at)
        .await
        .unwrap();
    let asker = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let other = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    // Registered: a board-info request reaches the fake member.
    let mut buf = [0u8; 2048];
    loop {
        asker.send_to(&captured(), at).await.unwrap();
        let (n, _) = tokio::time::timeout(BOUND, fake.recv_from(&mut buf))
            .await
            .expect("the relay")
            .unwrap();
        if share::Msg::decode(&buf[..n]) == Some(share::Msg::Request(captured())) {
            break;
        }
    }
    let reply = {
        let mut mc = InfoBuilder::new(identity());
        let mut limit = Limit::default();
        let held = bridged();
        mc.bridge_within(&mut limit, Instant::now(), &asking(REQUEST), Some(&held))
            .unwrap()
    };
    let quiet = |s: &UdpSocket| {
        let mut b = [0u8; 256];
        s.try_recv_from(&mut b).is_err()
    };
    // Before any ask: dropped.
    let to_asker = share::Msg::SendTo(v4(asker.local_addr().unwrap()), reply.clone()).encode();
    fake.send_to(&to_asker, manager_at).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(quiet(&asker), "the hand-over before the ask was dropped");
    // The asker asks; the relay reaches the fake member with its sender.
    asker.send_to(&asking(REQUEST), at).await.unwrap();
    let (n, _) = tokio::time::timeout(BOUND, fake.recv_from(&mut buf))
        .await
        .expect("the relay")
        .unwrap();
    assert_eq!(
        share::Msg::decode(&buf[..n]),
        Some(share::Msg::Ask(
            v4(asker.local_addr().unwrap()),
            asking(REQUEST)
        ))
    );
    // Now: a packet that is no bridge reply, and a reply to another
    // address, are dropped; the reply to the asker goes out.
    let bad = share::Msg::SendTo(v4(asker.local_addr().unwrap()), captured()).encode();
    fake.send_to(&bad, manager_at).await.unwrap();
    let short = share::Msg::SendTo(
        v4(asker.local_addr().unwrap()),
        reply[..reply.len() - 1].to_vec(),
    )
    .encode();
    fake.send_to(&short, manager_at).await.unwrap();
    let elsewhere = share::Msg::SendTo(v4(other.local_addr().unwrap()), reply.clone()).encode();
    fake.send_to(&elsewhere, manager_at).await.unwrap();
    fake.send_to(&to_asker, manager_at).await.unwrap();
    let (n, from) = tokio::time::timeout(BOUND, asker.recv_from(&mut buf))
        .await
        .expect("the reply")
        .unwrap();
    assert_eq!((from, &buf[..n]), (at, &reply[..]), "only the good reply");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(quiet(&asker), "nothing else to the asker");
    assert!(quiet(&other), "nothing to an address nobody asked from");
    manager.abort();
}

#[test]
fn the_manager_keeps_an_asker_for_a_second_and_at_most_32() {
    let a = |port: u16| SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 1), port);
    let mut asks = share::Asks::default();
    assert!(!asks.holds(&a(1), 0));
    asks.note(a(1), 1_000);
    assert!(asks.holds(&a(1), 1_000));
    assert!(asks.holds(&a(1), 1_000 + share::ASK_HOLD_NS - 1));
    assert!(!asks.holds(&a(1), 1_000 + share::ASK_HOLD_NS));
    assert!(!asks.holds(&a(2), 1_000));
    // Full: a new asker is not kept, a known one is renewed.
    for port in 2..=32 {
        asks.note(a(port), 2_000);
    }
    asks.note(a(99), 2_000);
    assert!(!asks.holds(&a(99), 2_000), "the 33rd");
    asks.note(a(1), 3_000);
    assert!(asks.holds(&a(1), 3_000 + share::ASK_HOLD_NS - 1));
    // Once the old ones lapse, room again.
    asks.note(a(99), 2_000 + share::ASK_HOLD_NS);
    assert!(asks.holds(&a(99), 2_000 + share::ASK_HOLD_NS));
    assert!(!asks.holds(&a(2), 2_000 + share::ASK_HOLD_NS));
}

#[test]
fn a_reply_goes_only_to_one_host_and_only_a_bridge_reply_is_handed_over() {
    let at = |a: [u8; 4], port: u16| SocketAddrV4::new(Ipv4Addr::from(a), port);
    assert!(share::is_unicast(&at([10, 0, 0, 1], 4000)));
    assert!(share::is_unicast(&at([127, 0, 0, 1], 1)));
    assert!(share::is_unicast(&at([223, 255, 255, 255], 1)));
    assert!(!share::is_unicast(&at([224, 0, 0, 231], 8702)), "multicast");
    assert!(!share::is_unicast(&at([239, 1, 1, 1], 1)), "multicast");
    assert!(
        !share::is_unicast(&at([255, 255, 255, 255], 1)),
        "broadcast"
    );
    assert!(!share::is_unicast(&at([0, 0, 0, 0], 1)), "unspecified");
    assert!(!share::is_unicast(&at([10, 0, 0, 1], 0)), "port 0");
    let mut mc = InfoBuilder::new(identity());
    let held = bridged();
    let reply = mc
        .bridge_within(
            &mut Limit::default(),
            Instant::now(),
            &asking(REQUEST),
            Some(&held),
        )
        .unwrap();
    assert!(share::is_bridge_reply(&reply));
    assert!(!share::is_bridge_reply(&reply[..reply.len() - 1]));
    // A version 1 reply (a member older than dstack #419) is still handed
    // over; no length between or around it.
    assert!(share::is_bridge_reply(&reply[..32 + FIGURES_LEN]));
    for n in [32 + FIGURES_LEN - 1, 32 + FIGURES_LEN + 1, 32, 31, 0] {
        assert!(!share::is_bridge_reply(&reply[..n]), "{n}");
    }
    let mut longer = reply.clone();
    longer.push(0);
    assert!(!share::is_bridge_reply(&longer));
    for at in 24..32 {
        let mut other = reply.clone();
        other[at] ^= 1;
        assert!(!share::is_bridge_reply(&other), "opcode byte {at}");
    }
    assert!(!share::is_bridge_reply(&mc.board_info()));
}

/// The manager relays a bridge-figures request with its sender, anything
/// else as it came.
#[test]
fn the_manager_relays_a_bridge_request_with_its_sender() {
    let asker: SocketAddr = "127.0.0.9:4321".parse().unwrap();
    assert_eq!(
        server::relay_of(&asking(REQUEST), asker),
        share::Msg::Ask(v4(asker), asking(REQUEST))
    );
    assert_eq!(
        server::relay_of(&captured(), asker),
        share::Msg::Request(captured())
    );
    assert_eq!(
        server::relay_of(&captured()[..20], asker),
        share::Msg::Request(captured()[..20].to_vec())
    );
}

/// The client asks a server on a 8700 of its own address (127.0.39.1) and
/// reads the figures; a device without them gives a timeout.
#[tokio::test]
async fn the_client_reads_a_device_s_figures_and_times_out_without() {
    let ip = Ipv4Addr::new(127, 0, 39, 1);
    let (_, info_rx, beat_rx, to, _) = loopback(NEVER).await;
    let sock = UdpSocket::bind((ip, REQUEST_PORT)).await.unwrap();
    let task = tokio::spawn(serve_info(
        sock,
        InfoBuilder::new(identity()),
        to,
        Arc::new(Bridged(bridged())),
        Arc::new(Changes::default()),
        Arc::new(Requests::default()),
    ));
    recv(&info_rx).await;
    recv(&info_rx).await;
    recv(&beat_rx).await;
    let id = identity().device_id;
    let got = tokio::task::spawn_blocking(move || {
        let s = client::unicast_socket(Ipv4Addr::LOCALHOST).unwrap();
        let read = client::bridge_figures(&s, ip, Some(id), BOUND);
        let other = client::bridge_figures(&s, ip, Some([7; 8]), Duration::from_millis(300));
        (read, other)
    })
    .await
    .unwrap();
    let f = got.0.expect("the figures");
    assert_eq!(
        f,
        BridgeFigures {
            sequence: f.sequence,
            ..distinct()
        }
    );
    assert!(f.sequence >= 1);
    assert_eq!(
        got.1,
        Err(client::InfoError::Timeout {
            ask: client::Ask::BridgeFigures
        }),
        "another device's id"
    );
    task.abort();
}
