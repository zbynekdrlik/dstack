// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack's clock-figures read (dstack #429): the request's opcode, the
//! reply's bound and sequence, the figures from the clock's stats, the
//! share messages, and the server answering the asker alone (a member's
//! reply through its manager), each reply read back with the independent
//! walker (`tests/conformance/clock_figures.rs`), never only with our
//! parser. The body's codec is `clock_figures/codec.rs`.

use super::*;

use crate::clock::linked::{Source, Waiting};
use crate::clock::{ClockCounts, ClockStats, FollowerCounts, PollOutcome, Reading, Sample};
use crate::info::clock_figures::{
    ClockFigures, ClockFiguresError, FIGURES_LEN, FollowerFigures, NO_REPLY_MS, NOT_KNOWN_NS,
    REPLY_OPCODE, ServiceFigures, TimeSource, age_ms, figures_body, figures_of, parse_figures,
    poll_code, poll_of_code, waiting_code, waiting_of_code,
};

#[path = "../../../tests/conformance/clock_figures.rs"]
mod walk;

mod codec;

const REQUEST: [u8; 8] = [0x07, 0x31, 0xd5, 0xb3, 0, 0, 0, 0];

/// Figures whose every multi-byte field differs from every other in every
/// byte (odd bytes, no two fields alike), so a field written at another's
/// offset shows; the flags and codes differ where they can (each flag alone:
/// `codec.rs`).
fn distinct() -> ClockFigures {
    ClockFigures {
        sequence: 0x0305_0709,
        source: TimeSource::Internal,
        ready: true,
        follower: Some(FollowerFigures {
            locked: true,
            settled: false,
            holdover: true,
            waiting: Some(Waiting::NotSettled),
            grandmaster: Some([0x0b, 0x0d, 0x0f, 0x11, 0x13, 0x15]),
            holdovers: 0x1719_1b1d,
            freq_ppb: 0x1f21_2325,
            error_p90_ns: Some(0x2729_2b2d_2f31_3335),
            offset_from_service_ns: Some(i64::from_be_bytes([
                0xc7, 0xc9, 0xcb, 0xcd, 0xcf, 0xd1, 0xd3, 0xd5,
            ])),
        }),
        service: Some(ServiceFigures {
            poll: PollOutcome::StepNear,
            reply_age_ms: Some(0x4749_4b4d),
            locked: false,
            settled: true,
            mode: 0x4f,
            holdover: false,
            holdovers: 0x5153_5557,
            freq_ppb: i32::from_be_bytes([0xd9, 0xdb, 0xdd, 0xdf]),
            grandmaster: Some([0x59, 0x5b, 0x5d, 0x5f, 0x61, 0x63]),
        }),
    }
}

/// `f`'s fields by the walker's keys and in its order, as raw big-endian
/// values (a signed one as its two's complement), from `f` as it holds
/// them.
fn keyed(f: &ClockFigures) -> Vec<(&'static str, u64)> {
    let uuid =
        |u: Option<[u8; 6]>| u.map_or(0, |b| b.iter().fold(0u64, |v, &x| v * 256 + u64::from(x)));
    let ns = |v: Option<i64>| v.unwrap_or(i64::MIN) as u64;
    let w = f.follower.unwrap_or_default();
    let on = f.follower.is_some();
    let s = f.service;
    let flag = |b: bool| u64::from(b);
    vec![
        ("version", 1),
        ("source", u64::from(f.source.code())),
        ("ready", flag(f.ready)),
        ("sequence", u64::from(f.sequence)),
        (
            "follower_error_p90_ns",
            if on { ns(w.error_p90_ns) } else { 0 },
        ),
        (
            "follower_offset_from_service_ns",
            if on { ns(w.offset_from_service_ns) } else { 0 },
        ),
        ("follower_freq_ppb", u64::from(w.freq_ppb as u32)),
        ("follower_holdovers", u64::from(w.holdovers)),
        (
            "service_freq_ppb",
            s.map_or(0, |s| u64::from(s.freq_ppb as u32)),
        ),
        ("service_holdovers", s.map_or(0, |s| u64::from(s.holdovers))),
        (
            "service_reply_age_ms",
            s.map_or(0, |s| u64::from(s.reply_age_ms.unwrap_or(u32::MAX))),
        ),
        ("follower_grandmaster", uuid(w.grandmaster)),
        ("service_grandmaster", s.map_or(0, |s| uuid(s.grandmaster))),
        ("follower", flag(on)),
        ("follower_locked", flag(w.locked)),
        ("follower_settled", flag(w.settled)),
        ("follower_holdover", flag(w.holdover)),
        ("follower_waiting", u64::from(waiting_code(w.waiting))),
        ("service_poll", u64::from(poll_code(s.map(|s| s.poll)))),
        ("service_locked", s.map_or(0, |s| flag(s.locked))),
        ("service_settled", s.map_or(0, |s| flag(s.settled))),
        ("service_holdover", s.map_or(0, |s| flag(s.holdover))),
        ("service_mode", s.map_or(0, |s| u64::from(s.mode))),
    ]
}

