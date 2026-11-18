// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The bridge-figures body's codec (dstack #391, version 2 since #419):
//! every field at its offset for the independent walker and at both of its
//! bounds, a version 1 body still read, the length, version, cause and
//! driver-name refusals, and the driver name's cut.

use super::*;

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
    // The reader takes driver_len bytes and ignores the room's rest (the
    // walker, stricter, refuses it).
    let mut body = figures_body(&distinct());
    body[0xbf] = 0x7f;
    assert_eq!(parse_figures(&body), Ok(distinct()));
    assert!(walk::reply(&datagram(&body)).is_err());
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
