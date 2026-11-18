//! The fixed app block (dstack #330, `Rated::with_fixed_app_block`).

use super::*;

/// 44.1 kHz on 96 kHz with a 128-frame block: `A` = ceil(58.8) = 59.
const A: usize = 59;

/// The device's play time of a block that starts at device frame `at`
/// (a made-up P: 10 ms after the media time), ns.
fn play_of(at: u64) -> i64 {
    (at * 1_000_000_000 / 96_000) as i64 + 10_000_000
}

/// Runs `blocks` device blocks of 128 frames (input `f(i)`, `i` the device
/// sample, on every channel) from device frame 0, each with its play time;
/// returns the device output of channel 0 and, per device block, the app
/// calls the block made.
fn run_played(
    r: &mut Rated<Probe>,
    ch: (usize, usize),
    blocks: usize,
    f: impl Fn(u64) -> f64,
) -> (Vec<f64>, Vec<usize>) {
    let n = 128;
    let mut sent = Vec::new();
    let mut calls = Vec::new();
    for b in 0..blocks {
        let at = (b * n) as u64;
        let input: Vec<f64> = (0..ch.0)
            .flat_map(|_| (0..n).map(|i| f(at + i as u64)))
            .collect();
        let mut out = vec![0.0; ch.1 * n];
        let before = r.inner.frames.len();
        r.process(
            &mut Block::new(n, &input, &mut out)
                .at(at)
                .played_at(play_of(at)),
        );
        calls.push(r.inner.frames.len() - before);
        sent.extend_from_slice(&out[..n.min(out.len())]);
    }
    (sent, calls)
}

#[test]
fn the_fixed_block_is_the_device_block_at_the_app_s_rate_rounded_up() {
    for (dev, app, block, a) in [
        (96_000u32, 44_100u32, 128usize, A),
        (96_000, 44_100, 32, 15),
        (96_000, 48_000, 128, 64),
        (48_000, 96_000, 64, 128),
        (48_000, 44_100, 160, 147),
    ] {
        let r = Rated::with_fixed_app_block(Probe::new(0.0), dev, app, block, 1, 1).unwrap();
        assert_eq!(r.app_block(), Some(a), "{dev} {app} {block}");
    }
    let r = Rated::new(Probe::new(0.0), 96_000, 44_100, 128, 1, 1).unwrap();
    assert_eq!(r.app_block(), None);
    assert!(Rated::with_fixed_app_block(Probe::new(0.0), 96_000, 0, 128, 1, 1).is_err());
}

/// The app's calls at 44.1 kHz on 96 kHz, block 128, recorded without
/// keeping the audio.
#[derive(Default)]
struct Calls {
    /// Calls whose block was not `A` frames, or did not follow on.
    wrong: u64,
    /// Calls whose play time is not A / R after the last one's, within an
    /// app frame (the frames' phase in their device block) and the ns
    /// rounding of each.
    wrong_play: u64,
    first: Option<i64>,
    last: Option<(u64, i64)>,
    /// The device block each call came in (set by the test).
    block: u64,
    /// The calls' device blocks against their index, summed for the fit.
    calls: u64,
    sum_x: f64,
    sum_xx: f64,
    sum_xy: f64,
}

impl Process for Calls {
    fn process(&mut self, block: &mut Block<'_>) {
        let (start, play) = (block.start().unwrap(), block.play_at().unwrap());
        if block.frames() != A {
            self.wrong += 1;
        }
        if let Some((s, p)) = self.last {
            if start != s + A as u64 {
                self.wrong += 1;
            }
            let step = (play - p) as f64;
            if (step - A as f64 * 1e9 / 44_100.0).abs() > 1e9 / 44_100.0 + 2.0 {
                self.wrong_play += 1;
            }
        }
        self.first.get_or_insert(play);
        self.last = Some((start, play));
        block.output(0).fill(0.5);
        let (x, y) = (self.block as f64, self.calls as f64);
        self.sum_x += x;
        self.sum_xx += x * x;
        self.sum_xy += x * y;
        self.calls += 1;
    }
}

