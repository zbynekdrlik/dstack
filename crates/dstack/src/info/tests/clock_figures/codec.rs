// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The clock-figures body's codec (dstack #429): every field at its offset
//! for the independent walker and at both of its bounds, each flag at its
//! own key, the not-known values, the absent parts, each code both ways,
//! and the length, version, code and flag refusals.

use super::*;

#[test]
fn every_field_sits_at_its_offset_for_the_independent_walker() {
    let f = distinct();
    let body = figures_body(&f);
    assert_eq!(body.len(), FIGURES_LEN);
    let walked = walk::reply(&datagram(&body)).expect("the walker reads our reply");
    assert_eq!(walked.fields, keyed(&f));
    assert_eq!(parse_figures(&body), Ok(f));
}

/// The flags cannot all differ in one body: each alone, set, reads at its
/// own key and nowhere else.
#[test]
fn each_flag_alone_sits_at_its_own_key() {
    let bare = ClockFigures {
        follower: Some(FollowerFigures::default()),
        service: Some(ServiceFigures {
            poll: PollOutcome::Usable,
            reply_age_ms: Some(0),
            locked: false,
            settled: false,
            mode: 0,
            holdover: false,
            holdovers: 0,
            freq_ppb: 0,
            grandmaster: None,
        }),
        ..ClockFigures::default()
    };
    let set: [(&str, fn(&mut ClockFigures)); 8] = [
        ("ready", |f| f.ready = true),
        ("follower_locked", |f| {
            f.follower.as_mut().unwrap().locked = true
        }),
        ("follower_settled", |f| {
            f.follower.as_mut().unwrap().settled = true
        }),
        ("follower_holdover", |f| {
            f.follower.as_mut().unwrap().holdover = true
        }),
        ("service_locked", |f| {
            f.service.as_mut().unwrap().locked = true
        }),
        ("service_settled", |f| {
            f.service.as_mut().unwrap().settled = true
        }),
        ("service_holdover", |f| {
            f.service.as_mut().unwrap().holdover = true
        }),
        ("service_mode", |f| f.service.as_mut().unwrap().mode = 1),
    ];
    let quiet = keyed(&bare);
    for (key, apply) in set {
        let mut f = bare;
        apply(&mut f);
        let body = figures_body(&f);
        let walked = walk::reply(&datagram(&body)).expect("walked");
        for (k, v) in &walked.fields {
            let was = quiet.iter().find(|(q, _)| q == k).unwrap().1;
            let want = if *k == key { 1 } else { was };
            assert_eq!(*v, want, "{key} set: {k}");
        }
        assert_eq!(parse_figures(&body), Ok(f), "{key}");
    }
}

/// Every figure at each bound of its width, signed ones around 0 too.
#[test]
fn every_field_reads_back_at_both_of_its_bounds() {
    let wide = [i64::MIN + 1, -1, 0, 1, i64::MAX];
    let word = [i32::MIN, -1, 0, 1, i32::MAX];
    let count = [0, 1, u32::MAX - 1, u32::MAX];
    for i in 0..5 {
        let mut f = distinct();
        let w = f.follower.as_mut().unwrap();
        w.error_p90_ns = Some(wide[i]);
        w.offset_from_service_ns = Some(wide[4 - i]);
        w.freq_ppb = word[i];
        w.holdovers = count[i % 4];
        w.grandmaster = Some([0xff; 6]);
        let s = f.service.as_mut().unwrap();
        s.freq_ppb = word[4 - i];
        s.holdovers = count[(i + 1) % 4];
        s.reply_age_ms = Some([0, 1, u32::MAX - 1][i % 3]);
        s.mode = [0, 1, 0x7f, 0x80, 0xff][i];
        s.grandmaster = Some([0, 0, 0, 0, 0, 1]);
        f.sequence = count[(i + 2) % 4];
        let body = figures_body(&f);
        assert_eq!(parse_figures(&body), Ok(f), "{i}");
        let walked = walk::reply(&datagram(&body)).expect("walked");
        assert_eq!(walked.fields, keyed(&f), "{i}");
    }
}

