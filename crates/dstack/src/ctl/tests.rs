use super::*;

/// The status's `socket_drops` field: Linux only (`/proc/net/udp`).
const DROPS: &str = if cfg!(target_os = "linux") {
    "\"socket_drops\":0,"
} else {
    ""
};

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::clock::{Event, ManualClock};
use crate::daemon::media::tests::{BOUND, T, Unready, device_at, media_on, media_with_engine, sub};
use crate::media::check::CONFIRM;

const LIMITS: Limits = Limits {
    tx: 16,
    rx: 32,
    rate: 96_000,
};

fn parsed(line: &str) -> Result<Request, &'static str> {
    parse(line, LIMITS)
}

#[test]
fn each_request_parses() {
    let cases = [
        ("status", Request::Status),
        ("  status \r", Request::Status),
        (
            "source 1 silence",
            Request::Source {
                tx: 0,
                source: Source::Silence,
            },
        ),
        (
            "source 16 counter",
            Request::Source {
                tx: 15,
                source: Source::Counter,
            },
        ),
        (
            "source 2 sine 1000 -20",
            Request::Source {
                tx: 1,
                source: Source::Sine {
                    hz: 1000.0,
                    dbfs: -20.0,
                },
            },
        ),
        (
            "source 3 sine 47999.5 -96.5",
            Request::Source {
                tx: 2,
                source: Source::Sine {
                    hz: 47_999.5,
                    dbfs: -96.5,
                },
            },
        ),
        (
            "source 1 echo 32 65535",
            Request::Source {
                tx: 0,
                source: Source::Echo {
                    rx: 31,
                    offset: 65_535,
                },
            },
        ),
        (
            "source 1 echo 1 0",
            Request::Source {
                tx: 0,
                source: Source::Echo { rx: 0, offset: 0 },
            },
        ),
        ("check 1 off", Request::Check { rx: 0, check: None }),
        (
            "check 32 counter",
            Request::Check {
                rx: 31,
                check: Some(CheckKind::Counter),
            },
        ),
        (
            "check\t2\tsine 0.5",
            Request::Check {
                rx: 1,
                check: Some(CheckKind::Sine { hz: 0.5 }),
            },
        ),
    ];
    for (line, want) in cases {
        assert_eq!(parsed(line), Ok(want), "{line:?}");
    }
}

#[test]
fn a_bad_request_is_a_fixed_reason_never_its_own_words() {
    let cases = [
        ("", UNKNOWN),
        ("   ", UNKNOWN),
        ("zyxqw", UNKNOWN),
        ("STATUS", UNKNOWN),
        ("status zyxqw", "status takes nothing more"),
        ("source", SOURCE_USAGE),
        ("source 1", SOURCE_USAGE),
        ("source 1 zyxqw", SOURCE_USAGE),
        ("source 1 counter 2", SOURCE_USAGE),
        ("source 1 sine 1000", SOURCE_USAGE),
        ("source 1 echo 1", SOURCE_USAGE),
        ("source 1 engine 2", SOURCE_USAGE),
        ("source 1 ENGINE", SOURCE_USAGE),
        ("source 17 engine", "source: no such TX channel"),
        ("source 0 counter", "source: no such TX channel"),
        ("source 17 counter", "source: no such TX channel"),
        ("source -1 counter", "source: no such TX channel"),
        ("source zyxqw counter", "source: no such TX channel"),
        ("source 1 sine 0 -20", HZ),
        ("source 1 sine -5 -20", HZ),
        ("source 1 sine 48000 -20", HZ),
        ("source 1 sine NaN -20", HZ),
        ("source 1 sine inf -20", HZ),
        ("source 1 sine zyxqw -20", HZ),
        (
            "source 1 sine 1000 -19.9",
            "the sine's level must be a number of dBFS of at most -20",
        ),
        (
            "source 1 sine 1000 0",
            "the sine's level must be a number of dBFS of at most -20",
        ),
        (
            "source 1 sine 1000 NaN",
            "the sine's level must be a number of dBFS of at most -20",
        ),
        (
            "source 1 sine 1000 -inf",
            "the sine's level must be a number of dBFS of at most -20",
        ),
        ("source 1 echo 0 5", "source: no such RX channel to echo"),
        ("source 1 echo 33 5", "source: no such RX channel to echo"),
        (
            "source 1 echo 1 65536",
            "the echo's offset must be a whole number of samples below 65536",
        ),
        (
            "source 1 echo 1 -1",
            "the echo's offset must be a whole number of samples below 65536",
        ),
        (
            "source 1 echo 1 1.5",
            "the echo's offset must be a whole number of samples below 65536",
        ),
        ("check", CHECK_USAGE),
        ("check 1", CHECK_USAGE),
        ("check 1 sine", CHECK_USAGE),
        ("check 1 counter 5", CHECK_USAGE),
        ("check 0 counter", "check: no such RX channel"),
        ("check 33 counter", "check: no such RX channel"),
        ("check 1 sine 48000", HZ),
    ];
    for (line, want) in cases {
        let got = parsed(line).expect_err(line);
        assert_eq!(got, want, "{line:?}");
        assert!(!got.contains("zyxqw"), "{line:?} echoed: {got}");
    }
}

#[test]
fn source_engine_parses() {
    assert_eq!(
        parsed("source 1 engine"),
        Ok(Request::Source {
            tx: 0,
            source: Source::Engine,
        })
    );
    assert_eq!(
        parsed(" source\t16 engine\r"),
        Ok(Request::Source {
            tx: 15,
            source: Source::Engine,
        })
    );
}

#[test]
fn a_channel_counts_from_1_to_the_device_s_channels() {
    assert_eq!(channel("1", 3), Some(0));
    assert_eq!(channel("3", 3), Some(2));
    assert_eq!(channel("4", 3), None);
    assert_eq!(channel("0", 3), None);
    assert_eq!(channel("1", 0), None);
    // Just below half the rate is a frequency, half is not.
    assert_eq!(frequency("23999.9", 48_000), Some(23_999.9));
    assert_eq!(frequency("24000", 48_000), None);
    assert_eq!(frequency("1e-9", 48_000), Some(1e-9));
    assert_eq!(level("-20"), Some(-20.0));
    assert_eq!(level("-1e300"), Some(-1e300));
}

#[test]
fn the_opcodes_counted_are_the_header_s_and_the_info_request_s() {
    let arc = crate::wire::request(0x27ff, 9, 0x2200, 0, &[0, 1]);
    assert_eq!(header_op(&arc), 0x2200);
    assert_eq!(header_op(&arc[..8]), 0x2200);
    assert_eq!(header_op(&arc[..7]), 0);
    assert_eq!(header_op(&[]), 0);
    let mut info = vec![0u8; 32];
    info[24..32].copy_from_slice(&[0x07, 0x3e, 0x00, 0x61, 0, 0, 0, 0]);
    assert_eq!(info_op(&info), 0x0061);
    info[26] = 0x01;
    assert_eq!(info_op(&info), 0x0161);
    assert_eq!(info_op(&info[..28]), 0x0161);
    assert_eq!(info_op(&info[..27]), 0);
}

fn received(r: &Requests) -> Vec<(u16, usize, u16, u64)> {
    r.counts()
        .received
        .iter()
        .map(|e| (e.port, e.peer, e.op, e.n))
        .collect()
}