/// Runs `blocks` device blocks at 44.1 kHz on 96 kHz, block 128, with the
/// fixed app block, and checks every call and both FIFOs (dstack #330):
/// every call gets exactly `A` frames in follow-on time and play time, at
/// most one call comes in a device block, what waits for the app is always
/// less than a call (RX never dry), the app's 0.5 leaves the device whole
/// (TX never dry), and the calls come at R / A a second within 1 ppm (a
/// least-squares fit over every call, and the mean play-time step).
fn check_exact(blocks: u64) {
    let mut r = Rated::with_fixed_app_block(Calls::default(), 96_000, 44_100, 128, 1, 1).unwrap();
    let dry = r.dry();
    let room = |r: &Rated<Calls>| {
        let rx = r.reblock.as_ref().unwrap().fifo[0].capacity();
        (rx, r.fifo[0].capacity())
    };
    let rooms_before = room(&r);
    let input = [0.25f64; 128];
    let mut out = [0.0f64; 128];
    let mut twice = 0u64;
    for b in 0..blocks {
        r.inner.block = b;
        let before = r.inner.calls;
        r.process(
            &mut Block::new(128, &input, &mut out)
                .at(b * 128)
                .played_at(b as i64 * 4_000_000 / 3),
        );
        if r.inner.calls > before + 1 {
            twice += 1;
        }
        if b > 100 {
            assert!(out.iter().all(|v| (v - 0.5).abs() < 1e-6), "block {b}");
        }
        let held = r.reblock.as_ref().unwrap().held;
        assert!(held < A, "block {b}: {held}");
    }
    // Neither FIFO ever grew (no allocation in `process`).
    assert_eq!(room(&r), rooms_before);
    let c = &r.inner;
    assert_eq!((c.wrong, c.wrong_play, twice), (0, 0, 0));
    assert_eq!(dry.load(Ordering::Relaxed), 0);
    let want = 128.0 * 44_100.0 / (96_000.0 * A as f64);
    // Least squares over (device block, call index): calls per block.
    let n = c.calls as f64;
    let mean_y = (n - 1.0) / 2.0;
    let slope = (c.sum_xy - c.sum_x * mean_y) / (c.sum_xx - c.sum_x * c.sum_x / n);
    let ppm = (slope / want - 1.0) * 1e6;
    assert!(
        ppm.abs() < 1.0,
        "{slope} calls a block, {ppm} ppm off {want}"
    );
    // The play time's mean step is A / R.
    let span = (c.last.unwrap().1 - c.first.unwrap()) as f64 / (n - 1.0);
    let ppm = (span / (A as f64 * 1e9 / 44_100.0) - 1.0) * 1e6;
    assert!(ppm.abs() < 1.0, "a call every {span} ns, {ppm} ppm");
    // Every device block's frames reached the app but the last few.
    assert!((blocks as f64 * want - n).abs() < 2.0, "{n} calls");
}

/// dstack #330: twenty simulated seconds (68 skipped calls) in the unit
/// tests; `tests/rated_fixed.rs` runs ten minutes.
#[test]
fn a_fixed_block_at_44_1_khz_stays_exact() {
    check_exact(20 * 96_000 / 128);
}

