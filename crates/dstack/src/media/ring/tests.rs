use super::*;

/// Ten years of a grandmaster's uptime at 96 kHz, as a position.
const T: u64 = 10 * 365 * 86_400 * 96_000;
const N: usize = 32;

/// A packet's worth of samples, distinct and non-zero.
fn packet(start: u64) -> Vec<i32> {
    (0..N as u64)
        .map(|i| ((start + i) as i32 | 1) << 8)
        .collect()
}

fn read(ring: &Ring, pos: u64, n: usize) -> (Vec<i32>, Read) {
    let mut out = vec![0x5555; n];
    let r = ring.read_at(pos, &mut out);
    (out, r)
}

#[test]
fn the_loudest_sample_stored_is_kept_until_taken() {
    let ring = Ring::new();
    assert_eq!(ring.take_peak(), 0);
    ring.write_at(T, &[100, -7_000, 300]);
    ring.write_at(T + 3, &[6_999]);
    assert_eq!(ring.take_peak(), 7_000);
    assert_eq!(ring.take_peak(), 0, "taken");
    // The most negative sample's magnitude is 2^31.
    ring.write_at(T + 4, &[i32::MIN, i32::MAX]);
    assert_eq!(ring.take_peak(), 1 << 31);
    // Only what is stored counts: a late write is not.
    let mut out = [0; 8];
    ring.read_at(T + 8, &mut out);
    assert_eq!(ring.write_at(T, &[-9_000]).late, 1);
    assert_eq!(ring.take_peak(), 0);
    // Nor an early one.
    assert_eq!(ring.write_at(T + 16 + SLOTS as u64, &[9_000]).early, 1);
    assert_eq!(ring.take_peak(), 0);
}

/// Review Focus 3, the pure part: a packet stamped T carries the source's
/// samples T…T+fpp−1; RX places it by its stamp across the 32-bit seconds
/// wrap and a grandmaster up for decades; the echo sends RX T − F at T.
#[test]
fn packets_across_the_seconds_wrap_land_at_their_stamps_and_echo_exactly() {
    use crate::clock::{from_stamp, stamp};
    use crate::media::packet;
    use crate::media::source::Source;

    const RATE: u32 = 96_000;
    const FPP: usize = 16;
    const F: u32 = 229;
    let wrap = (1u64 << 32) * u64::from(RATE);
    let rings = [Ring::new()];
    let mut counter = Source::Counter;
    let mut echo = Source::Echo { rx: 0, offset: F };
    let mut buf = [0u8; 256];
    let mut samples = [0i32; FPP];
    // 64 packets from 32 before the wrap; the clock 1 ms ahead of them.
    let first = wrap - 32 * FPP as u64;
    for p in 0..64u64 {
        let t = first + p * FPP as u64;
        counter.fill(t, RATE, &mut samples, &rings, None);
        let (secs, sub) = stamp(t, RATE);
        let n = packet::write(&mut buf, secs, sub, 1, 3, FPP, |f, _| samples[f]);
        // The receiver: header, the full position near its clock, the ring.
        let (secs, sub) = packet::read_header(&buf[..n]).unwrap();
        let pos = from_stamp(secs, sub, RATE, t + 96);
        assert_eq!(pos, t, "packet {p}");
        let mut got = [0i32; FPP];
        packet::read_frames(&buf[..n], 1, 3, |f, _, v| got[f] = v).unwrap();
        assert_eq!(rings[0].write_at(pos, &got), written(FPP), "packet {p}");
    }
    // The echo at T is the RX sample at T - F, sample for sample, across
    // the wrap.
    let mut out = [0i32; FPP];
    for t in (first + u64::from(F)..first + 64 * FPP as u64).step_by(FPP) {
        let f = echo.fill(t, RATE, &mut out, &rings, None);
        assert_eq!(f.holes, 0, "{t}");
        for (i, v) in out.iter().enumerate() {
            let rx = t + i as u64 - u64::from(F);
            assert_eq!((v >> 8) as u64, rx % 65_536, "{t} {i}");
        }
    }
    // The echo's own packets carry those samples under their own stamps.
    let t = wrap + 7;
    echo.fill(t, RATE, &mut out, &rings, None);
    let (secs, sub) = stamp(t, RATE);
    assert_eq!((secs, sub), (0, 7));
    let n = packet::write(&mut buf, secs, sub, 1, 3, FPP, |f, _| out[f]);
    let mut sent = [0i32; FPP];
    packet::read_frames(&buf[..n], 1, 3, |f, _, v| sent[f] = v).unwrap();
    assert_eq!(sent, out);
    assert_eq!((sent[0] >> 8) as u64, (wrap + 7 - u64::from(F)) % 65_536);
}

