// SPDX-License-Identifier: MIT OR Apache-2.0

use std::io::ErrorKind;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::{MAX_ANSWER, ProbeError, REQUEST, line_of, status, status_at, time_left};

/// The size tests' deadline: a megabyte through a coverage-instrumented
/// build takes longer than the probe's own [`super::WAIT`]; they test the
/// size, the deadline has its own test (`a_slow_drip_ends_at_the_deadline`).
const SIZE_WAIT: Duration = Duration::from_secs(30);

#[test]
fn time_left_is_up_at_the_deadline() {
    let now = Instant::now();
    let d = Duration::from_millis(300);
    assert_eq!(time_left(now + d, now).expect("time left"), d);
    let at = time_left(now, now).expect_err("none at the deadline");
    assert_eq!(at.kind(), ErrorKind::TimedOut);
    let past = time_left(now, now + d).expect_err("none past it");
    assert_eq!(past.kind(), ErrorKind::TimedOut);
}

#[test]
fn a_slow_drip_ends_at_the_deadline() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    let addr = listener.local_addr().expect("addr");
    let server = thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("a connection");
        // One byte every 50 ms, never a newline, until the probe hangs up.
        while s.write_all(b"a").is_ok() {
            thread::sleep(Duration::from_millis(50));
        }
    });
    let start = Instant::now();
    let e = status_at(addr, Duration::from_millis(300)).expect_err("cut off");
    let took = start.elapsed();
    assert!(
        matches!(&e, ProbeError::Io(io) if io.kind() == ErrorKind::TimedOut || io.kind() == ErrorKind::WouldBlock),
        "{e:?}"
    );
    assert!(took < Duration::from_millis(1500), "{took:?}");
    server.join().expect("the server");
}

#[test]
fn a_newline_past_the_largest_size_is_too_long() {
    let mut fits = vec![b'a'; MAX_ANSWER];
    fits.push(b'\n');
    let (addr, server) = endpoint(vec![fits]);
    assert_eq!(
        status_at(addr, SIZE_WAIT).expect("a line").len(),
        MAX_ANSWER
    );
    server.join().expect("the server");
    // The newline arrives in the same read as the bytes past the size.
    let mut over = vec![b'a'; MAX_ANSWER + 1];
    over.push(b'\n');
    let (addr, server) = endpoint(vec![
        vec![b'a'; MAX_ANSWER - 10],
        over.split_off(MAX_ANSWER - 10),
    ]);
    assert!(matches!(
        status_at(addr, SIZE_WAIT),
        Err(ProbeError::TooLong)
    ));
    let _ = server.join();
}

/// A status line as `dstack::ctl::status` writes it (a device with two RX
/// channels and an engine), shortened to the parts a reader uses.
const STATUS: &str = concat!(
    r#"{"clock":{"ready":1,"epoch":1,"polls":120,"re_anchors":0,"holdovers":0,"holdover":0,"freq_ppb":-1520,"source":"dsync"},"#,
    r#""requests":[],"sent":[],"uncounted":0,"tx":[],"#,
    r#""rx":[{"flow":1,"granted":1,"fpp":16,"channels":2,"latency":480,"packets":9000,"late":0,"unplaced":0}],"rx_poll_errors":0,"#,
    r#""channels":[{"rx":1,"status":16842761,"written":1,"holes":0,"late":0,"duplicate":0,"early":0}],"#,
    r#""sources":[],"checks":[],"#,
    r#""engine":{"media":1,"offset":0,"playout":480,"blocks":100,"late":0,"missed":0,"overruns":0,"rebootstraps":0,"max_process_us":40}}"#
);

/// An endpoint on 127.0.0.1 (a port the OS picks) taking one connection:
/// it reads the request, then writes each part of `answer` with a pause
/// between, then closes. The thread returns the request it read.
fn endpoint(answer: Vec<Vec<u8>>) -> (SocketAddr, JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind 127.0.0.1");
    let addr = listener.local_addr().expect("its address");
    let handle = thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("a connection");
        let mut req = vec![0u8; REQUEST.len()];
        s.read_exact(&mut req).expect("the request");
        for (i, part) in answer.iter().enumerate() {
            if i > 0 {
                thread::sleep(Duration::from_millis(50));
            }
            s.write_all(part).expect("the answer");
            s.flush().expect("flush");
        }
        req
    });
    (addr, handle)
}