#[test]
fn requests_are_counted_by_port_peer_and_opcode_peers_shared_with_what_we_send() {
    let r = Requests::default();
    let a = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 11));
    let b = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 12));
    assert_eq!(r.counts(), RequestCounts::default());
    assert_eq!(r.received(4440, a, 0x2200), 1);
    assert_eq!(r.received(4440, a, 0x2200), 1);
    assert_eq!(r.received(8700, b, 0x0061), 2);
    assert_eq!(r.received(4440, a, 0x1000), 1);
    // What we send shares the peers' ids.
    assert_eq!(r.sent(b, 0x0100), 2);
    assert_eq!(r.sent(b, 0x0100), 2);
    let c = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 13));
    assert_eq!(r.sent(c, 0x0101), 3);
    assert_eq!(
        received(&r),
        [
            (4440, 1, 0x1000, 1),
            (4440, 1, 0x2200, 2),
            (8700, 2, 0x0061, 1)
        ]
    );
    assert_eq!(
        r.counts().sent,
        [
            Sent {
                peer: 2,
                op: 0x0100,
                n: 2
            },
            Sent {
                peer: 3,
                op: 0x0101,
                n: 1
            }
        ]
    );
    assert_eq!(r.counts().uncounted, 0);
}

#[test]
fn the_tables_are_bounded_peers_past_64_are_0_and_entries_past_256_summed() {
    let r = Requests::default();
    let ip = |k: u32| IpAddr::V4(Ipv4Addr::from_bits(0x0a00_0000 + k));
    for k in 0..MAX_PEERS as u32 {
        assert_eq!(r.received(4440, ip(k), 1), k as usize + 1);
    }
    assert_eq!(r.received(4440, ip(64), 1), 0);
    assert_eq!(r.sent(ip(65), 0x0100), 0);
    // The first peer keeps its id.
    assert_eq!(r.received(4440, ip(0), 1), 1);
    // 65 entries so far; fill the table to 256 with opcodes of peer 1.
    for op in 2..=(MAX_ENTRIES as u16 - 64) {
        r.received(4440, ip(0), op);
    }
    assert_eq!(r.counts().received.len(), MAX_ENTRIES);
    assert_eq!(r.counts().uncounted, 0);
    // A new entry is summed; an entry already counted still counts.
    r.received(4440, ip(0), 9999);
    r.received(8800, ip(0), 1);
    assert_eq!(r.counts().uncounted, 2);
    r.received(4440, ip(0), 2);
    assert!(received(&r).contains(&(4440, 1, 2, 2)));
    assert_eq!(r.counts().uncounted, 2);
    // The sent table has its own 256 entries (one is peer 0's request).
    for op in 0..MAX_ENTRIES as u16 - 1 {
        r.sent(ip(1), op);
    }
    assert_eq!(r.counts().sent.len(), MAX_ENTRIES);
    assert_eq!(r.counts().uncounted, 2);
    r.sent(ip(1), 9999);
    assert_eq!(r.counts().uncounted, 3);
}

/// A ring holding the counter source (`(position mod 65 536) << 8`) at
/// `from..to`.
fn counter_ring(from: u64, to: u64) -> Ring {
    let ring = Ring::new();
    let samples: Vec<i32> = (from..to).map(|p| ((p % 65_536) << 8) as i32).collect();
    ring.write_at(from, &samples);
    ring
}

const PLAY: Playout = Playout {
    latency: 96,
    bits: 24,
};