fn written(n: usize) -> Write {
    Write {
        written: n,
        ..Write::default()
    }
}

#[test]
fn samples_land_at_their_stamps() {
    let ring = Ring::new();
    // Out of order: the third packet first.
    for start in [T + 64, T, T + 32] {
        assert_eq!(ring.write_at(start, &packet(start)), written(N), "{start}");
    }
    let (got, r) = read(&ring, T, 3 * N);
    let want: Vec<i32> = [T, T + 32, T + 64]
        .iter()
        .flat_map(|&s| packet(s))
        .collect();
    assert_eq!(got, want);
    assert_eq!(r, Read { holes: 0 });
    // A read may start anywhere.
    let (got, _) = read(&ring, T + 30, 4);
    assert_eq!(got, want[30..34]);
    assert_eq!(
        ring.counters(),
        Counters {
            written: 3 * N as u64,
            ..Counters::default()
        }
    );
}

#[test]
fn a_missing_packet_reads_as_a_hole() {
    let ring = Ring::new();
    ring.write_at(T, &packet(T));
    ring.write_at(T + 64, &packet(T + 64));
    let (got, r) = read(&ring, T, 3 * N);
    assert_eq!(&got[..N], &packet(T)[..]);
    assert!(got[N..2 * N].iter().all(|&v| v == 0), "the hole reads 0");
    assert_eq!(&got[2 * N..], &packet(T + 64)[..]);
    assert_eq!(r, Read { holes: N });
    // A ring nobody wrote: all holes, wherever it is read.
    let empty = Ring::new();
    for pos in [0, 1 << 47, T, (1 << 48) + 5] {
        assert_eq!(
            read(&empty, pos, 8),
            (vec![0; 8], Read { holes: 8 }),
            "{pos}"
        );
    }
    assert_eq!(ring.counters().holes, N as u64);
    assert_eq!(empty.counters().holes, 32);
}

#[test]
fn a_packet_behind_the_reader_is_late_not_written() {
    let ring = Ring::new();
    assert_eq!(read(&ring, T, N).1, Read { holes: N });
    // The reader has passed T..T+32: those samples are late.
    assert_eq!(
        ring.write_at(T, &packet(T)),
        Write {
            late: N,
            ..Write::default()
        }
    );
    assert_eq!(read(&ring, T, N).1, Read { holes: N }, "never written");
    // A packet across the reader: its first half late, the rest written.
    let w = ring.write_at(T + 16, &packet(T + 16));
    assert_eq!(
        w,
        Write {
            written: 16,
            late: 16,
            ..Write::default()
        }
    );
    let (got, r) = read(&ring, T + 32, 16);
    assert_eq!(got, packet(T + 16)[16..]);
    assert_eq!(r.holes, 0);
    // The furthest read counts, not the last one.
    read(&ring, T + 100, 32);
    read(&ring, T, 1);
    assert_eq!(ring.write_at(T + 131, &[7 << 8]).late, 1);
    assert_eq!(ring.write_at(T + 132, &[7 << 8]).written, 1);
    let c = ring.counters();
    assert_eq!((c.late, c.written), (N as u64 + 16 + 1, 16 + 1));
}

#[test]
fn nothing_is_late_or_early_before_the_first_read() {
    let ring = Ring::new();
    assert_eq!(ring.write_at(T, &packet(T)), written(N));
    assert_eq!(ring.write_at(T - 1_000, &packet(T - 1_000)), written(N));
    let far = T + 10 * SLOTS as u64;
    assert_eq!(ring.write_at(far, &packet(far)), written(N));
}

