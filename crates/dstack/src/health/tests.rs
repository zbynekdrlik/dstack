use super::*;

use std::sync::Arc;

/// A ready clock, out of holdover, with past holdovers and a re-anchor
/// (equal in both reads: nothing new).
fn clock() -> Clock {
    Clock {
        ready: 1,
        holdover: 0,
        holdovers: 2,
        re_anchors: 1,
        freq_ppb: -750,
    }
}

/// A pump on the media clock with a past re-bootstrap.
fn pump(blocks: u64) -> Pump {
    Pump {
        media: 1,
        blocks,
        late: 3,
        missed: 4,
        rebootstraps: 1,
        max_process_us: 217,
    }
}

/// Two reads of an engine-hosted device a second apart: 3 000 blocks.
fn engine_reads() -> (Status, Status) {
    (
        Status {
            clock: clock(),
            engine: Some(pump(30_000)),
        },
        Status {
            clock: clock(),
            engine: Some(pump(33_000)),
        },
    )
}

/// Two reads of a device with no engine.
fn device_reads() -> (Status, Status) {
    let s = Status {
        clock: clock(),
        engine: None,
    };
    (s, s)
}

/// [`device_reasons`] of both reads.
fn of(reads: &(Status, Status), engine: bool) -> Vec<DeviceReason> {
    device_reasons(Some(&reads.0), Some(&reads.1), engine)
}

#[test]
fn a_running_device_has_no_reason() {
    assert_eq!(of(&engine_reads(), true), vec![]);
    assert_eq!(of(&device_reads(), false), vec![]);
}

#[test]
fn a_missing_read_is_no_status_and_nothing_else() {
    let (a, b) = engine_reads();
    for (first, second) in [(None, Some(&b)), (Some(&a), None), (None, None)] {
        for engine in [false, true] {
            assert_eq!(
                device_reasons(first, second, engine),
                vec![DeviceReason::NoStatus]
            );
        }
    }
}

#[test]
fn a_clock_not_ready_at_the_second_read_is_clock_not_ready() {
    let mut reads = device_reads();
    reads.1.clock.ready = 0;
    assert_eq!(of(&reads, false), vec![DeviceReason::ClockNotReady]);
    // An engine pacing itself while the clock is not ready is not free.
    let mut reads = engine_reads();
    reads.1.clock.ready = 0;
    reads.1.engine = Some(Pump {
        media: 0,
        ..pump(33_000)
    });
    assert_eq!(of(&reads, true), vec![DeviceReason::ClockNotReady]);
    // Judged on the second read: a clock that just became ready is ready.
    let mut reads = engine_reads();
    reads.0.clock.ready = 0;
    assert_eq!(of(&reads, true), vec![]);
}

#[test]
fn a_holdover_at_either_read_or_between_them_is_holdover() {
    let holdover = vec![DeviceReason::Holdover];
    let mut reads = engine_reads();
    reads.0.clock.holdover = 1;
    assert_eq!(of(&reads, true), holdover);
    let mut reads = engine_reads();
    reads.1.clock.holdover = 1;
    reads.1.clock.holdovers = 3;
    assert_eq!(of(&reads, true), holdover);
    // In and out again between the reads.
    let mut reads = engine_reads();
    reads.1.clock.holdovers = 3;
    assert_eq!(of(&reads, true), holdover);
    let mut reads = device_reads();
    reads.1.clock.holdover = 1;
    assert_eq!(of(&reads, false), holdover);
}

#[test]
fn a_ready_clock_with_a_pump_pacing_itself_is_pump_free() {
    let mut reads = engine_reads();
    reads.1.engine = Some(Pump {
        media: 0,
        ..pump(33_000)
    });
    assert_eq!(of(&reads, true), vec![DeviceReason::PumpFree]);
    // Judged on the second read: a pump that just went onto the clock.
    let mut reads = engine_reads();
    reads.0.engine = Some(Pump {
        media: 0,
        ..pump(30_000)
    });
    assert_eq!(of(&reads, true), vec![]);
}

#[test]
fn blocks_that_did_not_rise_are_pump_stalled() {
    let stalled = vec![DeviceReason::PumpStalled];
    let mut reads = engine_reads();
    reads.1.engine = Some(pump(30_000));
    assert_eq!(of(&reads, true), stalled);
    // Fewer (the engine started again between the reads).
    let mut reads = engine_reads();
    reads.1.engine = Some(pump(12));
    assert_eq!(of(&reads, true), stalled);
    // A read without the pump while an engine should host the device.
    for first in [false, true] {
        let mut reads = engine_reads();
        if first {
            reads.0.engine = None;
        } else {
            reads.1.engine = None;
        }
        assert_eq!(of(&reads, true), stalled);
    }
}

#[test]
fn a_rebootstrap_between_the_reads_is_pump_rebootstrap() {
    let mut reads = engine_reads();
    reads.1.engine = Some(Pump {
        rebootstraps: 2,
        ..pump(33_000)
    });
    assert_eq!(of(&reads, true), vec![DeviceReason::PumpRebootstrap]);
}

#[test]
fn without_an_engine_the_pump_is_not_judged() {
    // A pump in the status of a device that should host none: ignored.
    let (a, b) = engine_reads();
    let reads = (
        a,
        Status {
            engine: Some(pump(30_000)),
            ..b
        },
    );
    assert_eq!(of(&reads, false), vec![]);
}

