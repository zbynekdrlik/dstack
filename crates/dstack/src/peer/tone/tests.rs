// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

const RATE: u32 = 48_000;
const BLOCK: usize = 128;

#[test]
fn the_tone_s_phase_comes_from_the_media_sample() {
    assert_eq!(tone_at(0, 1000, RATE), 0.0);
    // A quarter period of 1 kHz at 48 kHz: 12 samples.
    assert!((tone_at(12, 1000, RATE) - LEVEL).abs() < 1e-12);
    assert!((tone_at(36, 1000, RATE) + LEVEL).abs() < 1e-12);
    // Whole seconds later: the same sample, however large the media time.
    let n = 1_800_000_000 * u64::from(RATE) + 12;
    assert!((tone_at(n, 1000, RATE) - LEVEL).abs() < 1e-12);
    assert!((tone_at(n, 997, RATE) - tone_at(12, 997, RATE)).abs() < 1e-12);
}

#[test]
fn a_frequency_reads_back_from_its_c() {
    let c = (std::f64::consts::TAU * 1000.0 / 48_000.0).cos();
    assert!((hz_of(c, RATE) - 1000.0).abs() < 1e-9);
    assert!((hz_of(c, 96_000) - 2000.0).abs() < 1e-9);
}

/// The tone's samples from `from` on, `len` of them.
fn tone(from: u64, len: usize, hz: u32) -> Vec<f64> {
    (0..len as u64)
        .map(|i| tone_at(from + i, hz, RATE))
        .collect()
}

#[test]
fn a_clean_sine_fits_with_its_frequency_and_level() {
    for hz in [200, 440, 1000, 5000, 15_000] {
        let x = tone(1_000, BLOCK + 2, hz);
        let f = fit([x[0], x[1]], &x[2..]).unwrap_or_else(|| panic!("{hz} Hz fits"));
        assert!(
            (hz_of(f.c, RATE) - f64::from(hz)).abs() < 1e-6,
            "{hz}: {f:?}"
        );
        assert!((f.amplitude() - LEVEL).abs() < 1e-9, "{hz}: {f:?}");
        assert!(
            (f.threshold - LEVEL * (1.0 - f.c.abs()) / 4.0).abs() < 1e-12,
            "{hz}: {f:?}"
        );
    }
}

#[test]
fn silence_dc_a_quiet_tone_or_a_glitch_do_not_fit() {
    assert_eq!(fit([0.0, 0.0], &[0.0; BLOCK]), None);
    assert_eq!(fit([0.3, 0.3], &[0.3; BLOCK]), None, "DC");
    let quiet: Vec<f64> = tone(0, BLOCK + 2, 1000)
        .iter()
        .map(|x| x / 1000.0)
        .collect();
    assert_eq!(fit([quiet[0], quiet[1]], &quiet[2..]), None, "-66 dBFS");
    // Just over the least amplitude: locked.
    let loud: Vec<f64> = tone(0, BLOCK + 2, 1000).iter().map(|x| x / 450.0).collect();
    assert!(fit([loud[0], loud[1]], &loud[2..]).is_some());
    let mut skip = tone(0, BLOCK + 3, 1000);
    skip.remove(70);
    assert_eq!(
        fit([skip[0], skip[1]], &skip[2..]),
        None,
        "a skipped sample"
    );
}

/// Feeds `x` in blocks of [`BLOCK`].
fn feed(check: &mut ToneCheck, x: &[f64]) {
    for b in x.chunks(BLOCK) {
        check.block(b);
    }
}

#[test]
fn an_unbroken_tone_has_no_gap() {
    for hz in [200, 440, 1000, 9_000] {
        let mut check = ToneCheck::default();
        feed(&mut check, &tone(5, 200 * BLOCK, hz));
        assert_eq!(check.gaps(), 0, "{hz}");
        assert!(check.fit().is_some());
    }
}

#[test]
fn a_skipped_or_repeated_sample_or_a_dropout_is_one_gap_each() {
    let base = tone(0, 100 * BLOCK, 440);
    let mut skipped = base.clone();
    skipped.remove(40 * BLOCK + 17);
    let mut repeated = base.clone();
    repeated.insert(40 * BLOCK + 17, repeated[40 * BLOCK + 17]);
    let mut dropout = base.clone();
    dropout[40 * BLOCK..40 * BLOCK + 300].fill(0.0);
    let mut two = skipped.clone();
    two.remove(70 * BLOCK);
    for (name, x, gaps) in [
        ("skipped", skipped, 1),
        ("repeated", repeated, 1),
        ("dropout", dropout, 1),
        ("two skips", two, 2),
    ] {
        let mut check = ToneCheck::default();
        feed(&mut check, &x);
        assert_eq!(check.gaps(), gaps, "{name}");
    }
}

#[test]
fn a_skip_at_a_block_boundary_is_a_gap_too() {
    let mut x = tone(0, 50 * BLOCK, 1000);
    x.remove(20 * BLOCK);
    let mut check = ToneCheck::default();
    feed(&mut check, &x);
    assert_eq!(check.gaps(), 1);
}

#[test]
fn silence_before_the_tone_is_no_gap() {
    let mut x = vec![0.0; 10 * BLOCK + 50];
    x.extend(tone(0, 50 * BLOCK, 1000));
    let mut check = ToneCheck::default();
    feed(&mut check, &x);
    assert_eq!(check.gaps(), 0);
}

#[test]
fn a_gap_ends_after_its_good_run() {
    let mut check = ToneCheck::default();
    check.judge(true);
    for _ in 0..CLEAR - 1 {
        check.judge(false);
    }
    check.judge(true);
    assert_eq!(check.gaps(), 1, "{} good samples keep the gap", CLEAR - 1);
    for _ in 0..CLEAR {
        check.judge(false);
    }
    check.judge(true);
    assert_eq!(check.gaps(), 2);
}

#[test]
fn short_blocks_judge_once_two_samples_were_seen() {
    let x = tone(0, 40 * BLOCK, 1000);
    let mut check = ToneCheck::default();
    check.block(&x[..1]);
    check.block(&x[1..2]);
    feed(&mut check, &x[2..]);
    assert_eq!(check.gaps(), 0);
    assert!(check.fit().is_some());
}

#[test]
fn the_counts_publish_to_the_status() {
    let out = RxTone::default();
    let mut check = ToneCheck::default();
    check.publish(&out, 7, RATE);
    assert_eq!(
        out.view(),
        RxToneView {
            samples: 7,
            ..RxToneView::default()
        }
    );
    let mut x = tone(0, 30 * BLOCK, 1000);
    x.remove(10 * BLOCK + 3);
    feed(&mut check, &x);
    check.publish(&out, 9, RATE);
    let v = out.view();
    assert_eq!((v.gaps, v.samples, v.locked), (1, 9, true));
    assert!(
        (v.hz - 1000.0).abs() < 1e-6 && (v.amplitude - LEVEL).abs() < 1e-9,
        "{v:?}"
    );
}

#[test]
fn a_nan_sample_is_a_gap() {
    let mut x = tone(0, 40 * BLOCK, 1000);
    x[20 * BLOCK + 5] = f64::NAN;
    let mut check = ToneCheck::default();
    feed(&mut check, &x);
    assert_eq!(check.gaps(), 1);
}
