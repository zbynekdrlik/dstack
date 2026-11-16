//! `Converter::skip_silence` (dstack #341, #343): a restart steps the
//! converter's phase to any frame of a silent stream with no filter work,
//! in calls for the rest of one ratio period only.

use super::*;

/// The scratch a skip needs: room for a call at its largest.
fn scratch(c: &Converter, channels: usize) -> (Vec<f64>, Vec<f64>) {
    (
        vec![0.0; channels * c.input_frames_max()],
        vec![0.0; channels * c.output_frames_max()],
    )
}

/// One call of `c` with `channels` channels of `input` (taken from the
/// front, each channel `input[c]`), the outputs appended per channel;
/// returns the frames taken and given.
fn call(
    c: &mut Converter,
    input: &[Vec<f64>],
    at: &mut usize,
    heard: &mut [Vec<f64>],
) -> (usize, usize) {
    let channels = input.len();
    let next = c.input_frames_next();
    let flat: Vec<f64> = input
        .iter()
        .flat_map(|ch| ch[*at..*at + next].iter().copied())
        .collect();
    let room = c.output_frames_max();
    let mut out = vec![0.0; channels * room];
    let (took, gave) = c.process(&flat, next, &mut out, room).unwrap();
    *at += took;
    for (ch, h) in heard.iter_mut().enumerate() {
        h.extend_from_slice(&out[ch * room..ch * room + gave]);
    }
    (took, gave)
}

/// A tone at `hz` on each channel, long enough for [`listen`].
fn tones(c: &Converter, hz: &[f64], rate: u32) -> Vec<Vec<f64>> {
    let len = ((c.delay_frames() + LISTEN) as f64 / c.ratio()) as usize + 4 * c.input_frames_max();
    hz.iter()
        .map(|&f| tone(f, f64::from(rate), len, 0.5))
        .collect()
}

/// The output frames [`listen`] gathers past the converter's delay.
const LISTEN: usize = 600;

/// Calls `c` on `input` until it gave [`LISTEN`] frames past its delay;
/// returns those frames.
fn listen(c: &mut Converter, input: &[Vec<f64>]) -> Vec<Vec<f64>> {
    let mut heard = vec![Vec::new(); input.len()];
    let mut at = 0;
    let delay = c.delay_frames();
    while heard[0].len() < delay + LISTEN {
        call(c, input, &mut at, &mut heard);
    }
    heard
        .into_iter()
        .map(|h| h[delay..delay + LISTEN].to_vec())
        .collect()
}

/// The rate pairs and blocks the skips are checked on: 44.1 kHz on 96 kHz
/// (dasio), 48 kHz on 44.1 kHz (period 147 device frames), 11.025 kHz on
/// 192 kHz at 16 frames (2560), each way.
const PAIRS: [(u32, u32, usize); 3] = [
    (96_000, 44_100, 128),
    (44_100, 48_000, 128),
    (192_000, 11_025, 16),
];

/// A skip of whole blocks lands where that many calls of silence through
/// the filter land: the same frames taken and given (but where the core's
/// float `floor` leaves an output on a whole boundary to the next call:
/// one frame, on that boundary only), the same next input, and then the
/// same samples of a tone, on a fixed input and a fixed output, from 0 to
/// more than two ratio periods.
#[test]
fn a_skip_lands_where_calls_of_silence_land() {
    for (device, app, n) in PAIRS {
        for (fixed, from, to) in [(Fixed::Input, device, app), (Fixed::Output, app, device)] {
            let mut run = Converter::new(from, to, 2, n, fixed).unwrap();
            let mut skipped = Converter::new(from, to, 2, n, fixed).unwrap();
            let (si, mut so) = scratch(&skipped, 2);
            let (p_in, p_out) = run.period();
            let p_fixed = if fixed == Fixed::Input { p_in } else { p_out };
            let silence = vec![vec![0.0; run.input_frames_max()]; 2];
            let mut sink = vec![Vec::new(), Vec::new()];
            let (mut took, mut gave) = (0u64, 0u64);
            let mut exact = 0;
            let blocks = (2 * p_fixed / n as u64 + 3) as usize;
            for k in 0..blocks {
                let frames = (k * n) as u64;
                skipped.reset();
                let got = skipped.skip_silence(frames, &si, &mut so).unwrap();
                let at = format!("{from}->{to} {fixed:?} {frames}");
                // The fixed side matches exactly; the other may differ by
                // one output on a whole boundary.
                if got == (took, gave) {
                    exact += 1;
                    assert_eq!(skipped.input_frames_next(), run.input_frames_next(), "{at}");
                } else {
                    // Fixed input: `floor((T - 2) * r)` on a whole value;
                    // fixed output: `ceil(J / r + 1)` on a whole value.
                    let (a, b, same, whole) = if fixed == Fixed::Input {
                        let whole = frames >= 2 && ((frames - 2) * p_out).is_multiple_of(p_in);
                        (got.1, gave, got.0 == took, whole)
                    } else {
                        (
                            got.0,
                            took,
                            got.1 == gave,
                            (frames * p_in).is_multiple_of(p_out),
                        )
                    };
                    assert!(same, "{at}: {got:?} vs {:?}", (took, gave));
                    assert_eq!(a.abs_diff(b), 1, "{at}: {got:?} vs {:?}", (took, gave));
                    assert!(whole, "{at}: off a whole boundary");
                }
                let mut at_in = 0;
                let (t, g) = call(&mut run, &silence, &mut at_in, &mut sink);
                took += t as u64;
                gave += g as u64;
            }
            assert!(
                exact + 3 > blocks,
                "{from}->{to} {fixed:?}: {exact} of {blocks}"
            );
        }
    }
}