#[test]
fn a_write_a_whole_ring_ahead_of_the_reader_is_early_and_keeps_what_is_unread() {
    let ring = Ring::new();
    ring.write_at(T, &packet(T));
    ring.write_at(T + 32, &packet(T + 32));
    // The reader is at T + 32: T + 32..T + 64 is not read yet.
    read(&ring, T, N);
    let ahead = T + 32 + SLOTS as u64;
    // Up to the slot before the reader's: written (those slots were read).
    assert_eq!(ring.write_at(ahead - 32, &packet(ahead - 32)), written(N));
    // From the reader's own slot on: early, the unread samples kept.
    assert_eq!(
        ring.write_at(ahead, &packet(ahead)),
        Write {
            early: N,
            ..Write::default()
        }
    );
    // Its first sample was written just above: a duplicate.
    let w = ring.write_at(ahead - 1, &packet(ahead - 1));
    assert_eq!((w.duplicate, w.early), (1, N - 1));
    let (got, r) = read(&ring, T + 32, N);
    assert_eq!((got, r.holes), (packet(T + 32), 0));
    assert_eq!(ring.counters().early, 2 * N as u64 - 1);
}

#[test]
fn a_duplicate_is_counted_and_harmless() {
    let ring = Ring::new();
    assert_eq!(ring.write_at(T, &packet(T)), written(N));
    assert_eq!(
        ring.write_at(T, &packet(T)),
        Write {
            duplicate: N,
            ..Write::default()
        }
    );
    // Half again, half new.
    let w = ring.write_at(T + 16, &packet(T + 16));
    assert_eq!((w.written, w.duplicate), (16, 16));
    let (got, r) = read(&ring, T, N + 16);
    assert_eq!(&got[..N], &packet(T)[..]);
    assert_eq!(&got[N..], &packet(T + 16)[16..]);
    assert_eq!(r.holes, 0);
    let c = ring.counters();
    assert_eq!(
        (c.written, c.duplicate, c.holes),
        (N as u64 + 16, N as u64 + 16, 0)
    );
    // The last copy wins: a stamp written ahead by a stray packet is
    // replaced by the flow's own.
    ring.write_at(T + 200, &[9 << 8]);
    ring.write_at(T + 200, &[3 << 8]);
    assert_eq!(read(&ring, T + 200, 1).0, [3 << 8]);
}

#[test]
fn a_wrapped_slot_never_reads_stale() {
    let ring = Ring::new();
    ring.write_at(T, &packet(T));
    // The same slots one lap on: never written there, so holes.
    let lap = SLOTS as u64;
    let (got, r) = read(&ring, T + lap, N);
    assert_eq!((got, r.holes), (vec![0; N], N));
    // A fresh ring: the next lap's write replaces the slot, and the earlier
    // position no longer reads its sample.
    let ring = Ring::new();
    ring.write_at(T, &packet(T));
    ring.write_at(T + lap, &packet(T + lap));
    assert_eq!(read(&ring, T, N), (vec![0; N], Read { holes: N }));
    assert_eq!(read(&ring, T + lap, N).0, packet(T + lap));
    // Laps far apart too (the lap is the position's bits 16..47).
    let ring = Ring::new();
    ring.write_at(T, &packet(T));
    for laps in [2, 255, 256, 65_536, 1 << 30] {
        let (got, r) = read(&ring, T + laps * lap, N);
        assert_eq!((got, r.holes), (vec![0; N], N), "{laps}");
    }
    // Negative samples keep their sign, zero reads as a real zero.
    let ring = Ring::new();
    ring.write_at(T, &[-256, i32::MIN, 0, i32::MAX]);
    assert_eq!(
        read(&ring, T, 4),
        (vec![-256, i32::MIN, 0, i32::MAX], Read { holes: 0 })
    );
}