#[test]
fn reasons_come_once_each_in_order() {
    let mut reads = engine_reads();
    reads.0.clock.holdover = 1;
    reads.1.clock.holdover = 1;
    reads.1.engine = Some(Pump {
        media: 0,
        rebootstraps: 2,
        ..pump(30_000)
    });
    assert_eq!(
        of(&reads, true),
        vec![
            DeviceReason::Holdover,
            DeviceReason::PumpFree,
            DeviceReason::PumpStalled,
            DeviceReason::PumpRebootstrap,
        ]
    );
}

#[test]
fn a_clock_not_ready_comes_first_and_hides_pump_free() {
    let mut reads = engine_reads();
    reads.1.clock.ready = 0;
    reads.1.clock.holdover = 1;
    reads.1.engine = Some(Pump {
        media: 0,
        rebootstraps: 2,
        ..pump(30_000)
    });
    assert_eq!(
        of(&reads, true),
        vec![
            DeviceReason::ClockNotReady,
            DeviceReason::Holdover,
            DeviceReason::PumpStalled,
            DeviceReason::PumpRebootstrap,
        ]
    );
}

#[test]
fn the_device_s_own_status_line_reads() {
    use crate::clock::{Event, ManualClock};
    use crate::daemon::media::tests::{T, Unready, device_at, media_on, media_with_engine};
    use crate::stream::{PumpStats, Timing};
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    // A fresh device: no clock, no engine.
    let media = media_on(&device, Arc::new(Unready));
    assert_eq!(
        parse_status(&crate::ctl::status(&media)),
        Some(Status {
            clock: Clock {
                ready: 0,
                holdover: 0,
                holdovers: 0,
                re_anchors: 0,
                freq_ppb: -750,
            },
            engine: None,
        })
    );
    // Ready, in holdover after one re-anchor, an engine attached.
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_with_engine(&device, Arc::new(ManualClock::new(T)), &[0]);
    media.clock_stats.record(Event::Ready { k_ns: 1 });
    media
        .clock_stats
        .record(Event::ReAnchored { delta_ns: 30_000 });
    media.clock_stats.record(Event::Holdover);
    media
        .freq
        .store(1_250, std::sync::atomic::Ordering::Relaxed);
    let timing = Timing {
        block: 32,
        playout: 96,
        margin: 96,
    };
    media.attach_engine(Arc::new(PumpStats::new(timing, 96_000)));
    let status = parse_status(&crate::ctl::status(&media)).unwrap();
    assert_eq!(
        status,
        Status {
            clock: Clock {
                ready: 1,
                holdover: 1,
                holdovers: 1,
                re_anchors: 1,
                freq_ppb: 1_250,
            },
            engine: Some(Pump {
                media: 0,
                blocks: 0,
                late: 0,
                missed: 0,
                rebootstraps: 0,
                max_process_us: 0,
            }),
        }
    );
    // Anything else is no status.
    for line in [
        "error: unknown request: status, source or check",
        "",
        "{}",
        "{\"clock\":{\"ready\":1}}",
        "ok",
    ] {
        assert_eq!(parse_status(line), None, "{line:?}");
    }
}

#[cfg(target_os = "linux")]
mod glue {
    use super::*;

    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::net::UnixListener;
    use std::time::{Duration, Instant};

    use crate::daemon::media::tests::BOUND;

    const STATUS_LINE: &str = "{\"clock\":{\"ready\":1,\"epoch\":0,\"polls\":9,\"re_anchors\":1,\
        \"holdovers\":2,\"holdover\":0,\"freq_ppb\":-750},\"requests\":[],\"sent\":[],\
        \"uncounted\":0,\"tx\":[],\"rx\":[],\"rx_poll_errors\":0,\"channels\":[],\
        \"sources\":[],\"checks\":[]}";

    /// A control socket in `dir` that answers one connection: it reads the
    /// request line and sends `answer`; returns the request it read.
    fn one_answer(
        dir: &std::path::Path,
        answer: &'static str,
    ) -> (std::path::PathBuf, std::thread::JoinHandle<String>) {
        let path = dir.join("control.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let served = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(BOUND)).unwrap();
            let mut reader = BufReader::new(stream);
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            reader.get_mut().write_all(answer.as_bytes()).unwrap();
            request
        });
        (path, served)
    }

    #[test]
    fn a_status_request_reads_the_status_line() {
        let dir = tempfile::tempdir().unwrap();
        let answer: &'static str = Box::leak(format!("{STATUS_LINE}\n").into_boxed_str());
        let (path, served) = one_answer(dir.path(), answer);
        let got = read_status(&path, BOUND);
        assert!(got.is_some());
        assert_eq!(got, parse_status(STATUS_LINE));
        assert_eq!(served.join().unwrap(), "status\n");
    }

    #[test]
    fn no_socket_or_an_error_answer_is_no_status() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_status(&dir.path().join("none.sock"), BOUND), None);
        let (path, served) = one_answer(dir.path(), "error: the request is not UTF-8\n");
        assert_eq!(read_status(&path, BOUND), None);
        served.join().unwrap();
    }

    #[test]
    fn a_device_that_never_answers_is_no_status_within_the_wait() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("control.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let held = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            // Holds the connection, answers nothing, until the client went.
            let mut buf = [0u8; 64];
            stream.set_read_timeout(Some(BOUND)).unwrap();
            let mut stream = stream;
            while matches!(stream.read(&mut buf), Ok(n) if n > 0) {}
        });
        let start = Instant::now();
        assert_eq!(read_status(&path, Duration::from_millis(200)), None);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
        held.join().unwrap();
    }
}
