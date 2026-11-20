// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #462: the 44.1 kHz conversion at a FOH-sized instance costs under
//! a quarter of the device block. dasio's release bench shape (dasio#107):
//! a 96 kHz device with 256-frame blocks (2.667 ms), 64 RX and 64 TX
//! channels, an ASIO host at 44.1 kHz with a fixed block
//! ([`Rated::with_fixed_app_block`], 118 frames). Up to v0.6.201 the
//! sinc core took 667-848 µs a block there in dasio's CI (25-32 %; on a
//! loaded dev1 1085-1119 µs mean, a median of 1410 µs at load 64, which
//! this test fails); the fixed-ratio table takes about half (540-565 µs
//! mean, a median of 613 µs at load 67). The assert holds #462's goal, a
//! quarter of the block, not the half.
//!
//! Its own binary, run by CI's Linux test job in release (`cargo test
//! --release -p dlink --test rated_cost`): a cost is only meaningful
//! optimised, and the debug build's `Rated` glue alone takes about 500 µs
//! a block. In a debug build the file holds no test. On a loaded box run
//! the binary under `chrt -f 10`: plain at dev1 load 80 the table read a
//! median of 689 µs (the run queue's), under `chrt -f 10` 587-601 µs.
#![cfg(not(debug_assertions))]

use std::time::Instant;

use dlink::Rated;
use dstack_audio_io::{Block, Process};

const CHANNELS: usize = 64;
const BLOCK: usize = 256;
const DEVICE_RATE: u32 = 96_000;
const APP_RATE: u32 = 44_100;
/// The blocks timed, after [`WARM`] untimed ones.
const BLOCKS: usize = 3_000;
const WARM: usize = 200;

/// The app: each output channel plays its input channel's first sample
/// (no work worth timing).
struct Daw;

impl Process for Daw {
    fn process(&mut self, block: &mut Block<'_>) {
        for c in 0..block.outputs().min(block.inputs()) {
            let v = block.input(c)[0];
            block.output(c).fill(v);
        }
    }
}

/// The median block, under a quarter of the period, at dasio's shape; the
/// mean and the worst are printed (a shared runner's preemption decides
/// those, not the converter).
#[test]
fn the_44_1_khz_conversion_at_64_by_64_takes_under_a_quarter_of_the_block() {
    let mut rated =
        Rated::with_fixed_app_block(Daw, DEVICE_RATE, APP_RATE, BLOCK, CHANNELS, CHANNELS).unwrap();
    assert_eq!(rated.app_block(), Some(118));
    let input: Vec<f64> = (0..CHANNELS * BLOCK)
        .map(|i| (i as f64 * 0.013).sin() * 0.5)
        .collect();
    let mut output = vec![0.0; CHANNELS * BLOCK];
    let period_ns = BLOCK as i64 * 1_000_000_000 / i64::from(DEVICE_RATE);
    let mut us = Vec::with_capacity(BLOCKS);
    for b in 0..WARM + BLOCKS {
        let started = Instant::now();
        rated.process(
            &mut Block::new(BLOCK, &input, &mut output)
                .at((b * BLOCK) as u64)
                .played_at(b as i64 * period_ns),
        );
        if b >= WARM {
            us.push(started.elapsed().as_secs_f64() * 1e6);
        }
    }
    // The app's input comes out of the TX side: the conversion ran.
    assert!(output.iter().any(|v| v.abs() > 0.01), "silent output");
    let mean = us.iter().sum::<f64>() / us.len() as f64;
    us.sort_by(f64::total_cmp);
    let median = us[us.len() / 2];
    let quarter = period_ns as f64 / 4e3;
    eprintln!(
        "{CHANNELS} x {CHANNELS} at {APP_RATE} Hz on {DEVICE_RATE} Hz, block {BLOCK}: median \
         {median:.1} µs, mean {mean:.1} µs, p99 {:.1} µs, worst {:.1} µs (a quarter: {quarter:.1} µs)",
        us[us.len() * 99 / 100],
        us[us.len() - 1]
    );
    assert!(
        median < quarter,
        "median {median:.1} µs a block, over a quarter of the {BLOCK}-frame block ({quarter:.1} µs)"
    );
}
