// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

const ALL: [Sample; 8] = [
    Sample::Int16,
    Sample::Int24,
    Sample::Int32,
    Sample::Float32,
    Sample::Int32In16,
    Sample::Int32In18,
    Sample::Int32In20,
    Sample::Int32In24,
];

#[test]
fn the_supported_codes_and_the_refused_ones() {
    let table = [
        (16, Sample::Int16, "Int16LSB", 2),
        (17, Sample::Int24, "Int24LSB", 3),
        (18, Sample::Int32, "Int32LSB", 4),
        (19, Sample::Float32, "Float32LSB", 4),
        (24, Sample::Int32In16, "Int32LSB16", 4),
        (25, Sample::Int32In18, "Int32LSB18", 4),
        (26, Sample::Int32In20, "Int32LSB20", 4),
        (27, Sample::Int32In24, "Int32LSB24", 4),
    ];
    for (code, sample, name, bytes) in table {
        assert_eq!(Sample::from_code(code), Ok(sample));
        assert_eq!((sample.name(), sample.bytes()), (name, bytes));
    }
    // Float64 (20), the MSB types (0-11), DSD (32, 33, 40) and unknown codes.
    for code in [0, 2, 8, 11, 15, 20, 21, 23, 28, 32, 33, 40, -1] {
        assert_eq!(Sample::from_code(code), Err(code));
    }
    assert_eq!(
        unsupported_sample_text(20),
        "the driver's sample type 20 is not supported (Int16/24/32LSB, Float32LSB, Int32LSB16-24)"
    );
}

fn one(sample: Sample, x: f64) -> Vec<u8> {
    let mut dst = vec![0xAA; sample.bytes()];
    sample.encode(&[x], &mut dst);
    dst
}

#[test]
fn each_type_encodes_exactly() {
    assert_eq!(one(Sample::Int16, 1.0), 32_767i16.to_le_bytes());
    assert_eq!(one(Sample::Int16, -1.0), (-32_767i16).to_le_bytes());
    assert_eq!(one(Sample::Int16, 0.5), 16_384i16.to_le_bytes());
    assert_eq!(one(Sample::Int24, 0.5), [0x00, 0x00, 0x40], "4_194_304");
    assert_eq!(one(Sample::Int24, -1.0), [0x01, 0x00, 0x80], "-8_388_607");
    assert_eq!(one(Sample::Int32, 1.0), 2_147_483_647i32.to_le_bytes());
    assert_eq!(one(Sample::Int32, -0.5), (-1_073_741_824i32).to_le_bytes());
    assert_eq!(one(Sample::Int32In24, 0.5), 4_194_304i32.to_le_bytes());
    assert_eq!(one(Sample::Int32In24, -1.0), (-8_388_607i32).to_le_bytes());
    assert_eq!(one(Sample::Int32In20, 1.0), 524_287i32.to_le_bytes());
    assert_eq!(one(Sample::Int32In18, 1.0), 131_071i32.to_le_bytes());
    assert_eq!(one(Sample::Int32In16, -1.0), (-32_767i32).to_le_bytes());
    assert_eq!(one(Sample::Float32, 0.25), 0.25f32.to_le_bytes());
    assert_eq!(one(Sample::Float32, 1.5), 1.0f32.to_le_bytes(), "clamped");
    assert_eq!(
        one(Sample::Int16, -3.0),
        (-32_767i16).to_le_bytes(),
        "clamped"
    );
    assert_eq!(one(Sample::Int24, f64::NAN), [0, 0, 0], "NaN is silence");
    assert_eq!(one(Sample::Float32, f64::INFINITY), [0, 0, 0, 0]);
}

/// SongPlayer's encode of an `f32` (songplayer `asio_format.rs`,
/// `AsioSample::encode` with `vban_packet::clean_f32`), written out as it
/// is there: the reference an app moving onto this crate must match.
fn songplayer_encode(sample: Sample, x: f32, dst: &mut [u8]) {
    let x = if x.is_finite() {
        x.clamp(-1.0, 1.0)
    } else {
        0.0
    };
    match (sample, sample.full_scale()) {
        (_, None) => dst.copy_from_slice(&x.to_le_bytes()),
        (Sample::Int16, Some(full)) => {
            dst.copy_from_slice(&((f64::from(x) * full).round() as i16).to_le_bytes());
        }
        (Sample::Int24, Some(full)) => {
            let b = ((f64::from(x) * full).round() as i32).to_le_bytes();
            dst.copy_from_slice(&b[..3]);
        }
        (_, Some(full)) => {
            dst.copy_from_slice(&((f64::from(x) * full).round() as i32).to_le_bytes());
        }
    }
}

