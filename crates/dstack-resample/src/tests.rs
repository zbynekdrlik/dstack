use super::*;
use std::f64::consts::PI;

/// −1 dBFS.
const MINUS_1_DBFS: f64 = 0.891_250_938_133_745_6;

/// `n` samples of a `hz` sine at `rate`, peak `amp`.
fn tone(hz: f64, rate: f64, n: usize, amp: f64) -> Vec<f64> {
    (0..n)
        .map(|i| amp * (2.0 * PI * hz * i as f64 / rate).sin())
        .collect()
}

/// Runs one channel of `input` through `c` (fixed input of `block`
/// frames); returns all the output.
fn run_fixed_input(c: &mut Converter, input: &[f64], block: usize) -> Vec<f64> {
    let room = c.output_frames_max();
    let mut out = vec![0.0; room];
    let mut all = Vec::new();
    for chunk in input.chunks_exact(block) {
        let (taken, made) = c.process(chunk, block, &mut out, room).unwrap();
        assert_eq!(taken, block);
        all.extend_from_slice(&out[..made]);
    }
    all
}

/// The residual of `x` after the least-squares fit of a sine at `hz`
/// (`rate`) with an offset: its THD+N in dB (signal over residual).
fn thd_n_db(x: &[f64], hz: f64, rate: f64) -> f64 {
    let w = 2.0 * PI * hz / rate;
    // Normal equations for a*sin + b*cos + d.
    let (mut ss, mut cc, mut sc, mut s1, mut c1, mut n) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    let (mut xs, mut xc, mut x1) = (0.0, 0.0, 0.0);
    for (i, &v) in x.iter().enumerate() {
        let (s, c) = (w * i as f64).sin_cos();
        ss += s * s;
        cc += c * c;
        sc += s * c;
        s1 += s;
        c1 += c;
        n += 1.0;
        xs += v * s;
        xc += v * c;
        x1 += v;
    }
    let m = [[ss, sc, s1], [sc, cc, c1], [s1, c1, n]];
    let r = [xs, xc, x1];
    let det = |m: [[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let d = det(m);
    let solve = |k: usize| {
        let mut mk = m;
        for (row, &rv) in mk.iter_mut().zip(&r) {
            row[k] = rv;
        }
        det(mk) / d
    };
    let (a, b, off) = (solve(0), solve(1), solve(2));
    let (mut sig, mut res) = (0.0, 0.0);
    for (i, &v) in x.iter().enumerate() {
        let (s, c) = (w * i as f64).sin_cos();
        let fit = a * s + b * c + off;
        sig += fit * fit;
        res += (v - fit) * (v - fit);
    }
    10.0 * (sig / res).log10()
}

/// The level of `hz` in `x` (`rate`), dBFS, by a Blackman-Harris windowed
/// correlation.
fn level_dbfs(x: &[f64], hz: f64, rate: f64) -> f64 {
    let n = x.len() as f64;
    let (mut re, mut im, mut wsum) = (0.0, 0.0, 0.0);
    for (i, &v) in x.iter().enumerate() {
        let t = 2.0 * PI * i as f64 / (n - 1.0);
        let win =
            0.35875 - 0.48829 * t.cos() + 0.14128 * (2.0 * t).cos() - 0.01168 * (3.0 * t).cos();
        let ph = 2.0 * PI * hz * i as f64 / rate;
        re += v * win * ph.cos();
        im += v * win * ph.sin();
        wsum += win;
    }
    20.0 * (2.0 * (re * re + im * im).sqrt() / wsum).log10()
}

/// The ratios every app meets on the site.
const PAIRS: [(u32, u32); 4] = [
    (96_000, 48_000),
    (48_000, 96_000),
    (48_000, 44_100),
    (44_100, 48_000),
];

#[test]
fn a_1_khz_tone_comes_through_every_ratio_above_120_db_thd_n() {
    for (from, to) in PAIRS {
        let mut c = Converter::new(from, to, 1, 32, Fixed::Input).unwrap();
        let input = tone(1_000.0, f64::from(from), 32 * 3_000, MINUS_1_DBFS);
        let out = run_fixed_input(&mut c, &input, 32);
        let settled = &out[TAPS * 2..];
        let db = thd_n_db(settled, 1_000.0, f64::from(to));
        assert!(db >= 120.0, "{from} -> {to}: THD+N {db:.1} dB");
        let lvl = level_dbfs(settled, 1_000.0, f64::from(to));
        assert!(
            (lvl + 1.0).abs() < 0.05,
            "{from} -> {to}: level {lvl:.2} dBFS"
        );
    }
}

#[test]
fn what_would_fold_back_stays_below_minus_120_dbfs() {
    // A 30 kHz tone at 96 kHz has no place at 48 kHz: it would land on
    // 18 kHz.
    let mut c = Converter::new(96_000, 48_000, 1, 32, Fixed::Input).unwrap();
    let input = tone(30_000.0, 96_000.0, 32 * 3_000, MINUS_1_DBFS);
    let out = run_fixed_input(&mut c, &input, 32);
    let settled = &out[TAPS * 2..];
    let folded = level_dbfs(settled, 18_000.0, 48_000.0);
    assert!(folded < -120.0, "folded at {folded:.1} dBFS");
    // Up: a 20 kHz tone at 48 kHz leaves no image at 28 kHz at 96 kHz.
    let mut c = Converter::new(48_000, 96_000, 1, 32, Fixed::Input).unwrap();
    let input = tone(20_000.0, 48_000.0, 32 * 3_000, MINUS_1_DBFS);
    let out = run_fixed_input(&mut c, &input, 32);
    let image = level_dbfs(&out[TAPS * 2..], 28_000.0, 96_000.0);
    assert!(image < -120.0, "image at {image:.1} dBFS");
}

#[test]
fn a_fixed_input_gives_the_exact_ratio_once_the_filter_is_full() {
    for (from, to) in PAIRS {
        let mut c = Converter::new(from, to, 1, 32, Fixed::Input).unwrap();
        let room = c.output_frames_max();
        let mut out = vec![0.0; room];
        let input = [0.0; 32];
        // The start: the filter fills (a few frames short).
        for _ in 0..1_000 {
            c.process(&input, 32, &mut out, room).unwrap();
        }
        let blocks = 2_000;
        let mut made_all = 0;
        for _ in 0..blocks {
            made_all += c.process(&input, 32, &mut out, room).unwrap().1;
        }
        let want = 32.0 * blocks as f64 * f64::from(to) / f64::from(from);
        assert!(
            (made_all as f64 - want).abs() <= 1.0,
            "{from} -> {to}: {made_all} of {want}"
        );
    }
}

#[test]
fn a_fixed_output_gives_its_block_every_call() {
    let mut c = Converter::new(48_000, 96_000, 2, 32, Fixed::Output).unwrap();
    let room = c.output_frames_max();
    assert_eq!(room, 32);
    let mut out = vec![0.0; 2 * room];
    let mut taken_all = 0;
    for _ in 0..1_000 {
        let n = c.input_frames_next();
        let input = vec![0.25; 2 * n];
        let (taken, made) = c.process(&input, n, &mut out, room).unwrap();
        assert_eq!((taken, made), (n, 32));
        taken_all += taken;
    }
    // On average half the frames go in for 2:1 up.
    assert!((taken_all as i64 - 16_000).abs() <= 2, "{taken_all}");
}

#[test]
fn channels_stay_apart() {
    let mut c = Converter::new(96_000, 48_000, 2, 32, Fixed::Input).unwrap();
    let room = c.output_frames_max();
    let mut out = vec![0.0; 2 * room];
    let mut last = (0.0, 0.0);
    for _ in 0..100 {
        let mut input = vec![0.0; 64];
        input[32..].fill(0.5);
        let (_, made) = c.process(&input, 32, &mut out, room).unwrap();
        last = (out[made - 1], out[room + made - 1]);
    }
    assert!(
        last.0.abs() < 1e-9 && (last.1 - 0.5).abs() < 1e-6,
        "{last:?}"
    );
}

#[test]
fn equal_rates_copy_with_no_delay() {
    let mut c = Converter::new(48_000, 48_000, 2, 16, Fixed::Input).unwrap();
    assert!(c.is_copy());
    assert_eq!((c.delay_frames(), c.ratio()), (0, 1.0));
    assert_eq!(
        (
            c.input_frames_next(),
            c.input_frames_max(),
            c.output_frames_max()
        ),
        (16, 16, 16)
    );
    let input: Vec<f64> = (0..32).map(f64::from).collect();
    let mut out = vec![9.0; 40];
    assert_eq!(c.process(&input, 16, &mut out, 20).unwrap(), (16, 16));
    assert_eq!(&out[..16], &input[..16]);
    assert_eq!(&out[20..36], &input[16..]);
    // Too little room is refused, not cut.
    assert!(c.process(&input, 16, &mut out, 15).is_err());
    c.reset();
}

#[test]
fn a_sinc_stage_reports_its_delay_and_forgets_on_reset() {
    let mut c = Converter::new(96_000, 48_000, 1, 32, Fixed::Input).unwrap();
    assert!(!c.is_copy());
    assert!((c.ratio() - 0.5).abs() < 1e-12);
    // Half the filter, in output frames.
    assert_eq!(c.delay_frames(), TAPS / 2 / 2);
    let room = c.output_frames_max();
    let mut out = vec![0.0; room];
    for _ in 0..50 {
        c.process(&[1.0; 32], 32, &mut out, room).unwrap();
    }
    c.reset();
    let (_, made) = c.process(&[0.0; 32], 32, &mut out, room).unwrap();
    assert!(out[..made].iter().all(|v| v.abs() < 1e-12));
}

#[test]
fn nonsense_is_refused() {
    for (from, to, ch, block) in [
        (0, 48_000, 1, 32),
        (48_000, 0, 1, 32),
        (48_000, 96_000, 0, 32),
        (48_000, 96_000, 1, 0),
    ] {
        assert!(matches!(
            Converter::new(from, to, ch, block, Fixed::Input),
            Err(ConvertError::Invalid(_))
        ));
    }
    let e = ConvertError::Invalid("x".into());
    assert_eq!(e.to_string(), "invalid converter: x");
    assert_eq!(ConvertError::Core("y".into()).to_string(), "resampler: y");
    // A wrong input length reaches the core's check, not a panic.
    let mut c = Converter::new(96_000, 48_000, 1, 32, Fixed::Input).unwrap();
    let room = c.output_frames_max();
    let mut out = vec![0.0; room];
    assert!(matches!(
        c.process(&[0.0; 8], 32, &mut out, room),
        Err(ConvertError::Core(_))
    ));
}

#[test]
fn the_smallest_device_block_converts_well_inside_its_own_time() {
    // dlink's smallest block: 16 frames at 96 kHz is 167 µs of audio. Four
    // channels down to 48 kHz must take a small part of it.
    let mut c = Converter::new(96_000, 48_000, 4, 16, Fixed::Input).unwrap();
    let room = c.output_frames_max();
    let mut out = vec![0.0; 4 * room];
    let input = vec![0.1; 64];
    for _ in 0..100 {
        c.process(&input, 16, &mut out, room).unwrap();
    }
    let t = std::time::Instant::now();
    let calls = 2_000;
    for _ in 0..calls {
        c.process(&input, 16, &mut out, room).unwrap();
    }
    let each = t.elapsed() / calls;
    assert!(
        each < std::time::Duration::from_micros(80),
        "{each:?} per 16-frame block"
    );
}

#[test]
fn a_copy_refuses_too_little_input_or_room_and_takes_exact_room() {
    let mut c = Converter::new(48_000, 48_000, 2, 16, Fixed::Input).unwrap();
    let input = vec![0.5; 32];
    let mut out = vec![0.0; 32];
    // Exactly the room: taken.
    assert_eq!(c.process(&input, 16, &mut out, 16).unwrap(), (16, 16));
    // Input shorter than its two channels.
    assert!(c.process(&input[..31], 16, &mut out, 16).is_err());
    // Output shorter than its two channels of room.
    assert!(c.process(&input, 16, &mut out[..31], 16).is_err());
    // The second channel is placed at the room's stride.
    let mut wide = vec![0.0; 40];
    let mixed: Vec<f64> = (0..32).map(|i| if i < 16 { 1.0 } else { 2.0 }).collect();
    c.process(&mixed, 16, &mut wide, 20).unwrap();
    assert!(wide[..16].iter().all(|&v| v == 1.0));
    assert!(wide[16..20].iter().all(|&v| v == 0.0));
    assert!(wide[20..36].iter().all(|&v| v == 2.0));
}

#[test]
fn a_converter_says_what_it_is() {
    let c = Converter::new(96_000, 48_000, 2, 32, Fixed::Output).unwrap();
    let text = format!("{c:?}");
    for part in [
        "from: 96000",
        "to: 48000",
        "channels: 2",
        "block: 32",
        "Output",
        "copy: false",
    ] {
        assert!(text.contains(part), "{part} missing: {text}");
    }
    let copy = Converter::new(48_000, 48_000, 1, 16, Fixed::Input).unwrap();
    assert!(format!("{copy:?}").contains("copy: true"));
}

#[test]
fn a_drifting_converter_follows_its_correction_and_a_copy_has_none() {
    let mut copy = Converter::new(48_000, 48_000, 1, 32, Fixed::Input).unwrap();
    assert!(copy.set_ppm(10.0).is_err());
    // Equal rates between two clocks: the sinc stage, not a copy.
    let mut c = Converter::drifting(48_000, 48_000, 1, 32, Fixed::Input).unwrap();
    assert!(!c.is_copy());
    let room = c.output_frames_max();
    let mut out = vec![0.0; room];
    let input = [0.0; 32];
    let run = |c: &mut Converter, out: &mut Vec<f64>, blocks: usize| -> usize {
        (0..blocks)
            .map(|_| c.process(&input, 32, out, room).unwrap().1)
            .sum()
    };
    run(&mut c, &mut out, 500);
    assert_eq!(
        run(&mut c, &mut out, 10_000),
        320_000,
        "0 ppm: one out per in"
    );
    // +250 ppm: 80 frames more over 320 000 (a ramp at the start).
    c.set_ppm(250.0).unwrap();
    run(&mut c, &mut out, 10);
    let more = run(&mut c, &mut out, 10_000);
    assert!((more as i64 - 320_080).abs() <= 1, "{more}");
    // Past the room it is held at ±1000 (songplayer's outputs' room).
    c.set_ppm(10_000.0).unwrap();
    run(&mut c, &mut out, 10);
    let most = run(&mut c, &mut out, 10_000);
    assert!((most as i64 - 320_320).abs() <= 1, "{most}");
    c.set_ppm(-10_000.0).unwrap();
    run(&mut c, &mut out, 10);
    let least = run(&mut c, &mut out, 10_000);
    assert!((least as i64 - 319_680).abs() <= 1, "{least}");
}

/// dstack #1: dlink's low-delay converter. 2:1 and 1:2 through the
/// half-band, with the sinc's bars on the audio band: a 1 kHz tone above
/// 120 dB THD+N at its level, 20 kHz at its level, the folded band and the
/// image below −145 dBFS (under a 24-bit LSB).
#[test]
fn a_low_delay_2_to_1_step_keeps_the_audio_transparent() {
    for (from, to) in [(96_000, 48_000), (48_000, 96_000)] {
        let mut c = Converter::low_delay(from, to, 1, 32, Fixed::Input).unwrap();
        assert!(c.is_half_band() && !c.is_copy(), "{from} -> {to}");
        let input = tone(1_000.0, f64::from(from), 32 * 3_000, MINUS_1_DBFS);
        let out = run_fixed_input(&mut c, &input, 32);
        let settled = &out[halfband::TAPS * 2..];
        let db = thd_n_db(settled, 1_000.0, f64::from(to));
        assert!(db >= 120.0, "{from} -> {to}: THD+N {db:.1} dB");
        let lvl = level_dbfs(settled, 1_000.0, f64::from(to));
        assert!(
            (lvl + 1.0).abs() < 0.0001,
            "{from} -> {to}: level {lvl:.5} dBFS"
        );
        // 20 kHz, the passband's edge, at its level too.
        let mut c = Converter::low_delay(from, to, 1, 32, Fixed::Input).unwrap();
        let input = tone(20_000.0, f64::from(from), 32 * 3_000, MINUS_1_DBFS);
        let out = run_fixed_input(&mut c, &input, 32);
        let lvl = level_dbfs(&out[halfband::TAPS * 2..], 20_000.0, f64::from(to));
        assert!(
            (lvl + 1.0).abs() < 0.0001,
            "{from} -> {to}: 20 kHz at {lvl:.5} dBFS"
        );
    }
    let mut c = Converter::low_delay(96_000, 48_000, 1, 32, Fixed::Input).unwrap();
    let input = tone(30_000.0, 96_000.0, 32 * 3_000, MINUS_1_DBFS);
    let out = run_fixed_input(&mut c, &input, 32);
    let folded = level_dbfs(&out[halfband::TAPS * 2..], 18_000.0, 48_000.0);
    assert!(folded < -145.0, "folded at {folded:.1} dBFS");
    let mut c = Converter::low_delay(48_000, 96_000, 1, 32, Fixed::Input).unwrap();
    let input = tone(20_000.0, 48_000.0, 32 * 3_000, MINUS_1_DBFS);
    let out = run_fixed_input(&mut c, &input, 32);
    let image = level_dbfs(&out[halfband::TAPS * 2..], 28_000.0, 96_000.0);
    assert!(image < -145.0, "image at {image:.1} dBFS");
}

#[test]
fn a_low_delay_step_delays_a_fraction_of_the_sinc() {
    // 96 -> 48 (a device's RX into a 48 kHz app): 30 frames at 48 kHz
    // (0.63 ms) against the sinc's 64 (1.33 ms); 48 -> 96 (the app's TX):
    // 61 at 96 kHz (0.64 ms) against 256 (2.67 ms).
    let rx = Converter::low_delay(96_000, 48_000, 2, 128, Fixed::Input).unwrap();
    let tx = Converter::low_delay(48_000, 96_000, 2, 128, Fixed::Output).unwrap();
    assert_eq!((rx.delay_frames(), tx.delay_frames()), (30, 61));
    let sinc_rx = Converter::new(96_000, 48_000, 2, 128, Fixed::Input).unwrap();
    let sinc_tx = Converter::new(48_000, 96_000, 2, 128, Fixed::Output).unwrap();
    assert_eq!((sinc_rx.delay_frames(), sinc_tx.delay_frames()), (64, 256));
    // The frames each side of a call: exact, every call.
    assert_eq!(
        (
            rx.input_frames_next(),
            rx.input_frames_max(),
            rx.output_frames_max()
        ),
        (128, 128, 64)
    );
    assert_eq!(
        (
            tx.input_frames_next(),
            tx.input_frames_max(),
            tx.output_frames_max()
        ),
        (64, 64, 128)
    );
    let down_out = Converter::low_delay(96_000, 48_000, 1, 64, Fixed::Output).unwrap();
    assert_eq!(
        (down_out.input_frames_next(), down_out.output_frames_max()),
        (128, 64)
    );
    let up_in = Converter::low_delay(48_000, 96_000, 1, 64, Fixed::Input).unwrap();
    assert_eq!(
        (up_in.input_frames_next(), up_in.output_frames_max()),
        (64, 128)
    );
}

#[test]
fn a_low_delay_converter_is_the_sinc_for_any_other_pair() {
    // 44.1 kHz, an odd block and equal rates are not a half-band step.
    let cd = Converter::low_delay(44_100, 96_000, 1, 128, Fixed::Output).unwrap();
    assert!(!cd.is_half_band() && !cd.is_copy());
    assert_eq!(
        cd.delay_frames(),
        Converter::new(44_100, 96_000, 1, 128, Fixed::Output)
            .unwrap()
            .delay_frames()
    );
    let odd = Converter::low_delay(96_000, 48_000, 1, 33, Fixed::Input).unwrap();
    assert!(!odd.is_half_band() && !odd.is_copy());
    let same = Converter::low_delay(48_000, 48_000, 1, 32, Fixed::Input).unwrap();
    assert!(same.is_copy() && !same.is_half_band());
    for (from, to, ch, block) in [
        (0, 0, 1, 32),
        (0, 48_000, 1, 32),
        (96_000, 48_000, 0, 32),
        (96_000, 48_000, 1, 0),
    ] {
        assert!(
            matches!(
                Converter::low_delay(from, to, ch, block, Fixed::Input),
                Err(ConvertError::Invalid(_))
            ),
            "{from} {to} {ch} {block}"
        );
    }
}

#[test]
fn a_low_delay_step_refuses_a_wrong_call_and_has_no_ratio_to_adjust() {
    let mut c = Converter::low_delay(48_000, 96_000, 2, 32, Fixed::Output).unwrap();
    assert!(format!("{c:?}").contains("copy: false"));
    assert!(c.set_ppm(10.0).is_err());
    let input = vec![0.25; 2 * 16];
    let mut out = vec![0.0; 2 * 32];
    assert_eq!(c.process(&input, 16, &mut out, 32).unwrap(), (16, 32));
    // Exactly the room is taken; not the frames it takes, too little
    // room, short buffers: refused.
    let mut wide = vec![0.0; 2 * 40];
    assert_eq!(c.process(&input, 16, &mut wide, 40).unwrap(), (16, 32));
    assert!(c.process(&input, 15, &mut out, 32).is_err());
    assert!(c.process(&input, 16, &mut out, 31).is_err());
    assert!(c.process(&input[..31], 16, &mut out, 32).is_err());
    assert!(c.process(&input, 16, &mut out[..63], 32).is_err());
    // Channels stay apart, each at its level once the filter is full.
    let mut split = vec![0.0; 2 * 16];
    split[16..].fill(0.5);
    for _ in 0..20 {
        c.process(&split, 16, &mut out, 32).unwrap();
    }
    assert!(out[31].abs() < 1e-12 && (out[63] - 0.5).abs() < 1e-9);
    c.reset();
    c.process(&[0.0; 32], 16, &mut out, 32).unwrap();
    assert!(out.iter().all(|&v| v == 0.0));
}