/// dstack #330: an exact 2:1 or 1:2 with the fixed block is the plain path
/// sample for sample: the same frames, times and output, no delay added.
#[test]
fn a_fixed_block_at_an_exact_ratio_is_the_plain_path() {
    for (dev, app) in [(96_000u32, 48_000u32), (48_000, 96_000)] {
        let tone = |i: u64| (2.0 * PI * 1_000.0 * i as f64 / f64::from(dev)).sin() * 0.5;
        let mut plain = Rated::new(Probe::new(0.25), dev, app, 128, 2, 2).unwrap();
        let mut fixed = Rated::with_fixed_app_block(Probe::new(0.25), dev, app, 128, 2, 2).unwrap();
        assert!(fixed.reblock.is_none(), "{dev} -> {app}");
        assert_eq!(fixed.delay_ns(), plain.delay_ns(), "{dev} -> {app}");
        let (a, calls) = run_played(&mut fixed, (2, 2), 1_000, tone);
        let (b, _) = run_played(&mut plain, (2, 2), 1_000, tone);
        assert_eq!(a, b, "{dev} -> {app}");
        assert!(calls.iter().all(|&c| c == 1), "{dev} -> {app}");
        let want = fixed.app_block().unwrap();
        assert!(
            fixed.inner.frames.iter().all(|&k| k == want),
            "{dev} -> {app}"
        );
        assert_eq!(fixed.inner.frames, plain.inner.frames);
        assert_eq!(fixed.inner.starts, plain.inner.starts);
        assert_eq!(fixed.inner.play_ats, plain.inner.play_ats);
        assert_eq!(fixed.inner.heard, plain.inner.heard);
    }
}

/// dstack #330: the fixed block's delay at 44.1 kHz is the plain path's
/// with `A - 1` app frames more RX and `A` more TX, the TX FIFO primed
/// with `A + PRIME`.
#[test]
fn a_fixed_block_s_delay_adds_its_fifo_s_worst_case() {
    let plain = Rated::new(Probe::new(0.0), 96_000, 44_100, 128, 1, 1).unwrap();
    let fixed = Rated::with_fixed_app_block(Probe::new(0.0), 96_000, 44_100, 128, 1, 1).unwrap();
    let ((prx, ptx), (frx, ftx)) = (plain.delay_ns(), fixed.delay_ns());
    let ns = |frames: u64| frames * 1_000_000_000 / 44_100;
    assert!(
        (frx - prx).abs_diff(ns(A as u64 - 1)) <= 1,
        "rx {prx} -> {frx}"
    );
    assert!((ftx - ptx).abs_diff(ns(A as u64)) <= 1, "tx {ptx} -> {ftx}");
    // About +1.32 ms RX and +1.34 ms TX (the module's numbers).
    assert_eq!(((frx - prx) / 10_000, (ftx - ptx) / 10_000), (131, 133));
    assert_eq!(fixed.fifo[0].len(), A + PRIME);
    assert!(fixed.fifo[0].iter().all(|&v| v == 0.0));
}

/// dstack #330: impulses into the device reach the fixed-block app no
/// later than [`Rated::delay_ns`]'s RX says, and at some phase as late. The
/// wait is measured against a plain `Rated` fed the same: in the device
/// block of the app's call, the plain path hands the app frame `F`; the
/// fixed call starts at an older frame, and the difference is what the
/// impulse waited on top of the converter.
#[test]
fn impulses_take_at_most_the_fixed_block_s_rx_delay() {
    let mut fixed =
        Rated::with_fixed_app_block(Probe::new(0.0), 96_000, 44_100, 128, 1, 1).unwrap();
    let mut plain = Rated::new(Probe::new(0.0), 96_000, 44_100, 128, 1, 1).unwrap();
    let want = fixed.delay_ns().0 as f64 * 44_100.0 / 1e9;
    // 300 impulses 1283 device frames apart: every phase of the wait.
    let ats: Vec<u64> = (0..300).map(|k| 6_400 + k * 1_283).collect();
    let blocks = (ats[ats.len() - 1] / 128) as usize + 20;
    let impulse = |i: u64| f64::from(u8::from(ats.contains(&i)));
    let (_, calls) = run_played(&mut fixed, (1, 1), blocks, impulse);
    run_played(&mut plain, (1, 1), blocks, impulse);
    // The device block of each call.
    let call_block: Vec<usize> = calls
        .iter()
        .enumerate()
        .flat_map(|(b, &c)| std::iter::repeat_n(b, c))
        .collect();
    let heard = &fixed.inner.heard;
    let mut worst = f64::MIN;
    for &at in &ats {
        // The impulse's peak, near where it lands.
        let near = (at as f64 * 44_100.0 / 96_000.0 + want) as usize;
        let peak = (near - 100..near + 100)
            .max_by(|&a, &b| heard[a].abs().total_cmp(&heard[b].abs()))
            .unwrap();
        let call = peak / A;
        let b = call_block[call];
        let waited =
            plain.inner.starts[b].unwrap() as f64 - fixed.inner.starts[call].unwrap() as f64;
        let rx = peak as f64 + waited - at as f64 * 44_100.0 / 96_000.0;
        assert!(rx <= want + 1.0, "impulse at {at}: rx {rx} of {want}");
        worst = worst.max(rx);
    }
    assert!(worst >= want - 1.5, "worst rx {worst} of {want}");
}