fn counter_stats(c: &Checks, rx: usize) -> CounterStats {
    match c.views()[rx].stats {
        CheckStats::Counter(s) => s,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_check_reads_its_ring_from_where_it_left_off_to_the_playout_point() {
    let mut c = Checks::new(2, 96_000);
    assert!(c.set(1, Some(CheckKind::Counter)));
    assert!(!c.set(2, Some(CheckKind::Counter)), "no RX 3");
    let rings = [Ring::new(), counter_ring(10_000, 20_000)];
    let plays = [None, Some(PLAY)];
    assert_eq!(c.views()[0].stats, CheckStats::Waiting);
    // The first round starts at the playout point (now less 96) and reads
    // nothing yet.
    c.step(10_096, &rings, &plays);
    assert_eq!(counter_stats(&c, 0), CounterStats::default());
    // The next reads 10 000…11 903: four samples to lock, then clean.
    c.step(12_000, &rings, &plays);
    let s = counter_stats(&c, 0);
    assert_eq!(s.blind, u64::from(CONFIRM));
    assert_eq!(s.checked, 1_904 - u64::from(CONFIRM));
    assert_eq!((s.lag, s.dropout, s.mismatch, s.slips), (Some(0), 0, 0, 0));
    // In chunks, across one round of 6 000 samples.
    c.step(18_000, &rings, &plays);
    let s = counter_stats(&c, 0);
    assert_eq!(s.checked + s.blind, 7_904);
    assert_eq!((s.dropout, s.mismatch), (0, 0));
    // Past what the ring holds: one dropout run.
    c.step(20_196, &rings, &plays);
    let s = counter_stats(&c, 0);
    assert_eq!((s.dropout, s.dropout_frames), (1, 100));
    assert_eq!(c.views()[0].rx, 1);
    assert_eq!(c.views()[0].skipped, 0);
    // The read was the ring's: holes counted there.
    assert_eq!(rings[1].counters().holes, 100);
}

#[test]
fn a_check_without_a_playing_flow_waits_and_starts_again_at_the_playout_point() {
    let mut c = Checks::new(1, 96_000);
    c.set(0, Some(CheckKind::Counter));
    let rings = [counter_ring(0, 60_000)];
    c.step(1_096, &rings, &[None]);
    assert_eq!(c.views()[0].stats, CheckStats::Waiting);
    c.step(2_096, &rings, &[Some(PLAY)]);
    c.step(3_096, &rings, &[Some(PLAY)]);
    assert_eq!(
        counter_stats(&c, 0).checked + counter_stats(&c, 0).blind,
        1_000
    );
    // The flow goes: nothing read; back later, the samples between are
    // not read (not a dropout, not skipped).
    c.step(5_096, &rings, &[None]);
    c.step(9_096, &rings, &[Some(PLAY)]);
    c.step(10_096, &rings, &[Some(PLAY)]);
    let s = counter_stats(&c, 0);
    assert_eq!((s.checked + s.blind, s.dropout), (2_000, 0));
    assert_eq!(c.views()[0].skipped, 0);
    // Off: no check.
    c.set(0, None);
    assert!(c.views().is_empty());
}

#[test]
fn a_check_too_far_behind_skips_ahead_and_a_clock_gone_back_is_waited_for() {
    let mut c = Checks::new(1, 96_000);
    c.set(0, Some(CheckKind::Counter));
    let rings = [Ring::new()];
    let plays = [Some(PLAY)];
    let read = |c: &Checks| match c.views()[0].stats {
        CheckStats::Counter(s) => s.checked + s.blind,
        other => panic!("{other:?}"),
    };
    // Half the ring: a slow round never lets the RX thread overwrite what
    // a check has yet to read.
    assert_eq!(MAX_BEHIND, SLOTS as u64 / 2);
    c.step(100_096, &rings, &plays);
    // Exactly the bound behind: read.
    c.step(100_096 + MAX_BEHIND, &rings, &plays);
    assert_eq!((read(&c), c.views()[0].skipped), (MAX_BEHIND, 0));
    // One past it: skipped, nothing read.
    c.step(100_096 + 2 * MAX_BEHIND + 1, &rings, &plays);
    assert_eq!(
        (read(&c), c.views()[0].skipped),
        (MAX_BEHIND, MAX_BEHIND + 1)
    );
    // The clock goes back 1 000 samples: wait until it passes the cursor.
    let at = 100_096 + 2 * MAX_BEHIND + 1;
    c.step(at - 1_000, &rings, &plays);
    c.step(at, &rings, &plays);
    assert_eq!(read(&c), MAX_BEHIND);
    c.step(at + 10, &rings, &plays);
    assert_eq!(read(&c), MAX_BEHIND + 10);
    // Back exactly the bound: still waited for (nothing read until the
    // playout point passes the cursor again).
    c.step(at + 10 - MAX_BEHIND, &rings, &plays);
    c.step(at + 9, &rings, &plays);
    assert_eq!(read(&c), MAX_BEHIND + 10);
    c.step(at + 11, &rings, &plays);
    assert_eq!(read(&c), MAX_BEHIND + 11);
    // Back further: start again at the new playout point, nothing skipped.
    let back = at + 11 - MAX_BEHIND - 1;
    c.step(back, &rings, &plays);
    c.step(back + 5, &rings, &plays);
    assert_eq!(
        (read(&c), c.views()[0].skipped),
        (MAX_BEHIND + 16, MAX_BEHIND + 1)
    );
}

/// A −20 dBFS 1 kHz sine at 96 kHz, cut to `bits` (left-justified).
fn sine_ring(bits: u32, len: u64) -> Ring {
    let ring = Ring::new();
    let amp = f64::from(i32::MAX) * 0.1;
    let step = 1i64 << (32 - bits);
    let samples: Vec<i32> = (0..len)
        .map(|p| {
            let x = amp * (std::f64::consts::TAU * 1000.0 * p as f64 / 96_000.0).sin();
            ((x as i64 / step) * step) as i32
        })
        .collect();
    ring.write_at(0, &samples);
    ring
}

#[test]
fn a_sine_check_judges_by_its_flow_s_bits() {
    let ring16 = [sine_ring(16, 20_000)];
    let sine = Some(CheckKind::Sine { hz: 1000.0 });
    let run = |rings: &[Ring], bits: u8| {
        let mut c = Checks::new(1, 96_000);
        c.set(0, sine);
        let plays = [Some(Playout { latency: 96, bits })];
        c.step(1_096, rings, &plays);
        c.step(19_096, rings, &plays);
        match c.views()[0].stats {
            CheckStats::Sine(s) => s,
            other => panic!("{other:?}"),
        }
    };
    // A 16-bit flow's sine is clean against a 16-bit floor ...
    let s = run(&ring16, 16);
    assert_eq!(s.discontinuities, 0, "{s:?}");
    assert!(s.checked > 17_000, "{s:?}");
    // ... and its truncation is a discontinuity against a 24-bit one.
    assert!(run(&ring16, 24).discontinuities > 0);
    let ring24 = [sine_ring(24, 20_000)];
    assert_eq!(run(&ring24, 24).discontinuities, 0);
}

#[test]
fn the_counter_check_says_no_return_after_10_s_without_a_lock() {
    let mut c = Checks::new(1, 1_000);
    c.set(0, Some(CheckKind::Counter));
    let rings = [Ring::new()];
    let plays = [Some(Playout {
        latency: 0,
        bits: 24,
    })];
    c.step(1, &rings, &plays);
    // Never a sample at or past the playout point.
    c.step(3, &rings, &plays);
    assert_eq!(counter_stats(&c, 0).blind, 2);
    c.step(10_000, &rings, &plays);
    assert_eq!(counter_stats(&c, 0).blind, 9_999);
    assert!(!counter_stats(&c, 0).no_return);
    c.step(10_001, &rings, &plays);
    assert!(counter_stats(&c, 0).no_return);
}

/// The status line as JSON, checked by hand: each value is a number,
/// `null`, an object, an array or a string; returns every string (keys
/// included). Panics on anything else.
fn strings_of_json(text: &str) -> Vec<String> {
    struct P<'a> {
        b: &'a [u8],
        at: usize,
        strings: Vec<String>,
    }
    impl P<'_> {
        fn peek(&self) -> u8 {
            *self.b.get(self.at).expect("more JSON")
        }
        fn eat(&mut self, c: u8) {
            assert_eq!(self.peek() as char, c as char, "at {}", self.at);
            self.at += 1;
        }
        fn string(&mut self) {
            self.eat(b'"');
            let start = self.at;
            while self.peek() != b'"' {
                assert!(self.peek() != b'\\', "an escape");
                self.at += 1;
            }
            let s = std::str::from_utf8(&self.b[start..self.at]).unwrap();
            self.strings.push(s.to_string());
            self.at += 1;
        }
        fn value(&mut self) {
            match self.peek() {
                b'{' => {
                    self.at += 1;
                    if self.peek() == b'}' {
                        self.at += 1;
                        return;
                    }
                    loop {
                        self.string();
                        self.eat(b':');
                        self.value();
                        if self.peek() == b',' {
                            self.at += 1;
                        } else {
                            self.eat(b'}');
                            return;
                        }
                    }
                }
                b'[' => {
                    self.at += 1;
                    if self.peek() == b']' {
                        self.at += 1;
                        return;
                    }
                    loop {
                        self.value();
                        if self.peek() == b',' {
                            self.at += 1;
                        } else {
                            self.eat(b']');
                            return;
                        }
                    }
                }
                b'"' => self.string(),
                b'n' => {
                    assert_eq!(&self.b[self.at..self.at + 4], b"null");
                    self.at += 4;
                }
                _ => {
                    let start = self.at;
                    while self
                        .b
                        .get(self.at)
                        .is_some_and(|c| b"-+.eE0123456789".contains(c))
                    {
                        self.at += 1;
                    }
                    let n = std::str::from_utf8(&self.b[start..self.at]).unwrap();
                    assert!(n.parse::<f64>().is_ok(), "not a number: {n:?}");
                }
            }
        }
    }
    let mut p = P {
        b: text.as_bytes(),
        at: 0,
        strings: Vec::new(),
    };
    p.value();
    assert_eq!(p.at, text.len(), "one value, nothing after");
    p.strings
}

/// The value of `key` in `text`'s first object holding it (`"key":value`).
fn value_of<'a>(text: &'a str, key: &str) -> &'a str {
    let pat = format!("\"{key}\":");
    let start = text.find(&pat).unwrap_or_else(|| panic!("{key} in {text}")) + pat.len();
    let end = text[start..]
        .find([',', '}', ']'])
        .map_or(text.len(), |e| start + e);
    &text[start..end]
}

/// The words a status may hold besides numbers: its keys and the fixed
/// words of sources and checks.
const WORDS: &[&str] = &[
    "clock",
    "ready",
    "epoch",
    "polls",
    "re_anchors",
    "holdovers",
    "holdover",
    "freq_ppb",
    "follower",
    "windows",
    "absorbed",
    "locked",
    "settled",
    "offset_ns",
    "recent_abs_e_ns",
    "pull_ns",
    "dsync",
    "system",
    "internal",
    "service",
    "none",
    "requests",
    "port",
    "peer",
    "op",
    "n",
    "sent",
    "uncounted",
    "tx",
    "flow",
    "live",
    "fpp",
    "channels",
    "packets",
    "send_errors",
    "send_retries",
    "keepalives",
    "rebootstraps",
    "holes",
    "late_bins",
    "late_max_us",
    "send_max_us",
    "age_max_us",
    "over_latency",
    "receiver_latency_us",
    "direct",
    "rx",
    "granted",
    "latency",
    "playout",
    "delay",
    "late_after",
    "advertised",
    "below_advertised",
    "refused",
    "stray",
    "out_of_order",
    "gaps",
    "gap_frames",
    "socket_drops",
    "late",
    "late_at_read",
    "unplaced",
    "lateness",
    "lateness_peak",
    "lateness_min_ns",
    "keepalive_errors",
    "recv_errors",
    "rx_poll_errors",
    "status",
    "written",
    "duplicate",
    "early",
    "sources",
    "source",
    "counter",
    "sine",
    "echo",
    "hz",
    "dbfs",
    "offset",
    "checks",
    "check",
    "skipped",
    "waiting",
    "checked",
    "blind",
    "mismatch",
    "mismatch_frames",
    "dropout",
    "dropout_frames",
    "slips",
    "last_slip",
    "lag",
    "no_return",
    "discontinuities",
    "discontinuity_frames",
    "engine",
    "media",
    "playout",
    "blocks",
    "missed",
    "overruns",
    "max_process_us",
];