/// Phase C: the engine's output rings. `put_at` stores behind the reader
/// and a whole ring ahead of it alike, never moves the reader's end, and
/// counts only `written` (a late block shows as a TX hole, not here).
#[test]
fn put_at_stores_behind_and_far_ahead_of_the_reader() {
    let ring = Ring::new();
    assert_eq!(read(&ring, T, N).1, Read { holes: N });
    // Behind the reader (write_at would call it late) and over the slots
    // the reader is at: stored.
    ring.put_at(T - 100, &packet(T - 100));
    ring.put_at(T, &packet(T));
    // A whole ring and more ahead (write_at would call it early): stored.
    let far = T + SLOTS as u64 + 1_000;
    ring.put_at(far, &packet(far));
    // The same positions again: stored again, the last copy wins.
    ring.put_at(T + 4, &[-(9 << 8); 2]);
    // The reader's end did not move: T + 32 is neither late nor early.
    assert_eq!(ring.write_at(T + N as u64, &packet(T + 32)), written(N));
    let c = ring.counters();
    assert_eq!(
        (c.written, c.late, c.early, c.duplicate, c.holes),
        (4 * N as u64 + 2, 0, 0, 0, N as u64),
        "{c:?}"
    );
    assert_eq!(read(&ring, T - 100, N), (packet(T - 100), Read::default()));
    let mut want = packet(T);
    want[4..6].copy_from_slice(&[-(9 << 8); 2]);
    assert_eq!(read(&ring, T, N), (want, Read::default()));
    assert_eq!(read(&ring, far, N), (packet(far), Read::default()));
    // An empty put stores and counts nothing.
    ring.put_at(T + 999, &[]);
    assert_eq!(ring.counters().written, 4 * N as u64 + 2);
}

/// obs (camera-box#1485): the RX thread and the reader on one CPU. A
/// write whose positions the reader passes while the write runs must
/// count them late, not written: each position the writer gives is
/// either read as its sample or counted late, and every hole the reader
/// saw at a written position is such a late sample.
#[test]
fn a_write_the_reader_overtakes_counts_the_overtaken_samples_late() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64};
    const BLOCK: usize = 4096;
    const READ: usize = 16;
    const BLOCKS: u64 = 256;
    let end = T + BLOCKS * BLOCK as u64;
    for _ in 0..8 {
        let ring = Arc::new(Ring::new());
        // The reader's end at T before the writer starts: with no read yet
        // a write has no early check, and a reader thread a runner starts
        // late would find samples a later lap overwrote (holes, neither
        // late nor early). Those READ positions count as holes.
        let primed = ring.read_at(T - READ as u64, &mut [0i32; READ]).holes as u64;
        let started = Arc::new(AtomicU64::new(T));
        let done = Arc::new(AtomicBool::new(false));
        let reader = {
            let (ring, started, done) = (ring.clone(), started.clone(), done.clone());
            std::thread::spawn(move || {
                let mut delivered = 0u64;
                let mut pos = T;
                let mut out = [0i32; READ];
                while pos < end {
                    // Read up to the end of the block being written, at
                    // once: the writer is still storing it.
                    if pos + READ as u64 <= started.load(Ordering::Acquire)
                        || done.load(Ordering::Acquire)
                    {
                        let r = ring.read_at(pos, &mut out);
                        delivered += (READ - r.holes) as u64;
                        pos += READ as u64;
                    } else {
                        std::hint::spin_loop();
                    }
                }
                delivered
            })
        };
        let block: Vec<i32> = (0..BLOCK as i32).map(|i| (i | 1) << 8).collect();
        for b in 0..BLOCKS {
            let pos = T + b * BLOCK as u64;
            started.store(pos + BLOCK as u64, Ordering::Release);
            ring.write_at(pos, &block);
        }
        done.store(true, Ordering::Release);
        let delivered = reader.join().unwrap();
        let c = ring.counters();
        let total = BLOCKS * BLOCK as u64;
        // A writer a whole ring ahead of a slow reader (a runner's thread
        // start) stores nothing: early, a hole too.
        assert_eq!(c.written + c.late + c.early, total, "{c:?}");
        assert_eq!(
            c.holes - primed,
            c.late + c.early,
            "every hole is a late or early sample: {c:?}"
        );
        assert_eq!(delivered, c.written, "{c:?}");
    }
}