/// dstack #243: an `f32` widened to `f64` writes SongPlayer's bytes, for
/// every type, over full-scale edges, rounding halves, over-range values
/// and non-finite ones (a pseudo-random sweep too).
#[test]
fn an_f32_widened_to_f64_writes_songplayers_bytes() {
    let mut xs: Vec<f32> = vec![
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.5,
        -0.5,
        1.000_001,
        -7.0,
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::MIN_POSITIVE,
        0.5 / 32_767.0,
        1.5 / 8_388_607.0,
        -2.5 / 32_767.0,
    ];
    let mut seed = 0x9E37_79B9_u32;
    for _ in 0..20_000 {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        xs.push((seed as f32 / u32::MAX as f32) * 2.4 - 1.2);
    }
    for sample in ALL {
        for &x in &xs {
            let mut want = vec![0u8; sample.bytes()];
            songplayer_encode(sample, x, &mut want);
            let mut got = vec![0xAAu8; sample.bytes()];
            sample.encode(&[f64::from(x)], &mut got);
            assert_eq!(got, want, "{sample:?} {x}");
        }
    }
}

#[test]
fn a_short_source_leaves_silence_and_a_long_one_only_what_fits() {
    let mut dst = vec![0xAAu8; 3 * 4];
    Sample::Int24.encode(&[0.5, 0.5, 0.5], &mut dst);
    assert_eq!(&dst[..9], &[0x00, 0x00, 0x40].repeat(3)[..]);
    assert_eq!(&dst[9..], &[0, 0, 0], "the missing frame is silence");
    let mut empty = vec![0xAAu8; 8];
    Sample::Float32.encode(&[], &mut empty);
    assert!(empty.iter().all(|&b| b == 0));
    let mut short = vec![0xAAu8; 2 * 3];
    Sample::Int24.encode(&[0.5, 0.5, 0.5], &mut short);
    assert_eq!(&short[..], &[0x00, 0x00, 0x40].repeat(2)[..]);
}

#[test]
fn each_type_reads_back_what_it_wrote() {
    for sample in ALL {
        let step = sample.full_scale().map_or(1e-7, |full| 1.0 / full);
        for x in [0.5, -0.5, 0.999, -1.0, 0.0, 0.123_456, -0.000_1] {
            let mut bytes = vec![0u8; sample.bytes()];
            sample.encode(&[x], &mut bytes);
            let mut back = [9.0];
            sample.decode(&bytes, &mut back);
            assert!(
                (back[0] - x).abs() <= step,
                "{sample:?}: {x} read back as {back:?}"
            );
        }
    }
}

#[test]
fn each_type_decodes_exactly() {
    let d = |s: Sample, b: &[u8]| s.decode_one(b);
    assert_eq!(
        d(Sample::Int16, &16_384i16.to_le_bytes()),
        16_384.0 / 32_767.0
    );
    assert_eq!(d(Sample::Int16, &(-32_767i16).to_le_bytes()), -1.0);
    assert_eq!(
        d(Sample::Int24, &[0x00, 0x00, 0x40]),
        4_194_304.0 / 8_388_607.0
    );
    assert_eq!(d(Sample::Int24, &[0x01, 0x00, 0x80]), -1.0);
    assert_eq!(
        d(Sample::Int24, &[0xFF, 0xFF, 0xFF]),
        -1.0 / 8_388_607.0,
        "sign-extended"
    );
    assert_eq!(d(Sample::Int32, &i32::MAX.to_le_bytes()), 1.0);
    assert_eq!(
        d(Sample::Int32In24, &(-4_194_304i32).to_le_bytes()),
        -4_194_304.0 / 8_388_607.0
    );
    assert_eq!(d(Sample::Int32In20, &524_287i32.to_le_bytes()), 1.0);
    assert_eq!(
        d(Sample::Int32In18, &65_536i32.to_le_bytes()),
        65_536.0 / 131_071.0
    );
    assert_eq!(d(Sample::Int32In16, &(-32_767i32).to_le_bytes()), -1.0);
    assert_eq!(
        d(Sample::Float32, &1.5f32.to_le_bytes()),
        1.5,
        "a float is never clamped"
    );
    assert_eq!(d(Sample::Float32, &f32::NAN.to_le_bytes()), 0.0);
    assert_eq!(d(Sample::Float32, &f32::NEG_INFINITY.to_le_bytes()), 0.0);
}

#[test]
fn a_short_input_reads_as_silence_after_its_samples() {
    let mut dst = [7.0; 3];
    Sample::Int16.decode(&32_767i16.to_le_bytes(), &mut dst);
    assert_eq!(dst, [1.0, 0.0, 0.0]);
    let mut none = [7.0; 2];
    Sample::Int32.decode(&[], &mut none);
    assert_eq!(none, [0.0, 0.0]);
}

#[test]
fn clean_clamps_and_silences() {
    assert_eq!(clean(0.25), 0.25);
    assert_eq!(clean(2.0), 1.0);
    assert_eq!(clean(-2.0), -1.0);
    assert_eq!(clean(f64::NAN), 0.0);
    assert_eq!(clean(f64::NEG_INFINITY), 0.0);
}