/// Sends a square wave: 0.5 on app frames in odd runs of [`RUN`] (by the
/// block's media start), 0 in even ones; records its blocks' starts.
struct Square {
    starts: Vec<u64>,
}

/// The square wave's half period, app frames.
const RUN: u64 = 590;

impl Process for Square {
    fn process(&mut self, block: &mut Block<'_>) {
        let s = block.start().unwrap();
        self.starts.push(s);
        for (j, v) in block.output(0).iter_mut().enumerate() {
            *v = if (s + j as u64) / RUN % 2 == 1 {
                0.5
            } else {
                0.0
            };
        }
    }
}

/// Each rise of [`Square`] through a `Rated` at 44.1 kHz on 96 kHz, block
/// 128 (the fixed app block or not): the TX delay `Rated::delay_ns` states
/// and every rise's measured one, device frames, counted from the device
/// block of the call that handed it, at the rise's place in the call.
fn rises(fixed: bool) -> (f64, Vec<f64>) {
    let square = Square { starts: Vec::new() };
    let mut r = if fixed {
        Rated::with_fixed_app_block(square, 96_000, 44_100, 128, 0, 1)
    } else {
        Rated::new(square, 96_000, 44_100, 128, 0, 1)
    }
    .unwrap();
    let want = r.delay_ns().1 as f64 * 96_000.0 / 1e9;
    let mut sent = Vec::new();
    let mut call_block = Vec::new();
    for b in 0..6_500usize {
        let mut out = [0.0f64; 128];
        let before = r.inner.starts.len();
        r.process(&mut Block::new(128, &[], &mut out).at(b as u64 * 128));
        call_block.extend(std::iter::repeat_n(b, r.inner.starts.len() - before));
        sent.extend_from_slice(&out);
    }
    let starts = &r.inner.starts;
    let end = starts[starts.len() - 1];
    let measured = (1..)
        .step_by(2)
        .map(|m| m * RUN)
        .take_while(|&e| e + 2 * RUN < end)
        .map(|rise| {
            // The call that holds the rise: the last that starts at or before it.
            let call = starts.iter().rposition(|&s| s <= rise).unwrap();
            let handed = (call_block[call] * 128) as f64
                + (rise - starts[call]) as f64 * 96_000.0 / 44_100.0;
            let from = handed as usize;
            let half = from + sent[from..].iter().position(|v| *v >= 0.25).unwrap();
            half as f64 - handed
        })
        .collect();
    (want, measured)
}

/// dstack #330: each rise from the fixed-block app reaches the device no
/// later than [`Rated::delay_ns`]'s TX says, and at some phase `A` app
/// frames later than the plain path's latest (its prime's `A` on top of
/// the plain path's, which the same converters and prime set).
#[test]
fn rises_take_at_most_the_fixed_block_s_tx_delay() {
    let (want, fixed) = rises(true);
    let (plain_want, plain) = rises(false);
    assert!(fixed.len() > 250, "{}", fixed.len());
    for (k, tx) in fixed.iter().enumerate() {
        assert!(*tx <= want + 3.0, "rise {k}: tx {tx} of {want}");
    }
    let worst = |v: &[f64]| v.iter().copied().fold(f64::MIN, f64::max);
    let a = A as f64 * 96_000.0 / 44_100.0;
    assert!((want - plain_want - a).abs() < 0.1, "{want} {plain_want}");
    let more = worst(&fixed) - worst(&plain);
    assert!(
        (more - a).abs() <= 2.0,
        "worst {} vs plain {}",
        worst(&fixed),
        worst(&plain)
    );
}