/// After a skip, a tone comes out sample for sample as after the same
/// calls of silence, on two channels; at frames that are no whole
/// boundary.
#[test]
fn after_a_skip_audio_comes_out_as_after_calls_of_silence() {
    for (device, app, n) in PAIRS {
        for (fixed, from, to) in [(Fixed::Input, device, app), (Fixed::Output, app, device)] {
            for blocks in [1usize, 3, 7] {
                let mut run = Converter::new(from, to, 2, n, fixed).unwrap();
                let mut skipped = Converter::new(from, to, 2, n, fixed).unwrap();
                let (si, mut so) = scratch(&skipped, 2);
                let silence = vec![vec![0.0; 8 * run.input_frames_max()]; 2];
                let mut sink = vec![Vec::new(), Vec::new()];
                let mut at = 0;
                for _ in 0..blocks {
                    call(&mut run, &silence, &mut at, &mut sink);
                }
                skipped
                    .skip_silence((blocks * n) as u64, &si, &mut so)
                    .unwrap();
                let input = tones(&run, &[1_000.0, 3_000.0], from);
                let (a, b) = (listen(&mut run, &input), listen(&mut skipped, &input));
                let what = format!("{from}->{to} {fixed:?} {blocks}");
                for ch in 0..2 {
                    for i in 0..LISTEN {
                        assert!(
                            (a[ch][i] - b[ch][i]).abs() < 1e-9,
                            "{what}: channel {ch}, frame {i}"
                        );
                    }
                }
                assert!(a[0].iter().any(|v| v.abs() > 0.1), "{what}: no tone");
            }
        }
    }
}

/// The phase repeats with the ratio's period: a skip of a frame count far
/// past it (and off the block grid) gives the counts of its rest plus
/// whole periods, and then the same samples as the skip of that rest; and
/// two skips add up to one.
#[test]
fn a_long_skip_is_its_rest_plus_whole_periods() {
    for (device, app, n) in PAIRS {
        for (fixed, from, to) in [(Fixed::Input, device, app), (Fixed::Output, app, device)] {
            let mut far = Converter::new(from, to, 1, n, fixed).unwrap();
            let mut near = Converter::new(from, to, 1, n, fixed).unwrap();
            let mut split = Converter::new(from, to, 1, n, fixed).unwrap();
            let (si, mut so) = scratch(&far, 1);
            let (p_in, p_out) = far.period();
            let p_fixed = if fixed == Fixed::Input { p_in } else { p_out };
            let what = format!("{from}->{to} {fixed:?}");
            for rest in [2u64, 7, p_fixed / 2 + 3, p_fixed - 1] {
                let periods = 1_000_003u64;
                far.reset();
                near.reset();
                split.reset();
                let (fi, fo) = far
                    .skip_silence(rest + periods * p_fixed, &si, &mut so)
                    .unwrap();
                let (ni, no) = near.skip_silence(rest, &si, &mut so).unwrap();
                assert_eq!(
                    (fi - ni, fo - no),
                    (periods * p_in, periods * p_out),
                    "{what} {rest}"
                );
                let (ai, ao) = split.skip_silence(rest / 2, &si, &mut so).unwrap();
                let (bi, bo) = split.skip_silence(rest - rest / 2, &si, &mut so).unwrap();
                assert_eq!((ai + bi, ao + bo), (ni, no), "{what} {rest}: two skips");
                let input = tones(&far, &[1_000.0], from);
                let (a, b, c) = (
                    listen(&mut far, &input),
                    listen(&mut near, &input),
                    listen(&mut split, &input),
                );
                let same = |p: &[f64], q: &[f64]| {
                    p[..LISTEN].iter().zip(q).all(|(x, y)| (x - y).abs() < 1e-9)
                };
                assert!(same(&a[0], &b[0]), "{what} {rest}");
                assert!(same(&b[0], &c[0]), "{what} {rest}");
                assert!(a[0].iter().any(|v| v.abs() > 0.1), "{what} {rest}: no tone");
            }
            // Under 2 frames of rest, a whole period more runs: the core's
            // first 2 frames do not repeat.
            for rest in [0u64, 1] {
                far.reset();
                near.reset();
                let (fi, fo) = far.skip_silence(rest + 5 * p_fixed, &si, &mut so).unwrap();
                let (ni, no) = near.skip_silence(rest + p_fixed, &si, &mut so).unwrap();
                assert_eq!((fi - ni, fo - no), (4 * p_in, 4 * p_out), "{what} {rest}");
                assert_eq!(
                    far.input_frames_next(),
                    near.input_frames_next(),
                    "{what} {rest}"
                );
            }
            // A skip of nothing moves nothing.
            far.reset();
            assert_eq!(far.skip_silence(0, &si, &mut so), Ok((0, 0)));
        }
    }
}

