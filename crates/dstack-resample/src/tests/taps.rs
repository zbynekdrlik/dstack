// SPDX-License-Identifier: MIT OR Apache-2.0

//! A drifting converter's sinc length (dstack #382, the owner's ROZHODNUTÉ
//! of 17.11.2026: an ASIO bridge at 96 <-> 96 kHz runs the shortest filter
//! that keeps the audio band). Measured at 96 <-> 96 kHz, +7.5 ppm (dsg's
//! driver), a -1 dBFS tone from 20 Hz to 20 kHz, the settled three quarters:
//! 48 taps within 5e-7 dB of the level everywhere, 32 taps -0.4016 dB at
//! 20 kHz; THD+N at 1 kHz 209.6 (256), 212.4 (48), 205.4 dB (32).

use super::*;

const RATE: u32 = 96_000;
/// dsg's driver against Dante, ppm (dasio#120).
const DRIFT_PPM: f64 = 7.5;
const BLOCK: usize = 32;
const BLOCKS: usize = 3_000;
/// The audio band's points, Hz.
const BAND: [f64; 9] = [
    20.0, 100.0, 1_000.0, 5_000.0, 10_000.0, 15_000.0, 18_000.0, 19_000.0, 20_000.0,
];

/// A drifting 96 <-> 96 kHz converter of `taps` (fixed input of
/// [`BLOCK`]) at [`DRIFT_PPM`].
fn drifting(taps: usize) -> Converter {
    let mut c = Converter::drifting_with_taps(RATE, RATE, 1, BLOCK, Fixed::Input, taps).unwrap();
    c.set_ppm(DRIFT_PPM).unwrap();
    c
}

/// A `hz` tone at -1 dBFS through `c`, the settled three quarters, and the
/// tone's frequency there (the drift moves it).
fn through(c: &mut Converter, hz: f64) -> (Vec<f64>, f64) {
    let input = tone(hz, f64::from(RATE), BLOCK * BLOCKS, MINUS_1_DBFS);
    let out = run_fixed_input(c, &input, BLOCK);
    (out[out.len() / 4..].to_vec(), hz / (1.0 + DRIFT_PPM * 1e-6))
}

/// The level against -1 dBFS at each of [`BAND`]'s points, dB.
fn band(taps: usize) -> Vec<(f64, f64)> {
    BAND.iter()
        .map(|&hz| {
            let (out, at) = through(&mut drifting(taps), hz);
            (hz, level_dbfs(&out, at, f64::from(RATE)) + 1.0)
        })
        .collect()
}

/// The delay of a 96 <-> 96 kHz converter of `taps` with a fixed output
/// of 128 frames (dlink's player), ms.
fn delay_ms(taps: usize) -> f64 {
    let c = Converter::drifting_with_taps(RATE, RATE, 2, 128, Fixed::Output, taps).unwrap();
    c.delay_frames() as f64 * 1e3 / f64::from(RATE)
}

/// [`TAPS`] through `drifting_with_taps` is the converter `drifting` makes:
/// the same delay (1.333 ms at 96 <-> 96 kHz) and the same samples.
#[test]
fn the_default_length_is_the_drifting_converter_unchanged() {
    assert!(
        (delay_ms(TAPS) - 4.0 / 3.0).abs() < 1e-9,
        "{}",
        delay_ms(TAPS)
    );
    let mut plain = Converter::drifting(RATE, RATE, 1, BLOCK, Fixed::Input).unwrap();
    plain.set_ppm(DRIFT_PPM).unwrap();
    let input = tone(19_000.0, f64::from(RATE), BLOCK * 500, MINUS_1_DBFS);
    let a = run_fixed_input(&mut plain, &input, BLOCK);
    let b = run_fixed_input(&mut drifting(TAPS), &input, BLOCK);
    assert_eq!(a, b);
}

/// 48 taps: 0.250 ms, the audio band flat (measured within 5e-7 dB) and
/// the THD+N far past any 24-bit floor (212.4 dB measured).
#[test]
fn forty_eight_taps_delay_a_quarter_ms_and_keep_the_band_flat() {
    assert!((delay_ms(48) - 0.25).abs() < 1e-9, "{}", delay_ms(48));
    for (hz, db) in band(48) {
        assert!(db.abs() < 0.001, "{hz} Hz: {db:+.6} dB");
    }
    let (out, at) = through(&mut drifting(48), 1_000.0);
    let thd = thd_n_db(&out, at, f64::from(RATE));
    assert!(thd > 200.0, "THD+N {thd:.1} dB");
}

/// 32 taps: 0.167 ms, flat to 5 kHz, the top of the band cut (measured
/// -0.4016 dB at 20 kHz).
#[test]
fn thirty_two_taps_delay_a_sixth_ms_and_cut_the_top_of_the_band() {
    assert!((delay_ms(32) - 1.0 / 6.0).abs() < 1e-9, "{}", delay_ms(32));
    let levels = band(32);
    for &(hz, db) in &levels[..4] {
        assert!(db.abs() < 0.001, "{hz} Hz: {db:+.6} dB");
    }
    let (hz, top) = levels[levels.len() - 1];
    assert!((-0.45..-0.35).contains(&top), "{hz} Hz: {top:+.4} dB");
}

/// A length the core would round (not a multiple of [`TAPS_STEP`]) or 0
/// is refused; any multiple is made as asked.
#[test]
fn a_length_off_the_core_s_step_is_refused() {
    for taps in [0, 4, 31, 33, 36, 257] {
        let made = Converter::drifting_with_taps(RATE, RATE, 1, BLOCK, Fixed::Output, taps);
        assert!(
            matches!(&made, Err(ConvertError::Invalid(why)) if why.contains(&taps.to_string())),
            "{taps}: {made:?}"
        );
    }
    for taps in [8, 32, 40, 48, 256, 512] {
        let c = Converter::drifting_with_taps(RATE, RATE, 1, BLOCK, Fixed::Output, taps).unwrap();
        assert_eq!(c.delay_frames(), taps / 2, "{taps}");
        assert!(!c.is_copy());
    }
}