/// dstack #330: each fixed call's media start and play time sit as many
/// frames before its device block's as were waiting; its play time adds
/// the fixed block's FIFO wait (A - 1 frames) and never the converter's
/// delay (dstack #410).
#[test]
fn a_fixed_call_s_start_and_play_time_follow_its_own_frames() {
    let mut r = Rated::with_fixed_app_block(Probe::new(0.0), 96_000, 44_100, 128, 1, 1).unwrap();
    let wait_ns = (A as i64 - 1) * 1_000_000_000 / 44_100;
    let mut plain = Rated::new(Probe::new(0.0), 96_000, 44_100, 128, 1, 1).unwrap();
    let (_, calls) = run_played(&mut r, (1, 1), 400, |_| 0.0);
    run_played(&mut plain, (1, 1), 400, |_| 0.0);
    let mut call = 0;
    for (b, &c) in calls.iter().enumerate() {
        for _ in 0..c {
            // The frames that waited: the plain path's start in this block
            // less the call's.
            let start = r.inner.starts[call].unwrap();
            let waited = plain.inner.starts[b].unwrap() as i64 - start as i64;
            assert!((0..A as i64).contains(&waited), "{b}: {waited}");
            let want = play_of(b as u64 * 128) + wait_ns - waited * 1_000_000_000 / 44_100;
            assert_eq!(r.inner.play_ats[call], Some(want), "block {b}");
            call += 1;
        }
    }
    assert!(call > 150);
    // A block with no times gives a call with none.
    let (i, mut o) = ([0.0; 128], [0.0; 128]);
    for _ in 0..3 {
        r.process(&mut Block::new(128, &i, &mut o));
    }
    assert_eq!(r.inner.starts.last(), Some(&None));
    assert_eq!(r.inner.play_ats.last(), Some(&None));
}

/// dstack #330: a device block that does not follow on restarts the app's
/// time at the device's start at the app's rate; the frames still waiting
/// keep their audio and sit that many frames before it, so the app's
/// frames follow on again from there.
#[test]
fn a_fixed_block_s_time_restarts_with_a_device_block_that_does_not_follow_on() {
    let mut r = Rated::with_fixed_app_block(Probe::new(0.0), 96_000, 44_100, 128, 1, 1).unwrap();
    let (i, mut o) = ([0.0; 128], [0.0; 128]);
    let mut b = 0u64;
    // Some frames waiting.
    while b < 10 || r.reblock.as_ref().unwrap().held == 0 {
        r.process(&mut Block::new(128, &i, &mut o).at(b * 128));
        b += 1;
    }
    let calls = r.inner.starts.len();
    let held = r.reblock.as_ref().unwrap().held as u64;
    // Ten seconds on: device frame 960 000 is app frame 441 000, and the
    // frames that waited come just before it, whichever block calls next.
    let jump = 96_000 * 10;
    for k in 0..10 {
        r.process(&mut Block::new(128, &i, &mut o).at(jump + k * 128));
    }
    let starts = &r.inner.starts[calls..];
    assert_eq!(starts[0], Some(441_000 - held));
    for w in starts.windows(2) {
        assert_eq!(w[1].unwrap(), w[0].unwrap() + A as u64);
    }
    assert!(starts.len() >= 9, "{}", starts.len());
}

#[test]
fn a_fixed_block_at_one_rate_is_the_device_s_block_with_no_fifo() {
    let fixed = Rated::with_fixed_app_block(Probe::new(0.0), 96_000, 96_000, 128, 1, 1).unwrap();
    let plain = Rated::new(Probe::new(0.0), 96_000, 96_000, 128, 1, 1).unwrap();
    assert!(fixed.reblock.is_none());
    assert_eq!(fixed.app_block(), Some(128));
    assert_eq!(fixed.delay_ns(), plain.delay_ns());
    assert_eq!(fixed.fifo[0].len(), plain.fifo[0].len());
}