#[test]
fn a_fresh_device_s_status_is_numbers_only() {
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_on(&device, Arc::new(Unready));
    let s = status(&media);
    for word in strings_of_json(&s) {
        assert!(WORDS.contains(&word.as_str()), "{word:?} in {s}");
    }
    assert_eq!(
        s,
        "{\"clock\":{\"ready\":0,\"epoch\":0,\"polls\":0,\"re_anchors\":0,\"holdovers\":0,\
         \"holdover\":0,\"freq_ppb\":-750,\"source\":\"dsync\"},\"requests\":[],\"sent\":[],\"uncounted\":0,\
         \"tx\":[],\"rx\":[],\"rx_poll_errors\":0,\"channels\":[],\"sources\":[],\"checks\":[]}"
    );
}

#[test]
fn the_status_names_the_clock_requests_channels_sources_and_checks() {
    let dir = tempfile::tempdir().unwrap();
    let mut device = device_at(dir.path());
    let cfg = device.cfg.clone();
    device
        .state
        .set_subscription(&cfg, 2, Some(sub("TX 4", "peer-device")));
    let clock = Arc::new(ManualClock::new(T));
    let media = media_on(&device, clock.clone());
    media.clock_stats.record(Event::Ready { k_ns: 1 });
    media.clock_stats.record(Event::Holdover);
    media
        .clock_stats
        .record(Event::ReAnchored { delta_ns: 30_000 });
    clock.re_anchor(30_000);
    let peer = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 11));
    media.requests.received(4440, peer, 0x2200);
    media.requests.sent(peer, 0x0100);
    media.tx.set_source(0, Source::Counter);
    media.tx.set_source(
        2,
        Source::Sine {
            hz: 997.5,
            dbfs: -26.0,
        },
    );
    media.tx.set_source(3, Source::Echo { rx: 1, offset: 192 });
    media.tx.set_source(4, Source::Silence);
    {
        let mut c = lock(&media.checks);
        c.set(0, Some(CheckKind::Counter));
        c.set(4, Some(CheckKind::Sine { hz: 440.0 }));
        // RX 1 has played: its counter check has numbers.
        let rings = media.rx.rings();
        c.step(
            1_096,
            rings,
            &[Some(Playout {
                latency: 96,
                bits: 24,
            })],
        );
        c.step(
            1_196,
            rings,
            &[Some(Playout {
                latency: 96,
                bits: 24,
            })],
        );
    }
    // RX 2's ring was written (a channel listed without a subscription).
    media.rx.rings()[1].write_at(5, &[7, 8]);
    let s = status(&media);
    for word in strings_of_json(&s) {
        assert!(WORDS.contains(&word.as_str()), "{word:?} in {s}");
    }
    assert!(
        s.starts_with(
            "{\"clock\":{\"ready\":1,\"epoch\":1,\"polls\":3,\"re_anchors\":1,\"holdovers\":1,\
             \"holdover\":0,\"freq_ppb\":-750,\"source\":\"dsync\"},\
             \"requests\":[{\"port\":4440,\"peer\":1,\"op\":8704,\"n\":1}],\
             \"sent\":[{\"peer\":1,\"op\":256,\"n\":1}],\"uncounted\":0,\"tx\":[],\"rx\":[],"
        ),
        "{s}"
    );
    // RX 1 read by its check (holes), RX 2 written, RX 3 subscribed
    // (Unresolved's code); the rest untouched and left out.
    assert!(
        s.contains(
            "\"channels\":[\
             {\"rx\":1,\"status\":0,\"written\":0,\"holes\":100,\"late\":0,\"duplicate\":0,\"early\":0},\
             {\"rx\":2,\"status\":0,\"written\":2,\"holes\":0,\"late\":0,\"duplicate\":0,\"early\":0},\
             {\"rx\":3,\"status\":1,\"written\":0,\"holes\":0,\"late\":0,\"duplicate\":0,\"early\":0}]"
        ),
        "{s}"
    );
    assert!(
        s.contains(
            "\"sources\":[{\"tx\":1,\"source\":\"counter\"},\
             {\"tx\":3,\"source\":\"sine\",\"hz\":997.5,\"dbfs\":-26},\
             {\"tx\":4,\"source\":\"echo\",\"rx\":2,\"offset\":192}]"
        ),
        "{s}"
    );
    assert!(
        s.ends_with(
            "\"checks\":[\
             {\"rx\":1,\"check\":\"counter\",\"skipped\":0,\"checked\":0,\"blind\":100,\
             \"mismatch\":0,\"mismatch_frames\":0,\"dropout\":0,\"dropout_frames\":0,\
             \"slips\":0,\"last_slip\":0,\"lag\":null,\"no_return\":0},\
             {\"rx\":5,\"check\":\"sine\",\"hz\":440,\"skipped\":0,\"waiting\":1}]}"
        ),
        "{s}"
    );
    assert_eq!(value_of(&s, "freq_ppb"), "-750");
}

#[test]
fn the_status_takes_none_of_what_the_heartbeat_takes() {
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_on(&device, Arc::new(ManualClock::new(T)));
    media.rx.rings()[0].write_at(1, &[-1_000_000]);
    let _ = status(&media);
    let _ = status(&media);
    assert_eq!(media.rx.rings()[0].take_peak(), 1_000_000);
}

