use super::*;

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::clock::{Event, ManualClock};
use crate::ctl::clock::{clock_counts_in_use, clock_source_word};
use crate::ctl::{Playout, local};
use crate::daemon::media::tests::{BOUND, T, Unready, device_at, media_on, media_with_engine, sub};

/// The status's `socket_drops` field: Linux only (`/proc/net/udp`).
const DROPS: &str = if cfg!(target_os = "linux") {
    "\"socket_drops\":0,"
} else {
    ""
};

/// The status's receive-stamp fields of a flow that received nothing:
/// Linux only (dstack #234, `SO_TIMESTAMPNS`).
const STAMPS_NONE: &str = if cfg!(target_os = "linux") {
    "\"wire_late\":0,\"wake_delay_us\":0,\"wake_delay_peak_us\":0,"
} else {
    ""
};

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

/// The strings of the status `text` besides the value of its top-level
/// `name` (dstack #209: the one value that is not a fixed word), which
/// must be `name`. Only that value is taken out: the name anywhere else
/// still fails the words check.
fn words_of(text: &str, name: &str) -> Vec<String> {
    let mut v: serde_json::Value = serde_json::from_str(text).expect("JSON");
    let own = v.as_object_mut().expect("an object").remove("name");
    assert_eq!(own, Some(name.into()), "{text}");
    strings_of_json(&v.to_string())
}

/// The words a status may hold besides numbers and the device's `name`
/// ([`words_of`] takes it out): its keys and the fixed words of sources and
/// checks.
const WORDS: &[&str] = &[
    "multicast",
    "joins",
    "join_errors",
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
    "tx_bytes",
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
    "device",
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
    "reordered",
    "gaps",
    "gap_frames",
    "socket_drops",
    "late",
    "late_at_read",
    "start_holes",
    "write_delay_p999_us",
    "write_delay_max_us",
    "wire_late",
    "wake_delay_us",
    "wake_delay_peak_us",
    "wake_delay_p50_us",
    "wake_delay_p99_us",
    "wake_delay_p999_us",
    "network_latency_min_us",
    "network_latency_p99_us",
    "network_latency_max_us",
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
fn a_fresh_device_s_status_is_its_name_and_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_on(&device, Arc::new(Unready));
    let s = status(&media);
    for word in words_of(&s, "test-device") {
        assert!(WORDS.contains(&word.as_str()), "{word:?} in {s}");
    }
    assert_eq!(
        s,
        "{\"clock\":{\"ready\":0,\"epoch\":0,\"polls\":0,\"re_anchors\":0,\"holdovers\":0,\
         \"holdover\":0,\"freq_ppb\":-750,\"source\":\"dsync\"},\"requests\":[],\"sent\":[],\"uncounted\":0,\
         \"tx\":[],\"tx_bytes\":0,\"name\":\"test-device\",\"rx\":[],\"rx_poll_errors\":0,\"channels\":[],\
         \"sources\":[],\"checks\":[]}"
    );
}