/// dstack #330: the FIFOs' room, pinned (a smaller one would grow, which
/// allocates, inside `process`).
#[test]
fn the_fifos_room_covers_a_call_and_the_prime() {
    assert_eq!(rooms(59, 60, 140, 67), (238, 1100));
    assert_eq!(rooms(0, 60, 140, 8), (120, 864));
    assert_eq!(rooms(70, 60, 140, 8), (260, 904));
    assert_eq!(rx_wait(None), 0);
}

#[test]
fn a_fixed_block_with_no_rx_channel_still_comes_at_the_app_s_rate() {
    let mut r = Rated::with_fixed_app_block(Probe::new(0.25), 96_000, 44_100, 128, 0, 1).unwrap();
    let (sent, calls) = run_played(&mut r, (0, 1), 1_000, |_| 0.0);
    assert!(r.inner.frames.iter().all(|&k| k == A));
    assert!(calls.iter().all(|&c| c <= 1));
    let total: usize = calls.iter().sum();
    // 1 000 blocks of 58.8 frames, in calls of 59.
    assert!((995..=997).contains(&total), "{total}");
    assert!((sent[sent.len() - 1] - 0.25).abs() < 1e-6);
}

#[test]
fn a_discontinuity_empties_the_fixed_block_s_fifo_and_primes_tx_again() {
    let mut r = Rated::with_fixed_app_block(Probe::new(0.5), 96_000, 44_100, 128, 2, 2).unwrap();
    run_played(&mut r, (2, 2), 50, |_| 1.0);
    // Some frames waiting, whatever the phase; each channel holds them.
    while r.reblock.as_ref().unwrap().held == 0 {
        run_played(&mut r, (2, 2), 1, |_| 1.0);
    }
    let rb = r.reblock.as_ref().unwrap();
    assert!(rb.fifo.iter().all(|f| f.len() == rb.held));
    r.discontinuity();
    let rb = r.reblock.as_ref().unwrap();
    assert_eq!(rb.held, 0);
    assert!(rb.fifo.iter().all(VecDeque::is_empty));
    assert!(
        r.fifo
            .iter()
            .all(|f| f.len() == A + PRIME && f.iter().all(|&v| v == 0.0))
    );
    assert!(r.next.is_none());
    assert_eq!(r.inner.resets, 1);
    // After it, the app hears only the new silence.
    let before = r.inner.heard.len();
    run_played(&mut r, (2, 2), 5, |_| 0.0);
    assert!(r.inner.heard[before..].iter().all(|v| v.abs() < 1e-12));
}

#[test]
fn each_channel_keeps_its_own_level_through_the_fixed_block() {
    let probe = Levels {
        levels: vec![0.25, 0.75, -0.5],
        last_in: vec![0.0; 3],
    };
    let mut r = Rated::with_fixed_app_block(probe, 96_000, 44_100, 128, 3, 3).unwrap();
    let mut out = vec![0.0; 3 * 128];
    let input: Vec<f64> = (0..3)
        .flat_map(|c| std::iter::repeat_n(0.1 * (c as f64 + 1.0), 128))
        .collect();
    for b in 0..1_000u64 {
        r.process(&mut Block::new(128, &input, &mut out).at(b * 128));
    }
    for (c, want) in [0.25, 0.75, -0.5].into_iter().enumerate() {
        for v in &out[c * 128..(c + 1) * 128] {
            assert!((v - want).abs() < 1e-6, "out {c} = {v}");
        }
        let heard = r.inner.last_in[c];
        assert!(
            (heard - 0.1 * (c as f64 + 1.0)).abs() < 1e-6,
            "in {c} = {heard}"
        );
    }
}