#[test]
fn the_status_shows_the_packets_judge_in_ns_numbers_only() {
    use crate::media::rx::{Receiver, RxPlan};
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    // T: 96 000 000 samples at 96 kHz, a whole second.
    let media = media_on(&device, Arc::new(ManualClock::new(T)));
    let lo = Ipv4Addr::LOCALHOST;
    let plan = RxPlan {
        src: Some(lo),
        bps: 3,
        nslots: 1,
        latency: 96,
        delay: 0,
        block: 0,
        live: true,
        generation: 1,
    };
    media.rx.publish(
        |slot| if slot == 3 { plan } else { RxPlan::default() },
        |_| None,
    );
    let mut receiver = Receiver::new(Arc::clone(&media.rx)).unwrap();
    let sender = std::net::UdpSocket::bind((lo, 0)).unwrap();
    // Two packets of flow 4, 40 and 25 samples late: one frame each, so
    // 14 frames between them never came (a gap).
    for pos in [96_000_000 - 40, 96_000_000 - 25] {
        let mut buf = [0u8; 64];
        let (secs, sub) = crate::clock::stamp(pos, 96_000);
        let n = crate::media::packet::write(&mut buf, secs, sub, 1, 3, 1, |_, _| 0);
        sender
            .send_to(&buf[..n], media.rx.local_addr(3).unwrap())
            .unwrap();
    }
    let deadline = std::time::Instant::now() + BOUND;
    while media.rx.lane(3).packets < 2 {
        receiver.wait(std::time::Duration::from_millis(20)).unwrap();
        receiver.pass(Some(T), 0);
        assert!(
            std::time::Instant::now() < deadline,
            "the packets never came"
        );
    }
    // Before the heartbeat took a second: no judge, the key absent.
    let s = status(&media);
    assert!(
        s.contains("\"unplaced\":0,\"lateness\":0,\"lateness_peak\":40,\"keepalives\":0,"),
        "{s}"
    );
    media.rx.take_max_lateness(3);
    let s = status(&media);
    for word in strings_of_json(&s) {
        assert!(WORDS.contains(&word.as_str()), "{word:?} in {s}");
    }
    // The least: 25 samples, 260 416 ns (the stamp at 999 999 739 584 ns,
    // rounded up); the largest 40 samples.
    assert!(
        s.contains(&format!(
            "\"rx\":[{{\"flow\":4,\"granted\":0,\"fpp\":0,\"channels\":0,\"latency\":0,\
             \"playout\":0,\"delay\":0,\"late_after\":0,\"advertised\":0,\
             \"below_advertised\":0,\"packets\":2,\"refused\":0,\"stray\":0,\"out_of_order\":0,\"gaps\":1,\"gap_frames\":14,{DROPS}\"late\":0,\"late_at_read\":0,\
             \"unplaced\":0,\"lateness\":40,\"lateness_peak\":40,\"lateness_min_ns\":260416,\"keepalives\":0,\
             \"keepalive_errors\":0,\"recv_errors\":0}}],"
        )),
        "{s}"
    );
    // The status takes nothing: read again, the same second.
    assert_eq!(value_of(&status(&media), "lateness_min_ns"), "260416");
}

#[test]
fn the_status_lists_the_flows_of_both_sides() {
    use crate::flows::control::FlowRequest;
    use crate::subscribe::{Action, Answer};
    let dir = tempfile::tempdir().unwrap();
    let mut device = device_at(dir.path());
    let cfg = device.cfg.clone();
    device
        .state
        .set_subscription(&cfg, 0, Some(sub("TX 1", "peer-device")));
    let media = media_on(&device, Arc::new(Unready));
    // A receiver's TX flow in our first slot: 3 channels, 32 frames.
    let rx_addr = std::net::SocketAddrV4::new(Ipv4Addr::LOCALHOST, 50_000);
    {
        let mut c = media.tx.lock();
        let req = FlowRequest {
            rx_host: "peer-device".into(),
            rx_flow_name: "1_0".into(),
            sample_rate: 96_000,
            bits: 24,
            channels: vec![Some(1), Some(2), None],
            fpp: 32,
            rx: rx_addr,
        };
        let (slot, _) = c.table.add(&req, 0).unwrap();
        assert_eq!(slot, 0);
    }
    // Our RX flow, asked for and granted.
    {
        let mut m = lock(&media.manager);
        let records = crate::daemon::media::tests::Records::default().with(
            "peer-device",
            "TX 1",
            crate::daemon::media::tests::advert(
                std::net::SocketAddrV4::new(Ipv4Addr::LOCALHOST, 4455),
                1,
            ),
        );
        let actions = m.tick(1, &media.rx.packets(), &records);
        let Some(Action::Request { flow, .. }) = actions.first() else {
            panic!("{actions:?}");
        };
        m.answered(*flow, Answer::Granted([0; 6]), 2);
    }
    let s = status(&media);
    assert!(
        s.contains(
            "\"tx\":[{\"flow\":1,\"live\":1,\"fpp\":32,\"channels\":3,\"packets\":0,\
             \"send_errors\":0,\"send_retries\":0,\"keepalives\":0,\"rebootstraps\":0,\
             \"holes\":0,\"late_bins\":[0,0,0,0,0,0],\"late_max_us\":0,\"send_max_us\":0,\
             \"age_max_us\":0,\"over_latency\":0,\"receiver_latency_us\":0,\"direct\":0,\
             \"early\":0}]"
        ),
        "{s}"
    );
    assert!(
        s.contains(&format!(
            "\"rx\":[{{\"flow\":1,\"granted\":1,\"fpp\":32,\"channels\":2,\"latency\":96,\
             \"playout\":96,\"delay\":0,\"late_after\":128,\"advertised\":96,\
             \"below_advertised\":0,\"packets\":0,\"refused\":0,\"stray\":0,\"out_of_order\":0,\"gaps\":0,\"gap_frames\":0,{DROPS}\"late\":0,\"late_at_read\":0,\
             \"unplaced\":0,\"lateness\":0,\"lateness_peak\":0,\"keepalives\":0,\"keepalive_errors\":0,\
             \"recv_errors\":0}}],\"rx_poll_errors\":0,"
        )),
        "{s}"
    );
    // RX 1 is InProgress: granted, nothing heard yet.
    assert!(s.contains("{\"rx\":1,\"status\":8,"), "{s}");
    // An expired TX flow is listed, not live.
    media.tx.lock().table.expire(10_000_000_000);
    assert!(status(&media).contains("\"tx\":[{\"flow\":1,\"live\":0,"));
}

#[test]
fn answers_set_sources_and_checks_and_refuse_with_a_reason() {
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_on(&device, Arc::new(Unready));
    assert_eq!(answer("source 2 counter", &media), "ok");
    assert_eq!(media.tx.source(1), Some(Source::Counter));
    assert_eq!(answer("source 2 silence", &media), "ok");
    assert_eq!(media.tx.source(1), Some(Source::Silence));
    assert_eq!(answer("check 3 sine 1000", &media), "ok");
    assert_eq!(
        lock(&media.checks).views()[0].kind,
        CheckKind::Sine { hz: 1000.0 }
    );
    assert_eq!(answer("check 3 off", &media), "ok");
    assert!(lock(&media.checks).views().is_empty());
    assert!(answer("status", &media).starts_with("{\"clock\":"));
    assert_eq!(
        answer("source 17 counter", &media),
        "error: source: no such TX channel"
    );
    assert_eq!(answer("zyxqw", &media), format!("error: {UNKNOWN}"));
    // Nothing changed by a refusal.
    assert_eq!(media.tx.source(16), None);
    assert_eq!(media.tx.source(1), Some(Source::Silence));
}

#[test]
fn source_engine_on_a_channel_without_an_engine_ring_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_with_engine(&device, Arc::new(Unready), &[0, 3]);
    assert_eq!(answer("source 4 engine", &media), "ok");
    assert_eq!(media.tx.source(3), Some(Source::Engine));
    assert_eq!(answer("source 1 engine", &media), "ok");
    assert_eq!(
        answer("source 2 engine", &media),
        format!("error: {NO_ENGINE}")
    );
    assert_eq!(media.tx.source(1), Some(Source::Silence), "nothing changed");
    // An engine channel switched to a test source and back.
    assert_eq!(answer("source 4 counter", &media), "ok");
    assert_eq!(media.tx.source(3), Some(Source::Counter));
    assert_eq!(answer("source 4 engine", &media), "ok");
    assert_eq!(media.tx.source(3), Some(Source::Engine));
    // The status words it `engine`.
    let s = status(&media);
    assert!(
        s.contains(
            "\"sources\":[{\"tx\":1,\"source\":\"engine\"},\
             {\"tx\":4,\"source\":\"engine\"}]"
        ),
        "{s}"
    );
    // The standalone device has no engine ring at all.
    let standalone = media_on(&device, Arc::new(Unready));
    assert_eq!(
        answer("source 1 engine", &standalone),
        format!("error: {NO_ENGINE}")
    );
    assert_eq!(standalone.tx.source(0), Some(Source::Silence));
}

/// A processor that leaves its outputs silent.
struct Silent;