/// A skip moves nothing and is refused after audio or a ratio change; a
/// reset allows it again, and a skip keeps it allowed. Scratch too small
/// for a call is the core's error, and the converter is reset.
#[test]
fn a_skip_is_refused_after_audio_or_a_ratio_change_until_a_reset() {
    let mut c = Converter::new(96_000, 44_100, 1, 128, Fixed::Input).unwrap();
    let (si, mut so) = scratch(&c, 1);
    let room = c.output_frames_max();
    let mut out = vec![0.0; room];
    c.process(&[0.5; 128], 128, &mut out, room).unwrap();
    assert!(matches!(
        c.skip_silence(128, &si, &mut so),
        Err(ConvertError::Invalid(_))
    ));
    c.reset();
    assert!(c.skip_silence(128, &si, &mut so).is_ok());
    assert!(c.skip_silence(128, &si, &mut so).is_ok());
    assert!(matches!(
        c.skip_silence(128, &si[..64], &mut so),
        Err(ConvertError::Core(_))
    ));
    // Reset by the error: a skip from the start again gives what a new
    // converter's does.
    let fresh = Converter::new(96_000, 44_100, 1, 128, Fixed::Input)
        .unwrap()
        .skip_silence(300, &si, &mut so.clone());
    assert_eq!(c.skip_silence(300, &si, &mut so), fresh);
    assert!(matches!(
        c.skip_silence(128, &si, &mut so[..10]),
        Err(ConvertError::Core(_))
    ));
    // A drifting converter's ratio moved: refused until a reset.
    let mut d = Converter::drifting(96_000, 44_100, 1, 128, Fixed::Input).unwrap();
    let (di, mut dout) = scratch(&d, 1);
    d.set_ppm(50.0).unwrap();
    assert!(matches!(
        d.skip_silence(128, &di, &mut dout),
        Err(ConvertError::Invalid(_))
    ));
    d.reset();
    assert!(d.skip_silence(128, &di, &mut dout).is_ok());
}

/// A copy and a half-band step have no phase between frames: a skip only
/// counts (the half-band's 2:1 side in whole steps), and is refused after
/// audio.
#[test]
fn a_skip_of_a_copy_or_a_half_band_step_only_counts() {
    for (to, fixed, frames, want) in [
        (96_000u32, Fixed::Input, 33u64, (33u64, 33u64)),
        (48_000, Fixed::Input, 34, (34, 17)),
        (192_000, Fixed::Input, 33, (33, 66)),
        (48_000, Fixed::Output, 33, (66, 33)),
        (192_000, Fixed::Output, 34, (17, 34)),
    ] {
        let mut c = Converter::low_delay(96_000, to, 1, 32, fixed).unwrap();
        assert!(c.is_copy() || c.is_half_band(), "{to}");
        let (si, mut so) = scratch(&c, 1);
        assert_eq!(
            c.skip_silence(frames, &si, &mut so),
            Ok(want),
            "{to} {fixed:?}"
        );
        let take = c.input_frames_next();
        let mut out = [0.0; 64];
        c.process(&[0.0; 64], take, &mut out, 64).unwrap();
        assert!(
            c.skip_silence(frames, &si, &mut so).is_err(),
            "{to} {fixed:?}"
        );
    }
    // The 2:1 side moves in whole steps.
    for (to, fixed) in [(48_000u32, Fixed::Input), (192_000, Fixed::Output)] {
        let mut c = Converter::low_delay(96_000, to, 1, 32, fixed).unwrap();
        let (si, mut so) = scratch(&c, 1);
        assert!(matches!(
            c.skip_silence(33, &si, &mut so),
            Err(ConvertError::Invalid(_))
        ));
    }
}

/// A ratio's period: the fewest whole frames on both sides at once.
#[test]
fn a_ratio_s_period_is_its_fewest_whole_frames() {
    for (from, to, period) in [
        (96_000u32, 44_100u32, (320u64, 147u64)),
        (44_100, 96_000, (147, 320)),
        (44_100, 48_000, (147, 160)),
        (192_000, 11_025, (2_560, 147)),
        (96_000, 48_000, (2, 1)),
        (48_000, 48_000, (1, 1)),
    ] {
        let c = Converter::new(from, to, 1, 32, Fixed::Input).unwrap();
        assert_eq!(c.period(), period, "{from}->{to}");
    }
    assert_eq!(gcd(7, 0), 7);
    assert_eq!(gcd(0, 7), 7);
}