const WAIT: Duration = Duration::from_secs(2);

#[test]
fn reads_the_status_line_of_an_endpoint() {
    let mut line = STATUS.as_bytes().to_vec();
    line.push(b'\n');
    let (addr, server) = endpoint(vec![line]);
    let got = status(addr.port()).expect("a status line");
    assert_eq!(got, STATUS);
    assert_eq!(server.join().expect("the server"), b"status\n");
}

#[test]
fn reads_a_line_sent_in_parts_and_stops_at_its_end() {
    let (head, tail) = STATUS.split_at(100);
    let mut rest = tail.as_bytes().to_vec();
    rest.extend_from_slice(b"\nnot read\n");
    let (addr, server) = endpoint(vec![head.as_bytes().to_vec(), rest]);
    assert_eq!(status_at(addr, WAIT).expect("a status line"), STATUS);
    server.join().expect("the server");
}

#[test]
fn a_line_closed_without_newline_still_reads() {
    let (addr, server) = endpoint(vec![b"{\"clock\":{}}\r".to_vec()]);
    assert_eq!(status_at(addr, WAIT).expect("a line"), "{\"clock\":{}}");
    server.join().expect("the server");
}

#[test]
fn an_answer_of_the_largest_size_reads_and_one_byte_more_does_not() {
    let (addr, server) = endpoint(vec![vec![b'a'; MAX_ANSWER]]);
    assert_eq!(
        status_at(addr, SIZE_WAIT).expect("a line").len(),
        MAX_ANSWER
    );
    server.join().expect("the server");
    let (addr, server) = endpoint(vec![vec![b'a'; MAX_ANSWER + 1]]);
    assert!(matches!(
        status_at(addr, SIZE_WAIT),
        Err(ProbeError::TooLong)
    ));
    let _ = server.join();
}

#[test]
fn the_devices_refusal_and_an_empty_answer_are_errors() {
    let (addr, server) = endpoint(vec![b"error: this endpoint answers status only\n".to_vec()]);
    match status_at(addr, WAIT) {
        Err(ProbeError::Refused(r)) => assert_eq!(r, "this endpoint answers status only"),
        other => panic!("not a refusal: {other:?}"),
    }
    server.join().expect("the server");
    let (addr, server) = endpoint(vec![]);
    assert!(matches!(status_at(addr, WAIT), Err(ProbeError::Empty)));
    server.join().expect("the server");
}

#[test]
fn no_endpoint_is_an_io_error() {
    let port = {
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        l.local_addr().expect("addr").port()
    };
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let e = status_at(addr, Duration::from_millis(500)).expect_err("nothing listens");
    assert!(matches!(e, ProbeError::Io(_)), "{e:?}");
    assert!(e.to_string().starts_with("no answer: "), "{e}");
}

#[test]
fn an_endpoint_that_never_answers_times_out() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    let addr = listener.local_addr().expect("addr");
    let start = std::time::Instant::now();
    let e = status_at(addr, Duration::from_millis(200)).expect_err("no answer");
    assert!(matches!(e, ProbeError::Io(_)), "{e:?}");
    assert!(start.elapsed() < Duration::from_secs(5));
    drop(listener);
}

#[test]
fn line_of_reads_text_only() {
    assert!(matches!(
        line_of(vec![0xff, 0xfe]),
        Err(ProbeError::NotText)
    ));
    assert!(matches!(line_of(b"\r".to_vec()), Err(ProbeError::Empty)));
    assert_eq!(line_of(b"{}".to_vec()).expect("a line"), "{}");
    // Only the exact prefix is a refusal.
    assert_eq!(line_of(b"error:x".to_vec()).expect("a line"), "error:x");
    assert_eq!(
        ProbeError::Refused("x".into()).to_string(),
        "the device answered: x"
    );
}