impl dstack_audio_io::Process for Silent {
    fn process(&mut self, _block: &mut dstack_audio_io::Block<'_>) {}
}

const ENGINE_TIMING: crate::stream::Timing = crate::stream::Timing {
    block: 32,
    playout: 96,
    margin: 96,
};

#[test]
fn the_status_has_no_engine_until_one_is_attached() {
    use crate::stream::PumpStats;
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_with_engine(&device, Arc::new(Unready), &[0]);
    let before = status(&media);
    assert!(!before.contains("engine"), "{before}");
    assert!(before.ends_with("\"checks\":[]}"), "{before}");
    media.attach_engine(Arc::new(PumpStats::new(ENGINE_TIMING, 96_000)));
    let after = status(&media);
    assert_eq!(
        after,
        format!(
            "{},\"engine\":{{\"media\":0,\"offset\":224,\"playout\":96,\"blocks\":0,\
             \"late\":0,\"missed\":0,\"overruns\":0,\"rebootstraps\":0,\"max_process_us\":0}}}}",
            before.strip_suffix('}').unwrap()
        )
    );
}

#[test]
fn the_engine_status_is_numbers_only() {
    use crate::stream::{Map, Pump, PumpStats};
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_with_engine(&device, Arc::new(Unready), &[0]);
    let stats = Arc::new(PumpStats::new(ENGINE_TIMING, 96_000));
    media.attach_engine(Arc::clone(&stats));
    let mut pump = Pump::new(
        Silent,
        Map {
            rx: vec![0],
            tx: vec![0],
        },
        ENGINE_TIMING,
        24,
        Arc::clone(&media.rx),
        Arc::clone(&media.tx),
        Arc::clone(&stats),
    );
    // On the media clock: a bootstrap, two late blocks, then a step far past
    // the margin (a re-bootstrap that skips blocks).
    let wake = pump.media(T, 0);
    pump.media(wake + 64, 0);
    pump.media(wake + 64 + 10_000, 0);
    let v = stats.view();
    assert!(
        v.media && v.blocks > 3 && v.late == 2 && v.missed > 0 && v.rebootstraps == 1,
        "{v:?}"
    );
    let engine = |v: crate::stream::PumpView| {
        format!(
            "\"engine\":{{\"media\":{},\"offset\":224,\"playout\":96,\"blocks\":{},\
             \"late\":{},\"missed\":{},\"overruns\":{},\"rebootstraps\":{},\
             \"max_process_us\":{}}}}}",
            u8::from(v.media),
            v.blocks,
            v.late,
            v.missed,
            v.overruns,
            v.rebootstraps,
            v.max_process_us
        )
    };
    let s = status(&media);
    for word in strings_of_json(&s) {
        assert!(WORDS.contains(&word.as_str()), "{word:?} in {s}");
    }
    assert!(s.ends_with(&engine(v)), "{s}");
    // Pacing itself (the clock not ready): `media` 0, one more block.
    pump.free();
    let paced = stats.view();
    assert!(!paced.media && paced.blocks == v.blocks + 1, "{paced:?}");
    let s = status(&media);
    assert!(s.ends_with(&engine(paced)), "{s}");
}

#[test]
fn the_app_object_is_one_compact_json_object_and_nothing_else() {
    assert_eq!(
        app_object("{\n  \"line\": \"ASIO 64\",\n  \"state\": \"ok\"\n}"),
        Some(r#"{"line":"ASIO 64","state":"ok"}"#.to_string())
    );
    assert_eq!(app_object("{}"), Some("{}".to_string()));
    for refused in ["", "[]", "\"ok\"", "1", "null", "{\"line\":", "{} {}"] {
        assert_eq!(app_object(refused), None, "{refused:?}");
    }
    // The size counts the compact form: `{"x":"…"}` is 8 bytes and the text.
    let at_most = format!("{{\"x\":\"{}\"}}", "a".repeat(APP_MAX - 8));
    assert_eq!(at_most.len(), APP_MAX);
    assert_eq!(app_object(&at_most), Some(at_most.clone()));
    let spaced = format!("{{ \"x\" : \"{}\" }}", "a".repeat(APP_MAX - 8));
    assert_eq!(app_object(&spaced), Some(at_most), "white space is free");
    let over = format!("{{\"x\":\"{}\"}}", "a".repeat(APP_MAX - 7));
    assert_eq!(app_object(&over), None);
}

#[test]
fn the_status_carries_the_app_s_object_last_and_a_later_hook_replaces_it() {
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_on(&device, Arc::new(Unready));
    let before = status(&media);
    assert_eq!(media.app_status(), None);
    assert!(!before.contains("app"), "{before}");
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    media.set_app_status(move || {
        counted.fetch_add(1, Ordering::Relaxed);
        "{\"line\": \"ASIO 64\",\n \"state\": \"warn\", \"n\": 2}".to_string()
    });
    let after = status_until(&media, |s| s.contains("\"app\""));
    assert_eq!(
        after,
        format!(
            "{},\"app\":{{\"line\":\"ASIO 64\",\"n\":2,\"state\":\"warn\"}}}}",
            before.strip_suffix('}').unwrap()
        )
    );
    assert!(calls.load(Ordering::Relaxed) >= 1);
    assert!(serde_json::from_str::<serde_json::Value>(&after).is_ok());
    media.set_app_status(|| r#"{"state":"error"}"#.to_string());
    let error = |s: &str| s.ends_with(",\"app\":{\"state\":\"error\"}}");
    status_until(&media, error);
    let first = calls.load(Ordering::Relaxed);
    let end = std::time::Instant::now() + std::time::Duration::from_millis(1300);
    while std::time::Instant::now() < end {
        assert!(error(&status(&media)));
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(
        calls.load(Ordering::Relaxed),
        first,
        "the first hook is gone"
    );
}

/// Reads the status every 10 ms until `done` holds for it (the app hook's
/// thread refreshes its part in the background), within a generous 10 s.
fn status_until(media: &Media, done: impl Fn(&str) -> bool) -> String {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let s = status(media);
        if done(&s) {
            return s;
        }
        assert!(std::time::Instant::now() < end, "never came: {s}");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn an_app_part_that_is_no_object_is_left_out_and_warned_once() {
    crate::test_log::undecided();
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_on(&device, Arc::new(Unready));
    let before = status(&media);
    let good = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let flip = Arc::clone(&good);
    media.set_app_status(move || {
        if flip.load(Ordering::Relaxed) {
            r#"{"state":"ok"}"#.to_string()
        } else {
            "[\"not an object\"]".to_string()
        }
    });
    let ok = |s: &str| s.ends_with(",\"app\":{\"state\":\"ok\"}}");
    // A good object warns nothing.
    let logs = crate::test_log::capture_warnings(|| {
        status_until(&media, ok);
    });
    assert_eq!(logs, "");
    good.store(false, Ordering::Relaxed);
    let logs = crate::test_log::capture_warnings(|| {
        status_until(&media, |s| s == before);
        for _ in 0..3 {
            assert_eq!(status(&media), before);
        }
    });
    assert_eq!(
        logs.matches("the app's status part is no JSON object")
            .count(),
        1,
        "{logs}"
    );
    // Good again: carried again, nothing more warned.
    good.store(true, Ordering::Relaxed);
    let logs = crate::test_log::capture_warnings(|| {
        status_until(&media, ok);
    });
    assert_eq!(logs, "");
    // A new hook that is refused is warned again, once: the status reads
    // as without a hook for two refreshes (its first call comes at once).
    media.set_app_status(|| "not json".to_string());
    let logs = crate::test_log::capture_warnings(|| {
        let end = std::time::Instant::now() + 2 * crate::daemon::media::app::REFRESH;
        while std::time::Instant::now() < end {
            assert_eq!(status(&media), before);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    });
    assert_eq!(
        logs.matches("the app's status part is no JSON object")
            .count(),
        1,
        "{logs}"
    );
}

#[cfg(unix)]
mod socket {
    use super::*;

    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    use std::time::Duration;

    use tokio::net::UnixStream;

    /// Sends `bytes` (then ends its side when `end`), serves the session
    /// (bounded) and reads everything it answered before it closed.
    async fn exchange(media: &Media, bytes: &[u8], end: bool, wait: Duration) -> String {
        use std::io::{Read, Write};
        let (mut ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        ours.write_all(bytes).unwrap();
        if end {
            ours.shutdown(std::net::Shutdown::Write).unwrap();
        }
        theirs.set_nonblocking(true).unwrap();
        let theirs = UnixStream::from_std(theirs).unwrap();
        tokio::time::timeout(Duration::from_secs(5), session(theirs, media, wait))
            .await
            .expect("the session ends within the bound");
        ours.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut out = String::new();
        ours.read_to_string(&mut out).unwrap();
        out
    }

    fn media() -> (tempfile::TempDir, Arc<Media>) {
        let dir = tempfile::tempdir().unwrap();
        let device = device_at(dir.path());
        let media = media_on(&device, Arc::new(Unready));
        (dir, media)
    }

    #[tokio::test]
    async fn a_session_answers_one_line_and_closes() {
        let (_dir, media) = media();
        let out = exchange(&media, b"source 1 counter\nstatus\n", false, WAIT).await;
        assert_eq!(out, "ok\n", "one request a connection");
        assert_eq!(media.tx.source(0), Some(Source::Counter));
        let out = exchange(&media, b"status\r\n", false, WAIT).await;
        assert!(
            out.starts_with("{\"clock\":{") && out.ends_with("]}\n"),
            "{out}"
        );
        assert_eq!(out.matches('\n').count(), 1);
        // A request without its newline, its sender done: still a request.
        let out = exchange(&media, b"check 1 counter", true, WAIT).await;
        assert_eq!(out, "ok\n");
        let out = exchange(&media, b"zyxqw\n", false, WAIT).await;
        assert_eq!(out, format!("error: {UNKNOWN}\n"));
    }

    #[tokio::test]
    async fn a_long_a_silent_or_a_garbled_request_is_an_error_line() {
        let (_dir, media) = media();
        let long = format!("status {}\n", "x".repeat(MAX_LINE));
        let out = exchange(&media, long.as_bytes(), false, WAIT).await;
        assert_eq!(out, "error: the request is longer than 256 bytes\n");
        // Exactly the most a line may be: read (and judged).
        let exact = format!("{:<width$}\n", "status", width = MAX_LINE);
        let out = exchange(&media, exact.as_bytes(), false, WAIT).await;
        assert!(out.starts_with('{'), "{out}");
        let out = exchange(&media, &[b'x'; MAX_LINE + 1], true, WAIT).await;
        assert_eq!(out, "error: the request is longer than 256 bytes\n");
        // A sender that goes on without a newline is cut off at once, not
        // at the end of the wait.
        let started = tokio::time::Instant::now();
        let out = exchange(&media, &[b'x'; MAX_LINE + 1], false, BOUND).await;
        assert_eq!(out, "error: the request is longer than 256 bytes\n");
        assert!(started.elapsed() < Duration::from_secs(1));
        // The most a line may be, ended by the sender instead of a newline.
        let exact = format!("{:<width$}", "status", width = MAX_LINE);
        let out = exchange(&media, exact.as_bytes(), true, WAIT).await;
        assert!(out.starts_with('{'), "{out}");
        let out = exchange(&media, b"\xff\xfe\n", false, WAIT).await;
        assert_eq!(out, "error: the request is not UTF-8\n");
        let started = tokio::time::Instant::now();
        let out = exchange(&media, b"stat", false, Duration::from_millis(50)).await;
        assert_eq!(out, "error: no request line within the wait\n");
        assert!(started.elapsed() < Duration::from_secs(2));
        // An empty line is a request (and refused).
        let out = exchange(&media, b"\n", false, WAIT).await;
        assert_eq!(out, format!("error: {UNKNOWN}\n"));
    }

    #[tokio::test]
    async fn a_client_gone_before_its_answer_ends_the_session_at_once() {
        use std::io::Write;
        let (_dir, media) = media();
        let (mut ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        ours.write_all(b"status\n").unwrap();
        drop(ours);
        theirs.set_nonblocking(true).unwrap();
        let theirs = UnixStream::from_std(theirs).unwrap();
        // The answer's write fails (no reader): the session ends with it,
        // not at the end of its wait.
        let started = tokio::time::Instant::now();
        tokio::time::timeout(BOUND, session(theirs, &media, BOUND))
            .await
            .expect("the session ends within the bound");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn the_control_socket_is_0660() {
        // Phase D: the service user's socket, open to its group (the admin
        // user's `ctl status`, the live guard, health); who may reach it at
        // all is the 0750 runtime folder's group. Whatever the umask.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("control.sock");
        let listener = bind_control(&path).unwrap();
        let meta = std::fs::symlink_metadata(&path).unwrap();
        assert!(meta.file_type().is_socket());
        assert_eq!(meta.permissions().mode() & 0o7777, 0o660);
        // A stale socket replaced is 0660 again.
        drop(listener);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let again = bind_control(&path).unwrap();
        let meta = std::fs::symlink_metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o7777, 0o660);
        drop(again);
    }

    #[tokio::test]
    async fn the_control_socket_serves_and_replaces_a_stale_one() {
        let (dir, media) = media();
        let path = dir.path().join("control.sock");
        let listener = bind_control(&path).unwrap();
        let meta = std::fs::symlink_metadata(&path).unwrap();
        assert!(meta.file_type().is_socket());
        // Served: a client gets its answer.
        let server = tokio::spawn(serve_control(listener, Arc::clone(&media)));
        let mut stream = std::os::unix::net::UnixStream::connect(&path).unwrap();
        let answer = tokio::task::spawn_blocking(move || {
            use std::io::{Read, Write};
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream.write_all(b"status\n").unwrap();
            let mut out = String::new();
            stream.read_to_string(&mut out).unwrap();
            out
        })
        .await
        .unwrap();
        assert!(answer.starts_with("{\"clock\":"), "{answer}");
        server.abort();
        // An earlier run's socket is replaced.
        drop(server.await);
        let again = bind_control(&path).unwrap();
        drop(again);
        // Anything else at the path is refused and kept.
        let file = dir.path().join("file");
        std::fs::write(&file, "keep").unwrap();
        let err = bind_control(&file).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep");
        // A folder that is not there is the error.
        let err = bind_control(&dir.path().join("none/control.sock")).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}

#[test]
fn a_lane_s_times_show_in_whole_microseconds() {
    let lane = LaneCounts {
        packets: 7,
        send_retries: 3,
        late_max_ns: 204_999,
        send_max_ns: 56_001,
        age_max_ns: 416_700,
        over_latency: 80,
        receiver_latency_ns: 10_000_999,
        direct: 5,
        early: 4,
        ..LaneCounts::default()
    };
    let mut j = Json::new();
    j.open(None, '{');
    lane_fields(&mut j, &lane);
    j.close('}');
    assert_eq!(
        j.out,
        "{\"packets\":7,\"send_errors\":0,\"send_retries\":3,\"keepalives\":0,\"rebootstraps\":0,\
         \"holes\":0,\"late_bins\":[0,0,0,0,0,0],\"late_max_us\":204,\"send_max_us\":56,\
         \"age_max_us\":416,\"over_latency\":80,\"receiver_latency_us\":10000,\"direct\":5,\
         \"early\":4}"
    );
}

/// dstack #104 (dtray): the local endpoint answers `status` and nothing
/// else; a test request through it changes nothing.
#[test]
fn the_local_endpoint_answers_status_only() {
    let dir = tempfile::tempdir().unwrap();
    let media = media_on(&device_at(dir.path()), Arc::new(Unready));
    assert!(local::reply("status", &media).starts_with("{\"clock\":{"));
    assert!(local::reply(" status \r", &media).starts_with("{\"clock\":{"));
    let refused = format!("error: {}", local::ONLY_STATUS);
    assert_eq!(local::reply("source 1 counter", &media), refused);
    assert_eq!(local::reply("statusx", &media), refused);
    assert_eq!(local::reply("", &media), refused);
    assert_eq!(media.tx.source(0), Some(Source::Silence));
}

/// The endpoint over TCP on 127.0.0.1: one line in, one line out, closed;
/// a request without its newline counts once its sender is done.
#[tokio::test]
async fn the_local_endpoint_serves_one_line_a_connection() {
    use std::io::{Read, Write};
    let dir = tempfile::tempdir().unwrap();
    let media = media_on(&device_at(dir.path()), Arc::new(Unready));
    let listener = local::bind().unwrap();
    let at = listener.local_addr().unwrap();
    assert!(at.ip().is_loopback());
    tokio::spawn(local::serve(listener, Arc::clone(&media)));
    let ask = move |req: &'static [u8], close_write: bool| {
        tokio::task::spawn_blocking(move || {
            let mut s = std::net::TcpStream::connect(at).unwrap();
            s.set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            s.write_all(req).unwrap();
            if close_write {
                s.shutdown(std::net::Shutdown::Write).unwrap();
            }
            let mut out = String::new();
            s.read_to_string(&mut out).unwrap();
            out
        })
    };
    let out = ask(b"status\nsource 1 counter\n", false).await.unwrap();
    assert!(
        out.starts_with("{\"clock\":{") && out.ends_with("}\n"),
        "{out}"
    );
    assert_eq!(out.matches('\n').count(), 1);
    let out = ask(b"status", true).await.unwrap();
    assert!(out.starts_with("{\"clock\":{"), "{out}");
    let out = ask(b"source 1 counter\n", false).await.unwrap();
    assert_eq!(out, format!("error: {}\n", local::ONLY_STATUS));
    assert_eq!(media.tx.source(0), Some(Source::Silence));
}

/// iemmixer (4.11.2026): the status names the clock source a linked device
/// actually uses (the follower in process, the dsync service, or none yet),
/// not "dsync" whichever it is; the owner's acceptance reads it there.
#[test]
fn the_clock_source_names_what_a_linked_device_uses() {
    use crate::clock::linked::Source;
    assert_eq!(clock_source_word(true, None), "system");
    assert_eq!(clock_source_word(false, None), "dsync");
    assert_eq!(clock_source_word(false, Some(Source::Internal)), "internal");
    assert_eq!(clock_source_word(false, Some(Source::Service)), "service");
    assert_eq!(clock_source_word(false, Some(Source::None)), "none");
    let stats = crate::clock::ClockStats::default();
    assert_eq!(stats.linked(), None, "a device without the follower");
    stats.set_linked(Source::Internal);
    assert_eq!(stats.linked(), Some(Source::Internal));
    stats.set_linked(Source::Service);
    assert_eq!(stats.linked(), Some(Source::Service));
}

/// iemmixer (4.11.2026, v0.6.29 live): stopping the dsync service for 30 s
/// showed holdover 1 and re_anchors 1 at the top of the clock status while
/// the device ran on the follower, and the follower's own counts were
/// nowhere. The top level now describes the source in use, and a linked
/// clock names each source's counts on its own.
#[test]
fn the_clock_counts_describe_the_source_in_use_and_each_source_apart() {
    use crate::clock::ClockCounts;
    use crate::clock::linked::Source;
    let service = ClockCounts {
        polls: 40,
        re_anchors: 1,
        holdovers: 1,
        in_holdover: true,
    };
    let follower = ClockCounts {
        polls: 300,
        re_anchors: 0,
        holdovers: 0,
        in_holdover: false,
    };
    assert_eq!(
        clock_counts_in_use(Some(Source::Internal), service, Some(follower)),
        follower
    );
    assert_eq!(
        clock_counts_in_use(Some(Source::Service), service, Some(follower)),
        service
    );
    assert_eq!(
        clock_counts_in_use(Some(Source::None), service, Some(follower)),
        service
    );
    assert_eq!(clock_counts_in_use(None, service, Some(follower)), service);
    assert_eq!(
        clock_counts_in_use(Some(Source::Internal), service, None),
        service
    );

    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_on(&device, Arc::new(Unready));
    media.clock_stats.record(Event::Ready { k_ns: 1 });
    media.clock_stats.record(Event::Holdover);
    // Not linked: no per-source objects, the poller's counts on top.
    let s = status(&media);
    assert!(
        s.contains("\"polls\":2,\"re_anchors\":0,\"holdovers\":1,\"holdover\":1,"),
        "{s}"
    );
    assert!(!s.contains("\"service\":{"), "{s}");
    assert!(!s.contains("\"follower\""), "{s}");
    // Linked, on the follower.
    media.clock_stats.set_linked(Source::Internal);
    media
        .clock_stats
        .set_follower(Some(crate::clock::FollowerCounts {
            counts: ClockCounts {
                polls: 300,
                re_anchors: 4,
                holdovers: 3,
                in_holdover: false,
            },
            absorbed: 2,
            locked: true,
            settled: false,
            offset_ns: Some(-39_806),
            recent_abs_e_ns: None,
            pull_ns: -88_000,
        }));
    assert_eq!(
        media.clock_stats.follower().map(|c| c.counts.polls),
        Some(300)
    );
    let s = status(&media);
    for word in strings_of_json(&s) {
        assert!(WORDS.contains(&word.as_str()), "{word:?} in {s}");
    }
    assert!(
        s.contains(
            "\"polls\":300,\"re_anchors\":4,\"holdovers\":3,\"holdover\":0,\"freq_ppb\":-750,\
             \"source\":\"internal\",\
             \"service\":{\"polls\":2,\"re_anchors\":0,\"holdovers\":1,\"holdover\":1},\
             \"follower\":{\"windows\":300,\"re_anchors\":4,\"holdovers\":3,\"holdover\":0,\"absorbed\":2,\
             \"locked\":1,\"settled\":0,\"offset_ns\":-39806,\"recent_abs_e_ns\":null,\"pull_ns\":-88000}}"
        ),
        "{s}"
    );
    // On the service: its counts on top; the follower still named.
    media.clock_stats.set_linked(Source::Service);
    let s = status(&media);
    assert!(
        s.contains("\"polls\":2,\"re_anchors\":0,\"holdovers\":1,\"holdover\":1,"),
        "{s}"
    );
    assert!(s.contains("\"follower\":{\"windows\":300,"), "{s}");
    // A linked clock whose follower did not start: no follower object.
    media.clock_stats.set_follower(None);
    let s = status(&media);
    assert!(s.contains("\"service\":{\"polls\":2,"), "{s}");
    assert!(!s.contains("\"follower\""), "{s}");
}
