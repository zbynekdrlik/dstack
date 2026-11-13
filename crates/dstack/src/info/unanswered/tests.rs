use super::*;

use std::net::Ipv4Addr;
use std::time::Duration;

use crate::test_log::capture_at;

const A: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 13));
const B: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 14));
const FIRST: &str = "info request not answered (told once";
const MINUTE: &str = "info requests not answered in a minute";

/// An info request (32-byte header) with `opcode` at bytes 24–31.
fn request(opcode: [u8; 8]) -> Vec<u8> {
    let mut d = vec![0u8; 32];
    d[24..32].copy_from_slice(&opcode);
    d
}

fn op(n: u16) -> Vec<u8> {
    let [hi, lo] = n.to_be_bytes();
    request([0x07, 0x34, hi, lo, 0, 0, 0, 0])
}

fn lines<'a>(text: &'a str, what: &str) -> Vec<&'a str> {
    text.lines().filter(|l| l.contains(what)).collect()
}

#[test]
fn a_sender_s_opcode_is_told_once_then_counted_in_one_line_a_minute() {
    let t = Instant::now();
    let mut u = Unanswered::new();
    let text = capture_at(tracing::Level::INFO, || {
        for k in 0..5 {
            u.record(t + Duration::from_secs(k), A, &op(0x100a));
        }
        u.record(t, B, &op(0x100a));
        u.record(t, A, &op(0x0099));
        // Not a minute yet: no summary.
        u.flush(t + WINDOW - Duration::from_millis(1));
    });
    let first = lines(&text, FIRST);
    assert_eq!(first.len(), 3, "{text}");
    assert!(
        first[0].contains("op=0734100a00000000 peer=1 from=192.0.2.13"),
        "{text}"
    );
    assert!(first[1].contains("peer=2 from=192.0.2.14"), "{text}");
    assert!(first[2].contains("op=0734009900000000 peer=1"), "{text}");
    assert!(lines(&text, MINUTE).is_empty(), "{text}");
    let text = capture_at(tracing::Level::INFO, || u.flush(t + WINDOW));
    let minute = lines(&text, MINUTE);
    assert_eq!(minute.len(), 1, "{text}");
    assert!(minute[0].contains("requests=7"), "{text}");
    assert!(
        minute[0].contains(
            "peer 1 0x100a unanswered 5, peer 1 0x0099 unanswered 1, peer 2 0x100a unanswered 1"
        ),
        "{text}"
    );
    // The next minute: the same pairs are only counted, never told again;
    // a request after the window logs the window before it.
    let later = t + WINDOW + WINDOW;
    let text = capture_at(tracing::Level::INFO, || {
        for _ in 0..3 {
            u.record(later, A, &op(0x100a));
        }
        u.record(later + WINDOW, A, &op(0x100a));
    });
    assert!(lines(&text, FIRST).is_empty(), "{text}");
    let minute = lines(&text, MINUTE);
    assert_eq!(minute.len(), 1, "{text}");
    assert!(minute[0].contains("requests=3"), "{text}");
}

#[test]
fn at_most_max_told_pairs_get_a_line_of_their_own() {
    let t = Instant::now();
    let mut u = Unanswered::default();
    let extra = 3;
    let text = capture_at(tracing::Level::INFO, || {
        for n in 0..MAX_TOLD + extra {
            u.record(t, A, &op(u16::try_from(n).unwrap()));
        }
    });
    assert_eq!(lines(&text, FIRST).len(), MAX_TOLD);
    // Every one is still counted.
    let text = capture_at(tracing::Level::INFO, || u.flush(t + WINDOW));
    let want = format!("requests={}", MAX_TOLD + extra);
    assert!(lines(&text, MINUTE)[0].contains(&want), "{text}");
}

#[test]
fn only_an_unknown_opcode_is_never_answered() {
    use crate::info::never_answered;
    assert!(never_answered(&op(0x100a)));
    // Board info and clock stats (answered, or left for the clock).
    assert!(!never_answered(&request([
        0x07, 0x2a, 0x00, 0x61, 0, 0, 0, 0
    ])));
    assert!(!never_answered(&request([
        0x07, 0x2a, 0x00, 0x21, 0, 0, 0, 0
    ])));
    // Shorter than the header.
    assert!(!never_answered(&op(0x100a)[..31]));
}