/// A read leaves a later lap's sample alone and marks an earlier lap's,
/// the empty slot or its own position's mark; the comparison is mod 2³¹.
#[test]
fn a_read_marks_only_a_slot_of_no_later_lap() {
    let one = SLOTS as u64;
    let p = T;
    assert!(newer(word(p + one, 5), p));
    assert!(
        newer(word(p + (1 << 30) * one - one, 5), p),
        "the last later lap"
    );
    assert!(
        !newer(word(p + (1 << 30) * one, 5), p),
        "2^30 laps on is earlier"
    );
    assert!(!newer(word(p, 5), p), "its own lap");
    assert!(!newer(word(p - one, 5), p));
    assert!(!newer(EMPTY, p));
    assert!(!newer(read_empty(p), p));
    assert!(newer(read_empty(p + one), p), "a later lap's mark");
    // Across the lap's wrap: lap 2^31 - 1 is just before lap 0.
    let last = ((1u64 << 31) - 1) * one;
    assert!(newer(word(last + one, 5), last));
    assert!(!newer(word(last, 5), last + one));
    assert_eq!(lap(last + one), 0);
    assert_eq!(read_empty(p) >> 32, u64::from(lap(p) + READ_EMPTY));
}

/// A slot no write has stored is never a later lap: a read in any lap marks
/// it (laps 2^30 to 2^31 - 2 found it "newer" and left it unmarked, so the
/// overtaken-write race came back for half of every 46-year cycle).
#[test]
fn an_unwritten_slot_is_never_a_later_lap() {
    let one = SLOTS as u64;
    for lap in [
        0,
        1,
        (1u64 << 30) - 1,
        1 << 30,
        (1 << 30) + 5,
        (1 << 31) - 2,
        (1 << 31) - 1,
    ] {
        assert!(!newer(EMPTY, lap * one), "lap {lap}");
    }
}

/// A read of a slot no write has stored is a hole in every lap, the last
/// one (2^31 - 1) too: it read as a sample 0.
#[test]
fn an_unwritten_slot_reads_as_a_hole_in_the_last_lap() {
    let ring = Ring::new();
    let last = ((1u64 << 31) - 1) * SLOTS as u64;
    let mut out = [7i32; 2];
    assert_eq!(ring.read_at(last, &mut out).holes, 2);
    assert_eq!(out, [0, 0]);
}

/// TX's look before it sends ahead (dstack #1): every position there,
/// nothing marked, the read end left where it was.
#[test]
fn holds_says_whether_every_position_is_there_and_marks_nothing() {
    let ring = Ring::new();
    let base = 7 * SLOTS as u64 + 100;
    assert!(!ring.holds(base, 1));
    assert!(ring.holds(base, 0), "nothing asked is held");
    ring.put_at(base, &[1, 2, 3, 4]);
    assert!(ring.holds(base, 4));
    assert!(ring.holds(base + 1, 3));
    assert!(!ring.holds(base, 5), "one past the written end");
    assert!(!ring.holds(base - 1, 2), "one before it");
    // The same slots one lap earlier are not held.
    assert!(!ring.holds(base - SLOTS as u64, 4));
    // A look that found holes marked none: a late write still lands and
    // a read finds it whole.
    ring.put_at(base + 4, &[5]);
    assert!(ring.holds(base, 5));
    let (out, r) = read(&ring, base, 5);
    assert_eq!(out, [1, 2, 3, 4, 5]);
    assert_eq!(r.holes, 0);
    // An RX write after a look at a missing position is not late: the
    // look moved no read end.
    let rx = Ring::new();
    // A read up to `base`, so a write before the read end would be late.
    let _ = read(&rx, base - 10, 10);
    assert!(!rx.holds(base, 4));
    assert_eq!(rx.write_at(base, &[9, 9, 9, 9]).late, 0);
    assert!(rx.holds(base, 4));
}
