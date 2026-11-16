//! `Converter::skip_silence` (dstack #341): a restart's silent blocks move
//! the converter's phase with no filter work.

use super::*;

/// A skip after a reset is a call of silence, frame for frame and sample
/// for sample after it, with nothing written; on two channels, both sides
/// fixed, after a reset of a converter that ran audio.
#[test]
fn a_skip_of_silence_moves_the_phase_as_a_call_of_silence_would() {
    for fixed in [Fixed::Input, Fixed::Output] {
        let mut skipped = Converter::new(96_000, 44_100, 2, 128, fixed).unwrap();
        let mut run = Converter::new(96_000, 44_100, 2, 128, fixed).unwrap();
        let (room_in, room) = (skipped.input_frames_max(), skipped.output_frames_max());
        let (mut a, mut b) = (vec![0.0; 2 * room], vec![0.0; 2 * room]);
        // Audio first, then a reset: the skip needs only a silent history.
        let loud = vec![0.7; 2 * room_in];
        let next = skipped.input_frames_next();
        skipped.process(&loud, next, &mut a, room).unwrap();
        skipped.reset();
        for call in 0..3 {
            let next = run.input_frames_next();
            assert_eq!(skipped.input_frames_next(), next, "{fixed:?} {call}");
            let silence = vec![0.0; 2 * next];
            a.fill(9.0);
            let one = skipped.skip_silence(&silence, next, &mut a, room).unwrap();
            let two = run.process(&silence, next, &mut b, room).unwrap();
            assert_eq!(one, two, "{fixed:?} {call}");
            assert!(a.iter().all(|&v| v == 9.0), "{fixed:?} {call}: written");
        }
        // Then the same tones (one per channel) give the same samples.
        let left = tone(1_000.0, 96_000.0, 8 * room_in, 0.5);
        let right = tone(3_000.0, 96_000.0, 8 * room_in, 0.5);
        let mut at = 0;
        let mut heard = false;
        for call in 0..6 {
            let next = run.input_frames_next();
            assert_eq!(skipped.input_frames_next(), next, "{fixed:?} {call}");
            let input: Vec<f64> = left[at..at + next]
                .iter()
                .chain(&right[at..at + next])
                .copied()
                .collect();
            at += next;
            let one = skipped.process(&input, next, &mut a, room).unwrap();
            let two = run.process(&input, next, &mut b, room).unwrap();
            assert_eq!(one, two, "{fixed:?} {call}");
            assert_eq!(a[..one.1], b[..two.1], "{fixed:?} {call}");
            assert_eq!(
                a[room..room + one.1],
                b[room..room + two.1],
                "{fixed:?} {call}"
            );
            heard |= a[..one.1].iter().any(|v| v.abs() > 0.1);
        }
        assert!(heard, "{fixed:?}: the tone never came through");
    }
}

/// A skip after audio is refused (the history would not be silence); a
/// reset allows it again, and a skip keeps it allowed.
#[test]
fn a_skip_after_audio_is_refused_until_a_reset() {
    let mut c = Converter::new(96_000, 44_100, 1, 128, Fixed::Input).unwrap();
    let room = c.output_frames_max();
    let mut out = vec![0.0; room];
    c.process(&[0.5; 128], 128, &mut out, room).unwrap();
    assert!(matches!(
        c.skip_silence(&[0.0; 128], 128, &mut out, room),
        Err(ConvertError::Invalid(_))
    ));
    c.reset();
    assert!(c.skip_silence(&[0.0; 128], 128, &mut out, room).is_ok());
    assert!(c.skip_silence(&[0.0; 128], 128, &mut out, room).is_ok());
    // Buffers too small for the call: the core's error.
    assert!(matches!(
        c.skip_silence(&[0.0; 64], 128, &mut out, room),
        Err(ConvertError::Core(_))
    ));
    assert!(matches!(
        c.skip_silence(&[0.0; 128], 128, &mut out[..10], room),
        Err(ConvertError::Core(_))
    ));
}

/// A copy and a half-band step have no phase between frames: a skip
/// counts what the call would take and give, refuses what the call
/// refuses, and is refused after audio.
#[test]
fn a_skip_of_a_copy_or_a_half_band_step_only_counts() {
    for (to, fixed, take, give) in [
        (96_000u32, Fixed::Input, 32, 32),
        (48_000, Fixed::Input, 32, 16),
        (48_000, Fixed::Output, 64, 32),
        (192_000, Fixed::Output, 16, 32),
    ] {
        let mut c = Converter::low_delay(96_000, to, 1, 32, fixed).unwrap();
        assert!(c.is_copy() || c.is_half_band(), "{to}");
        let mut out = [0.0; 64];
        assert_eq!(
            c.skip_silence(&[0.0; 64], take, &mut out, give),
            Ok((take, give)),
            "{to} {fixed:?}"
        );
        // Room for one frame less, or another input: refused.
        assert!(
            c.skip_silence(&[0.0; 64], take, &mut out, give - 1)
                .is_err()
        );
        assert!(c.skip_silence(&[0.0; 64], take + 2, &mut out, 64).is_err());
        c.process(&[0.0; 64], take, &mut out, give).unwrap();
        assert!(c.skip_silence(&[0.0; 64], take, &mut out, give).is_err());
    }
}