/// A `None` goes on the wire as its not-known value and reads back
/// `None`; that value written as known reads `None` too.
#[test]
fn a_not_known_figure_has_its_own_value_on_the_wire() {
    let mut f = distinct();
    let w = f.follower.as_mut().unwrap();
    w.error_p90_ns = None;
    w.offset_from_service_ns = None;
    w.grandmaster = None;
    w.waiting = None;
    let s = f.service.as_mut().unwrap();
    s.reply_age_ms = None;
    s.grandmaster = None;
    let body = figures_body(&f);
    assert_eq!(
        body[0x08..0x18],
        [0x80, 0, 0, 0, 0, 0, 0, 0, 0x80, 0, 0, 0, 0, 0, 0, 0]
    );
    assert_eq!(body[0x28..0x2c], [0xff; 4]);
    assert_eq!(body[0x2c..0x38], [0; 12]);
    assert_eq!(body[0x3c], 0);
    assert_eq!(parse_figures(&body), Ok(f));
    let walked = walk::reply(&datagram(&body)).expect("walked");
    assert_eq!(walked.fields, keyed(&f));
    let mut known = f;
    known.follower.as_mut().unwrap().error_p90_ns = Some(NOT_KNOWN_NS);
    known.service.as_mut().unwrap().reply_age_ms = Some(NO_REPLY_MS);
    assert_eq!(parse_figures(&figures_body(&known)), Ok(f));
}

/// No follower, no service: their fields all 0 on the wire, `None` read;
/// the reader does not look at an absent part's bytes.
#[test]
fn an_absent_part_is_zeros_and_reads_none() {
    for (follower, service) in [(false, false), (true, false), (false, true)] {
        let mut f = distinct();
        if !follower {
            f.follower = None;
        }
        if !service {
            f.service = None;
        }
        let body = figures_body(&f);
        let walked = walk::reply(&datagram(&body)).expect("the walker's zero rule");
        assert_eq!(walked.fields, keyed(&f));
        assert_eq!(parse_figures(&body), Ok(f));
        let part = |k: &str| {
            (!follower && k.starts_with("follower")) || (!service && k.starts_with("service_"))
        };
        for (k, v) in walked.fields {
            if part(k) {
                assert_eq!(v, 0, "{k}");
            }
        }
    }
    // Junk in an absent part: ours reads past it (the walker refuses it).
    let none = ClockFigures {
        follower: None,
        service: None,
        ..distinct()
    };
    let mut body = figures_body(&none);
    for o in [0x08, 0x10, 0x18, 0x1c, 0x2c, 0x39, 0x3a, 0x3b, 0x3c] {
        body[o] = 9;
    }
    for o in [0x20, 0x24, 0x28, 0x32, 0x3e, 0x3f, 0x40, 0x41] {
        body[o] = 9;
    }
    assert_eq!(parse_figures(&body), Ok(none));
    assert!(walk::reply(&datagram(&body)).is_err());
}

#[test]
fn each_code_has_its_byte_both_ways() {
    let sources = [
        (TimeSource::None, 0),
        (TimeSource::Internal, 1),
        (TimeSource::Service, 2),
        (TimeSource::System, 3),
    ];
    for (s, code) in sources {
        assert_eq!((s.code(), TimeSource::of_code(code)), (code, Some(s)));
        let f = ClockFigures {
            source: s,
            ..distinct()
        };
        assert_eq!(parse_figures(&figures_body(&f)), Ok(f));
    }
    assert_eq!(TimeSource::default(), TimeSource::None);
    let waits = [
        (None, 0),
        (Some(Waiting::Stopped), 1),
        (Some(Waiting::NoPtp), 2),
        (Some(Waiting::NotLocked), 3),
        (Some(Waiting::NotSettled), 4),
        (Some(Waiting::WaitsToAgree), 5),
    ];
    for (w, code) in waits {
        assert_eq!((waiting_code(w), waiting_of_code(code)), (code, Some(w)));
        let mut f = distinct();
        f.follower.as_mut().unwrap().waiting = w;
        assert_eq!(parse_figures(&figures_body(&f)), Ok(f));
    }
    let polls = [
        (None, 0),
        (Some(PollOutcome::Usable), 1),
        (Some(PollOutcome::NoReply), 2),
        (Some(PollOutcome::NotLocked), 3),
        (Some(PollOutcome::NoDateState), 4),
        (Some(PollOutcome::StepNear), 5),
        (Some(PollOutcome::Slewing), 6),
    ];
    for (p, code) in polls {
        assert_eq!((poll_code(p), poll_of_code(code)), (code, Some(p)));
        if let Some(p) = p {
            let mut f = distinct();
            f.service.as_mut().unwrap().poll = p;
            assert_eq!(parse_figures(&figures_body(&f)), Ok(f));
        }
    }
    for code in [4, 5, 0x80, 0xff] {
        assert_eq!(TimeSource::of_code(code), None);
    }
    for code in [6, 7, 0xff] {
        assert_eq!(waiting_of_code(code), None);
    }
    for code in [7, 8, 0xff] {
        assert_eq!(poll_of_code(code), None);
    }
}