/// dstack #209: the status names the device as it is now (the name its ARC
/// record announces), so a local reader (the traffic watch, dtray) can tell
/// which device answered. A rename shows at the next status; the name is
/// escaped.
#[test]
fn the_status_names_the_device_as_it_is_now() {
    use crate::arc::{Flows, Running};
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_on(&device, Arc::new(Unready));
    let name_in = |media: &Media| {
        let v: serde_json::Value = serde_json::from_str(&status(media)).expect("JSON");
        v["name"].clone()
    };
    assert_eq!(name_in(&media), "test-device");
    let flows = Running {
        tx: Arc::clone(&media.tx),
        rx: Arc::clone(&media.manager),
    };
    flows.renamed("renamed-device");
    assert_eq!(name_in(&media), "renamed-device");
    flows.renamed("a\"b\\c\n");
    assert_eq!(name_in(&media), "a\"b\\c\n");
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
    for word in words_of(&s, "test-device") {
        assert!(WORDS.contains(&word.as_str()), "{word:?} in {s}");
    }
    assert!(
        s.starts_with(
            "{\"clock\":{\"ready\":1,\"epoch\":1,\"polls\":3,\"re_anchors\":1,\"holdovers\":1,\
             \"holdover\":0,\"freq_ppb\":-750,\"source\":\"dsync\"},\
             \"requests\":[{\"port\":4440,\"peer\":1,\"op\":8704,\"n\":1}],\
             \"sent\":[{\"peer\":1,\"op\":256,\"n\":1}],\"uncounted\":0,\"tx\":[],\"tx_bytes\":0,\
             \"name\":\"test-device\",\"rx\":[],"
        ),
        "{s}"
    );
    // RX 1 read by its check (start holes: nothing written, dstack #234),
    // RX 2 written, RX 3 subscribed (Unresolved's code); the rest untouched
    // and left out.
    assert!(
        s.contains(
            "\"channels\":[\
             {\"rx\":1,\"status\":0,\"written\":0,\"holes\":0,\"late\":0,\"duplicate\":0,\"early\":0,\"start_holes\":100},\
             {\"rx\":2,\"status\":0,\"written\":2,\"holes\":0,\"late\":0,\"duplicate\":0,\"early\":0,\"start_holes\":0},\
             {\"rx\":3,\"status\":1,\"written\":0,\"holes\":0,\"late\":0,\"duplicate\":0,\"early\":0,\"start_holes\":0}]"
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
        group: None,
    };
    media.rx.publish(
        |slot| if slot == 3 { plan } else { RxPlan::default() },
        |_| None,
    );
    let mut receiver = Receiver::new(Arc::clone(&media.rx)).unwrap();
    let sender = std::net::UdpSocket::bind((lo, 0)).unwrap();
    // Two packets of flow 4, 40 and 25 samples late: one frame each, so
    // 14 frames between them have not come: an open hole, not yet behind
    // the playout (96 samples), so no gap (dstack #278).
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
    // dstack #266: the transmitter's address is the one string value.
    let source = sender.local_addr().unwrap().to_string();
    for word in words_of(&s, "test-device") {
        assert!(
            WORDS.contains(&word.as_str()) || word == source,
            "{word:?} in {s}"
        );
    }
    // The receive-stamp fields (Linux): the wake delays are real time, so
    // read back; both packets were read after the kernel had them.
    let stamps = if cfg!(target_os = "linux") {
        let (wake, peak) = (
            value_of(&s, "wake_delay_us"),
            value_of(&s, "wake_delay_peak_us"),
        );
        assert!(wake.parse::<u64>().unwrap() <= peak.parse::<u64>().unwrap());
        // dstack #244: the measured network latency, each packet's arrival
        // (the pass's time less its wake delay) less its stamp: at most
        // the packets' lateness at the pass (40 and 25 samples, 416 and
        // 260 us), min <= p99 <= max.
        let net = ["min", "p99", "max"].map(|k| {
            value_of(&s, &format!("network_latency_{k}_us"))
                .parse::<u64>()
                .unwrap()
        });
        assert!(net[0] <= 260 && net[2] <= 416, "{net:?}");
        assert!(net[0] <= net[1] && net[1] <= net[2], "{net:?}");
        format!(
            "\"wire_late\":0,\"wake_delay_us\":{wake},\"wake_delay_peak_us\":{peak},\
             \"network_latency_min_us\":{},\"network_latency_p99_us\":{},\
             \"network_latency_max_us\":{},",
            net[0], net[1], net[2]
        )
    } else {
        String::new()
    };
    // The least: 25 samples, 260 416 ns (the stamp at 999 999 739 584 ns,
    // rounded up); the largest 40 samples.
    assert!(
        s.contains(&format!(
            "\"rx\":[{{\"flow\":4,\"source\":\"{source}\",\"granted\":0,\"multicast\":0,\"fpp\":0,\"channels\":0,\"latency\":0,\
             \"playout\":0,\"delay\":0,\"late_after\":0,\"advertised\":0,\
             \"below_advertised\":0,\"packets\":2,\"refused\":0,\"stray\":0,\"out_of_order\":0,\"reordered\":0,\"gaps\":0,\"gap_frames\":0,{DROPS}\"late\":0,\"late_at_read\":0,\"start_holes\":0,{stamps}\
             \"unplaced\":0,\"lateness\":40,\"lateness_peak\":40,\"lateness_min_ns\":260416,\"keepalives\":0,\
             \"keepalive_errors\":0,\"recv_errors\":0,\"joins\":0,\"join_errors\":0}}],"
        )),
        "{s}"
    );
    // The status takes nothing: read again, the same second.
    assert_eq!(value_of(&status(&media), "lateness_min_ns"), "260416");
}

#[test]
fn the_receive_stamp_keys_show_only_while_the_sockets_stamp() {
    use crate::media::rx::{Receiver, RxPlan};
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_on(&device, Arc::new(ManualClock::new(T)));
    // One packet on flow slot 1, so the status lists it.
    let lo = Ipv4Addr::LOCALHOST;
    let plan = RxPlan {
        src: Some(lo),
        bps: 3,
        nslots: 1,
        latency: 96,
        delay: 0,
        block: 0,
        live: false,
        generation: 1,
        group: None,
    };
    // RX 1 fed by it: a write delay needs a ring write (dstack #234).
    media.rx.publish(
        |slot| if slot == 0 { plan } else { RxPlan::default() },
        |rx| (rx == 0).then_some(crate::media::rx::RxRoute { flow: 0, slot: 0 }),
    );
    let mut receiver = Receiver::new(Arc::clone(&media.rx)).unwrap();
    let mut buf = [0u8; 64];
    let (secs, sub) = crate::clock::stamp(96_000_000, 96_000);
    let n = crate::media::packet::write(&mut buf, secs, sub, 1, 3, 1, |_, _| 0);
    std::net::UdpSocket::bind((lo, 0))
        .unwrap()
        .send_to(&buf[..n], media.rx.local_addr(0).unwrap())
        .unwrap();
    let deadline = std::time::Instant::now() + BOUND;
    while media.rx.lane(0).packets < 1 {
        receiver.wait(std::time::Duration::from_millis(20)).unwrap();
        receiver.pass(Some(T), 0);
        assert!(std::time::Instant::now() < deadline, "no packet");
    }
    assert_eq!(
        status(&media).contains("\"wake_delay_peak_us\":"),
        cfg!(target_os = "linux")
    );
    // The measured network latency (dstack #244) shows once the heartbeat
    // took a second with a stamped packet.
    assert!(!status(&media).contains("network_latency"));
    media.rx.take_max_lateness(0);
    assert_eq!(
        status(&media).contains("\"network_latency_p99_us\":"),
        cfg!(target_os = "linux")
    );
    // The wake percentiles (dstack #234) show once the RX minute watch
    // took a minute with a stamped packet, numbers only.
    assert!(!status(&media).contains("wake_delay_p50_us"));
    media.rx.take_minute(0);
    let s = status(&media);
    assert_eq!(
        s.contains("\"wake_delay_p50_us\":") && s.contains("\"wake_delay_p999_us\":"),
        cfg!(target_os = "linux"),
        "{s}"
    );
    // The write delays (dstack #234) likewise, after the wake percentiles.
    assert!(!s.contains("write_delay"), "{s}");
    media.rx.take_write_minute(0);
    let s = status(&media);
    assert_eq!(
        s.contains("\"wake_delay_p999_us\":")
            && s.contains("\"write_delay_p999_us\":")
            && s.contains("\"write_delay_max_us\":"),
        cfg!(target_os = "linux"),
        "{s}"
    );
    for word in words_of(&s, "test-device") {
        assert!(
            WORDS.contains(&word.as_str()) || word.starts_with("127.0.0.1:"),
            "{word:?} in {s}"
        );
    }
    media.rx.set_stamps(false);
    let s = status(&media);
    for key in [
        "wire_late",
        "wake_delay_us",
        "wake_delay_peak_us",
        "wake_delay_p50_us",
        "wake_delay_p99_us",
        "wake_delay_p999_us",
        "write_delay_p999_us",
        "write_delay_max_us",
        "network_latency_min_us",
        "network_latency_p99_us",
        "network_latency_max_us",
    ] {
        assert!(!s.contains(key), "{key} in {s}");
    }
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
             \"age_max_us\":0,\"over_latency\":0,\"receiver_latency_us\":0,\"direct\":0}]"
        ),
        "{s}"
    );
    assert!(
        s.contains(&format!(
            "\"rx\":[{{\"flow\":1,\"device\":\"peer-device\",\"granted\":1,\"multicast\":0,\"fpp\":32,\"channels\":2,\"latency\":96,\
             \"playout\":96,\"delay\":0,\"late_after\":128,\"advertised\":96,\
             \"below_advertised\":0,\"packets\":0,\"refused\":0,\"stray\":0,\"out_of_order\":0,\"reordered\":0,\"gaps\":0,\"gap_frames\":0,{DROPS}\"late\":0,\"late_at_read\":0,\"start_holes\":0,{STAMPS_NONE}\
             \"unplaced\":0,\"lateness\":0,\"lateness_peak\":0,\"keepalives\":0,\"keepalive_errors\":0,\
             \"recv_errors\":0,\"joins\":0,\"join_errors\":0}}],\"rx_poll_errors\":0,"
        )),
        "{s}"
    );
    // RX 1 is InProgress: granted, nothing heard yet.
    assert!(s.contains("{\"rx\":1,\"status\":8,"), "{s}");
    // An expired TX flow is listed, not live.
    media.tx.lock().table.expire(10_000_000_000);
    assert!(status(&media).contains("\"tx\":[{\"flow\":1,\"live\":0,"));
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
    for word in words_of(&s, "test-device") {
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
         \"age_max_us\":416,\"over_latency\":80,\"receiver_latency_us\":10000,\"direct\":5}"
    );
}

/// dstack #203: the status carries every slot's wire bytes, which a
/// neighbour's traffic watch on the PC reads.
#[test]
fn the_status_carries_the_tx_wire_bytes_for_a_neighbour() {
    let dir = tempfile::tempdir().unwrap();
    let media = media_on(&device_at(dir.path()), Arc::new(Unready));
    assert_eq!(crate::traffic::tx_bytes_of(&status(&media)), Some(0));
    media.tx.count_sent(0, 100);
    media.tx.count_sent(2, 58);
    let want = 158 + 2 * crate::media::tx::WIRE_OVERHEAD;
    assert_eq!(crate::traffic::tx_bytes_of(&status(&media)), Some(want));
    assert_eq!(
        crate::traffic::tx_bytes_of(&local::reply("status", &media)),
        Some(want)
    );
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
    for word in words_of(&s, "test-device") {
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