/// A reply datagram for `body` as a device sends it.
fn datagram(body: &[u8]) -> Vec<u8> {
    let mut d = header(0xffff, 32 + body.len(), 9, REPLY_OPCODE);
    d.extend_from_slice(body);
    d
}

#[test]
fn the_request_is_known_by_its_opcode_whatever_its_second_byte() {
    for second in [0x00, 0x31, 0x2a, 0xff] {
        let mut op = REQUEST;
        op[1] = second;
        assert_eq!(parse_request(&asking(op)).unwrap().kind, Kind::ClockFigures);
        assert!(!never_answered(&asking(op)));
    }
    for (at, v) in [
        (0, 0x08),
        (2, 0xd4),
        (2, 0x00),
        (3, 0xb2),
        (3, 0xb1),
        (3, 0xb4),
    ] {
        let mut op = REQUEST;
        op[at] = v;
        assert_ne!(
            parse_request(&asking(op)).unwrap().kind,
            Kind::ClockFigures,
            "{at} {v:#x}"
        );
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

/// A reader of `f` that counts its reads (or panics, `panics`).
fn counted(
    f: Option<ClockFigures>,
    panics: bool,
) -> (
    impl Fn() -> Option<ClockFigures>,
    Arc<std::sync::atomic::AtomicU64>,
) {
    let reads = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let r = Arc::clone(&reads);
    let read = move || {
        assert!(!panics, "figures read for another request");
        r.fetch_add(1, Ordering::Relaxed);
        f
    };
    (read, reads)
}

/// The group's path never answers it, and another datagram never reads
/// the figures nor takes a sequence.
#[test]
fn only_the_unicast_path_answers_and_only_this_request() {
    let mut mc = InfoBuilder::new(identity());
    let mut limit = Limit::default();
    let now = Instant::now();
    assert_eq!(mc.answer(&asking(REQUEST)), None);
    for _ in 0..20 {
        assert_eq!(mc.answer_within(&mut limit, now, &asking(REQUEST)), None);
    }
    let (never, _) = counted(Some(distinct()), true);
    for d in [
        captured(),
        captured()[..31].to_vec(),
        asking([0x07, 0x31, 0xd5, 0xb1, 0, 0, 0, 0]),
    ] {
        assert_eq!(mc.clock_within(&mut limit, now, &d, &never), None);
    }
    // A bridge-figures request is not this one either.
    assert_eq!(
        mc.bridge_within(&mut limit, now, &asking(REQUEST), None),
        None
    );
    assert_eq!(mc.board_info()[4..6], [0, 1], "no sequence used");
    // The bound is whole: ten replies now.
    let (read, reads) = counted(Some(distinct()), false);
    let n = (0..10)
        .filter(|_| {
            mc.clock_within(&mut limit, now, &asking(REQUEST), &read)
                .is_some()
        })
        .count();
    assert_eq!((n, reads.load(Ordering::Relaxed)), (10, 10));
}

/// A request over the bound reads no figures (the clock's stats lock); a
/// source with none answers nothing and uses no sequence.
#[test]
fn a_request_over_the_bound_reads_no_figures() {
    let mut mc = InfoBuilder::new(identity());
    let mut limit = Limit::default();
    let now = Instant::now();
    let (read, reads) = counted(Some(distinct()), false);
    let replies = (0..25)
        .filter(|_| {
            mc.clock_within(&mut limit, now, &asking(REQUEST), &read)
                .is_some()
        })
        .count();
    assert_eq!((replies, reads.load(Ordering::Relaxed)), (10, 10));
    let mut mc = InfoBuilder::new(identity());
    let (none, reads) = counted(None, false);
    assert_eq!(
        mc.clock_within(&mut Limit::default(), now, &asking(REQUEST), &none),
        None
    );
    assert_eq!(reads.load(Ordering::Relaxed), 1);
    assert_eq!(mc.board_info()[4..6], [0, 1], "no sequence used");
    let (read, _) = counted(Some(distinct()), false);
    let p = mc
        .clock_within(&mut Limit::default(), now, &asking(REQUEST), &read)
        .unwrap();
    assert_eq!(walk::reply(&p).unwrap().get("sequence"), 1);
}

#[test]
fn a_reply_carries_the_figures_its_own_header_and_a_sequence_from_1() {
    let mut mc = InfoBuilder::new(identity());
    let mut limit = Limit::default();
    let now = Instant::now();
    let (read, _) = counted(Some(distinct()), false);
    let first = mc
        .clock_within(&mut limit, now, &asking(REQUEST), &read)
        .expect("a reply");
    assert_eq!(
        first[..32],
        header(0xffff, 32 + FIGURES_LEN, 1, REPLY_OPCODE)
    );
    let walked = walk::reply(&first).expect("walked");
    assert_eq!(walked.device_id, identity().device_id);
    let want = ClockFigures {
        sequence: 1,
        ..distinct()
    };
    assert_eq!(walked.fields, keyed(&want));
    assert_eq!(mc.board_info()[4..6], [0, 2]);
    let second = mc
        .clock_within(&mut limit, now, &asking(REQUEST), &read)
        .expect("a reply");
    assert_eq!(second[4..6], [0, 3]);
    assert_eq!(walk::reply(&second).unwrap().get("sequence"), 2);
    // The bridge figures count their own sequence.
    assert_eq!(mc.bridge_replies, 0);
}

#[test]
fn the_sequence_wraps_after_u32_max() {
    let mut mc = InfoBuilder::new(identity());
    mc.clock_replies = u32::MAX - 1;
    let mut limit = Limit::default();
    let now = Instant::now();
    let (read, _) = counted(Some(distinct()), false);
    let seqs: Vec<u64> = (0..3)
        .map(|_| {
            let p = mc
                .clock_within(&mut limit, now, &asking(REQUEST), &read)
                .unwrap();
            walk::reply(&p).unwrap().get("sequence")
        })
        .collect();
    assert_eq!(seqs, [u64::from(u32::MAX), 0, 1]);
}

/// Each kind keeps its own bound: the bridge figures' and the board
/// info's are untouched by a clock-figures burst, and the report names it.
#[test]
fn ten_clock_replies_a_second_beside_the_other_kinds() {
    let mut mc = InfoBuilder::new(identity());
    let mut limit = Limit::default();
    let t0 = Instant::now();
    let (read, _) = counted(Some(distinct()), false);
    let replies = (0..13)
        .filter(|_| {
            mc.clock_within(&mut limit, t0, &asking(REQUEST), &read)
                .is_some()
        })
        .count();
    assert_eq!(replies, 10);
    let boards = (0..10)
        .filter(|_| mc.answer_within(&mut limit, t0, &captured()).is_some())
        .count();
    assert_eq!(boards, 10);
    let report = limit
        .flush(t0 + limit::REPORT_EVERY)
        .expect("drops counted");
    assert_eq!(report.to_string(), "0xd5b3 clock figures 3");
}

#[test]
fn the_source_is_the_test_clock_or_the_linked_one_or_the_service() {
    assert_eq!(TimeSource::of(true, None), TimeSource::System);
    assert_eq!(
        TimeSource::of(true, Some(Source::Internal)),
        TimeSource::System
    );
    assert_eq!(TimeSource::of(false, None), TimeSource::Service);
    assert_eq!(
        TimeSource::of(false, Some(Source::Service)),
        TimeSource::Service
    );
    assert_eq!(
        TimeSource::of(false, Some(Source::Internal)),
        TimeSource::Internal
    );
    assert_eq!(TimeSource::of(false, Some(Source::None)), TimeSource::None);
}

const S: i64 = 1_000_000_000;

/// A DSYX reading: locked or not, settled, mode 4, -750 ppb, a grandmaster.
fn reading(locked: bool) -> Reading {
    Reading {
        wall_ns: 1,
        locked,
        settled: true,
        mode: 4,
        freq_ppb: -750,
        grandmaster: [0x00, 0x1d, 0xc1, 0x0a, 0x0b, 0x0c],
        master: [0; 6],
        date: None,
    }
}

/// The test system clock: no follower, no service, ready as asked.
#[test]
fn the_test_clock_s_figures_name_it_and_nothing_else() {
    let stats = ClockStats::default();
    stats.mark_system();
    assert_eq!(
        figures_of(&stats, true, 5 * S),
        ClockFigures {
            source: TimeSource::System,
            ready: true,
            ..ClockFigures::default()
        }
    );
    assert!(!figures_of(&stats, false, 5 * S).ready);
}

/// A device on the service only: the service's part from the poller's
/// read, its age from the reply's moment; before the first poll none.
#[test]
fn the_service_s_part_is_the_poller_s_last_read() {
    let stats = ClockStats::default();
    let f = figures_of(&stats, false, 5 * S);
    assert_eq!(
        (f.source, f.follower, f.service),
        (TimeSource::Service, None, None)
    );
    // Not locked yet: the reply's word and mode, not locked.
    // The grandmaster is the reply's own, never another poll's.
    stats.set_grandmaster([9; 6]);
    stats.set_poll(Sample::NotLocked, Some(reading(false)), 2 * S);
    let f = figures_of(&stats, false, 2 * S + 1_500_000);
    assert_eq!(
        f.service,
        Some(ServiceFigures {
            poll: PollOutcome::NotLocked,
            reply_age_ms: Some(1),
            locked: false,
            settled: true,
            mode: 4,
            holdover: false,
            holdovers: 0,
            freq_ppb: -750,
            grandmaster: Some([0x00, 0x1d, 0xc1, 0x0a, 0x0b, 0x0c]),
        })
    );
    // Locked and ready, then a poll with no reply: the last reply kept,
    // older, and the poller's holdover counted.
    stats.set_poll(Sample::Usable { k_ns: 7 }, Some(reading(true)), 3 * S);
    stats.record(crate::clock::Event::Ready { k_ns: 7 });
    stats.set_poll(Sample::NoReply, None, 4 * S);
    stats.record(crate::clock::Event::Holdover);
    let f = figures_of(&stats, true, 6 * S);
    let s = f.service.unwrap();
    assert_eq!(
        (s.poll, s.reply_age_ms, s.locked),
        (PollOutcome::NoReply, Some(3_000), true)
    );
    assert_eq!((s.holdover, s.holdovers), (true, 1));
    assert!(f.ready);
    // A poll that never had a reply: no age, nothing of a reply.
    let fresh = ClockStats::default();
    fresh.set_poll(Sample::NoReply, None, S);
    assert_eq!(
        figures_of(&fresh, false, 2 * S).service,
        Some(ServiceFigures {
            poll: PollOutcome::NoReply,
            reply_age_ms: None,
            locked: false,
            settled: false,
            mode: 0,
            holdover: false,
            holdovers: 0,
            freq_ppb: 0,
            grandmaster: None,
        })
    );
}

/// A linked clock: its source, and the follower's part from the counts
/// the selector set last.
#[test]
fn the_follower_s_part_is_the_selector_s_last_counts() {
    let stats = ClockStats::default();
    stats.set_linked(Source::Service);
    stats.set_follower(Some(FollowerCounts {
        counts: ClockCounts {
            polls: 300,
            re_anchors: 4,
            holdovers: 3,
            in_holdover: true,
        },
        absorbed: 2,
        locked: true,
        settled: false,
        offset_ns: Some(-39_806),
        recent_abs_e_ns: Some(21_000),
        pull_ns: -88_000,
        latency_steps: 0,
        grandmaster: Some([0x00, 0x1d, 0xc1, 0xab, 0xcd, 0xef]),
        waiting: Some(Waiting::WaitsToAgree),
        freq_ppb: -12_345,
        offset_from_service_ns: Some(1_000_250),
    }));
    let f = figures_of(&stats, true, S);
    assert_eq!(f.source, TimeSource::Service);
    assert_eq!(
        f.follower,
        Some(FollowerFigures {
            locked: true,
            settled: false,
            holdover: true,
            waiting: Some(Waiting::WaitsToAgree),
            grandmaster: Some([0x00, 0x1d, 0xc1, 0xab, 0xcd, 0xef]),
            holdovers: 3,
            freq_ppb: -12_345,
            error_p90_ns: Some(21_000),
            offset_from_service_ns: Some(1_000_250),
        })
    );
    stats.set_linked(Source::Internal);
    assert_eq!(figures_of(&stats, true, S).source, TimeSource::Internal);
    stats.set_follower(None);
    assert_eq!(figures_of(&stats, true, S).follower, None);
}

/// A beat with clock figures (and no bridge).
struct Clocked(ClockFigures);

impl Beat for Clocked {
    fn records(&self) -> Option<Records> {
        None
    }

    fn clock_figures(&self) -> Option<ClockFigures> {
        Some(self.0)
    }
}

#[test]
fn the_running_device_s_beat_gives_its_clock_s_figures() {
    use crate::daemon::media::tests::{Unready, device_at, media_on};
    let dir = tempfile::tempdir().unwrap();
    let media = media_on(&device_at(dir.path()), Arc::new(Unready));
    let beat = media.beat();
    // An unready clock on the service only, never polled.
    assert_eq!(
        Beat::clock_figures(&beat),
        Some(ClockFigures {
            source: TimeSource::Service,
            ..ClockFigures::default()
        })
    );
    media.clock_stats.set_poll(
        Sample::Usable { k_ns: 1 },
        Some(reading(true)),
        crate::clock::monotonic_ns(),
    );
    let s = Beat::clock_figures(&beat).unwrap().service.expect("polled");
    assert_eq!(
        (s.poll, s.locked, s.freq_ppb),
        (PollOutcome::Usable, true, -750)
    );
    // Read now: a reply a moment ago is a few ms old at most.
    assert!(s.reply_age_ms.unwrap() < 5_000, "{s:?}");
    assert!(
        NoClock.clock_figures().is_none(),
        "a beat knows none by default"
    );
}

#[tokio::test]
async fn the_server_answers_clock_figures_to_the_asker_alone() {
    let (at, info_rx, beat_rx, to, sock) = loopback(NEVER).await;
    let task = tokio::spawn(serve_info(
        sock,
        InfoBuilder::new(identity()),
        to,
        Arc::new(Clocked(distinct())),
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
    let want = ClockFigures {
        sequence: 1,
        ..distinct()
    };
    assert_eq!(walked.fields, keyed(&want));
    let mut group = [0u8; 64];
    let to_group =
        tokio::time::timeout(Duration::from_millis(200), info_rx.recv_from(&mut group)).await;
    assert!(to_group.is_err(), "the info group heard nothing");
    task.abort();
}

/// Several devices on one PC: the request reaches a member with its
/// sender, and the member's reply goes back to that sender through the
/// manager (which knows no clock here), with the member's id.
#[tokio::test]
async fn a_member_s_clock_figures_go_to_the_asker_through_the_manager() {
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
        Arc::new(Clocked(distinct())),
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
        .expect("the member's reply")
        .unwrap();
    assert_eq!(from, at, "sent from the address's info port");
    let walked = walk::reply(&buf[..n]).expect("walked");
    assert_eq!(walked.device_id, b.device_id);
    assert_eq!(
        walked.fields,
        keyed(&ClockFigures {
            sequence: 1,
            ..distinct()
        })
    );
    let again = tokio::time::timeout(Duration::from_millis(200), asker.recv_from(&mut buf)).await;
    assert!(again.is_err(), "one reply");
    // A forged request to the member from a stranger is never answered.
    let stranger = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let ask = share::Msg::Ask(v4(asker.local_addr().unwrap()), asking(REQUEST)).encode();
    stranger.send_to(&ask, member_at).await.unwrap();
    let got = tokio::time::timeout(Duration::from_millis(200), asker.recv_from(&mut buf)).await;
    assert!(got.is_err(), "a member answers its manager's relays only");
    member.abort();
    manager.abort();
}

/// The manager hands a member's clock reply to a recent asker, and drops
/// one that is no clock or bridge reply.
#[tokio::test]
async fn the_manager_sends_a_member_s_clock_reply_only_as_such() {
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
    asker.send_to(&asking(REQUEST), at).await.unwrap();
    let (n, _) = tokio::time::timeout(BOUND, fake.recv_from(&mut buf))
        .await
        .expect("the relay")
        .unwrap();
    let me = v4(asker.local_addr().unwrap());
    assert_eq!(
        share::Msg::decode(&buf[..n]),
        Some(share::Msg::Ask(me, asking(REQUEST)))
    );
    let reply = {
        let mut mc = InfoBuilder::new(identity());
        let (read, _) = counted(Some(distinct()), false);
        mc.clock_within(
            &mut Limit::default(),
            Instant::now(),
            &asking(REQUEST),
            &read,
        )
        .unwrap()
    };
    let short = share::Msg::SendTo(me, reply[..reply.len() - 1].to_vec()).encode();
    fake.send_to(&short, manager_at).await.unwrap();
    fake.send_to(&share::Msg::SendTo(me, reply.clone()).encode(), manager_at)
        .await
        .unwrap();
    let (n, from) = tokio::time::timeout(BOUND, asker.recv_from(&mut buf))
        .await
        .expect("the reply")
        .unwrap();
    assert_eq!((from, &buf[..n]), (at, &reply[..]), "only the clock reply");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut b = [0u8; 256];
    assert!(asker.try_recv_from(&mut b).is_err(), "nothing else");
    manager.abort();
}

#[test]
fn only_a_clock_reply_of_its_length_and_opcode_is_one() {
    let mut mc = InfoBuilder::new(identity());
    let (read, _) = counted(Some(distinct()), false);
    let reply = mc
        .clock_within(
            &mut Limit::default(),
            Instant::now(),
            &asking(REQUEST),
            &read,
        )
        .unwrap();
    assert!(share::is_clock_reply(&reply));
    assert!(!share::is_bridge_reply(&reply));
    for n in [32 + FIGURES_LEN - 1, 32, 31, 0] {
        assert!(!share::is_clock_reply(&reply[..n]), "{n}");
    }
    let mut longer = reply.clone();
    longer.push(0);
    assert!(!share::is_clock_reply(&longer));
    for at in 24..32 {
        let mut other = reply.clone();
        other[at] ^= 1;
        assert!(!share::is_clock_reply(&other), "opcode byte {at}");
    }
    assert!(!share::is_clock_reply(&mc.board_info()));
}

#[test]
fn the_manager_relays_a_clock_request_with_its_sender() {
    let asker: SocketAddr = "127.0.0.9:4321".parse().unwrap();
    assert_eq!(
        server::relay_of(&asking(REQUEST), asker),
        share::Msg::Ask(v4(asker), asking(REQUEST))
    );
}

/// The client asks a server on a 8700 of its own address (127.0.39.2) and
/// reads the figures; another device's id gives a timeout.
#[tokio::test]
async fn the_client_reads_a_device_s_clock_figures() {
    let ip = Ipv4Addr::new(127, 0, 39, 2);
    let (_, info_rx, beat_rx, to, _) = loopback(NEVER).await;
    let sock = UdpSocket::bind((ip, REQUEST_PORT)).await.unwrap();
    let task = tokio::spawn(serve_info(
        sock,
        InfoBuilder::new(identity()),
        to,
        Arc::new(Clocked(distinct())),
        Arc::new(Changes::default()),
        Arc::new(Requests::default()),
    ));
    recv(&info_rx).await;
    recv(&info_rx).await;
    recv(&beat_rx).await;
    let id = identity().device_id;
    let got = tokio::task::spawn_blocking(move || {
        let s = client::unicast_socket(Ipv4Addr::LOCALHOST).unwrap();
        let read = client::clock_figures(&s, ip, Some(id), BOUND);
        let other = client::clock_figures(&s, ip, Some([7; 8]), Duration::from_millis(300));
        (read, other)
    })
    .await
    .unwrap();
    let f = got.0.expect("the figures");
    assert_eq!(
        f,
        ClockFigures {
            sequence: f.sequence,
            ..distinct()
        }
    );
    assert!(f.sequence >= 1);
    assert_eq!(
        got.1,
        Err(client::InfoError::Timeout {
            ask: client::Ask::ClockFigures
        }),
        "another device's id"
    );
    task.abort();
}