/// A code past its table, or a flag past 1, is refused by both readers,
/// naming its byte.
#[test]
fn a_code_or_a_flag_no_version_writes_is_refused() {
    let cases: [(usize, u8, &str); 12] = [
        (0x02, 4, "source"),
        (0x03, 2, "flag"),
        (0x38, 2, "flag"),
        (0x39, 2, "flag"),
        (0x3a, 0x80, "flag"),
        (0x3b, 0xff, "flag"),
        (0x3c, 6, "wait reason"),
        (0x3d, 7, "poll outcome"),
        (0x3e, 2, "flag"),
        (0x3f, 2, "flag"),
        (0x40, 2, "flag"),
        (0x3d, 0xff, "poll outcome"),
    ];
    for (at, byte, what) in cases {
        let mut body = figures_body(&distinct());
        body[at] = byte;
        assert_eq!(
            parse_figures(&body),
            Err(ClockFiguresError::Code { at, byte, what }),
            "{at:#x}"
        );
        assert!(walk::reply(&datagram(&body)).is_err(), "{at:#x}");
    }
    // The mode is dsync's byte as it comes: any byte reads.
    let mut body = figures_body(&distinct());
    body[0x41] = 0xff;
    assert_eq!(parse_figures(&body).unwrap().service.unwrap().mode, 0xff);
}

#[test]
fn a_body_of_another_length_is_refused() {
    let body = figures_body(&distinct());
    for n in [0, 1, 2, 3, FIGURES_LEN - 1] {
        assert_eq!(parse_figures(&body[..n]), Err(ClockFiguresError::Length(n)));
        assert!(walk::reply(&datagram(&body[..n])).is_err());
    }
    let mut long = body.to_vec();
    long.push(0);
    assert_eq!(
        parse_figures(&long),
        Err(ClockFiguresError::Length(FIGURES_LEN + 1))
    );
    assert!(walk::reply(&datagram(&long)).is_err());
}

#[test]
fn another_version_is_refused() {
    for (bytes, v) in [
        ([0, 0], 0),
        ([0, 2], 2),
        ([1, 1], 0x101),
        ([0xff, 0xff], 0xffff),
    ] {
        let mut body = figures_body(&distinct());
        body[0..2].copy_from_slice(&bytes);
        assert_eq!(parse_figures(&body), Err(ClockFiguresError::Version(v)));
        assert!(walk::reply(&datagram(&body)).is_err());
        // The version is judged before the length.
        assert_eq!(
            parse_figures(&body[..10]),
            Err(ClockFiguresError::Version(v))
        );
    }
}

#[test]
fn the_reply_age_is_whole_ms_and_never_reads_as_no_reply() {
    let s = 1_000_000_000;
    assert_eq!(age_ms(5 * s, 5 * s), 0);
    assert_eq!(age_ms(5 * s, 5 * s + 999_999), 0);
    assert_eq!(age_ms(5 * s, 5 * s + 1_000_000), 1);
    assert_eq!(age_ms(5 * s, 7 * s + 2_500_000), 2_002);
    // A reply stamped ahead of now (never on one monotonic clock): 0.
    assert_eq!(age_ms(5 * s, 4 * s), 0);
    // Past u32 ms, and at the extremes: one under "no reply".
    let top = i64::from(u32::MAX) * 1_000_000;
    assert_eq!(age_ms(0, top - 1_000_000), u32::MAX - 1);
    assert_eq!(age_ms(0, top), u32::MAX - 1);
    assert_eq!(age_ms(0, top - 2_000_000), u32::MAX - 2);
    assert_eq!(age_ms(i64::MIN, i64::MAX), u32::MAX - 1);
}
