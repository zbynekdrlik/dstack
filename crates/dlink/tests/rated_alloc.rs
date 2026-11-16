// SPDX-License-Identifier: MIT OR Apache-2.0

//! `Rated::process` allocates nothing (dstack #90, #330): the plain path
//! and the fixed app block, at an exact ratio and at one that does not
//! divide the block, with a discontinuity in between, under
//! `assert_no_alloc` (warn mode: the violation count must stay zero). Its
//! own test binary, because it installs the global allocator. The first
//! test proves the detector sees an allocation.

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use dlink::Rated;
use dstack_audio_io::{Block, Process};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

/// Sends its input's first channel back on every output.
struct Echo;

impl Process for Echo {
    fn process(&mut self, block: &mut Block<'_>) {
        for c in 0..block.outputs() {
            for j in 0..block.frames() {
                let v = if block.inputs() > 0 {
                    block.input(0)[j]
                } else {
                    0.25
                };
                block.output(c)[j] = v;
            }
        }
    }
}

#[test]
fn the_detector_sees_an_allocation() {
    reset_violation_count();
    let v = assert_no_alloc(|| vec![1u8; 4]);
    assert!(violation_count() > 0);
    assert_eq!(v.len(), 4);
}

#[test]
fn rated_s_process_allocates_nothing() {
    for (app, fixed) in [
        (48_000u32, false),
        (48_000, true),
        (44_100, false),
        (44_100, true),
    ] {
        let mut r = if fixed {
            Rated::with_fixed_app_block(Echo, 96_000, app, 128, 2, 2)
        } else {
            Rated::new(Echo, 96_000, app, 128, 2, 2)
        }
        .unwrap();
        let input: Vec<f64> = (0..256).map(|i| (f64::from(i) * 0.01).sin()).collect();
        let mut out = vec![0.0f64; 256];
        reset_violation_count();
        assert_no_alloc(|| {
            for b in 0..3_000u64 {
                if b == 1_500 {
                    r.discontinuity();
                }
                r.process(
                    &mut Block::new(128, &input, &mut out)
                        .at(b * 128)
                        .played_at(b as i64 * 1_333_333),
                );
            }
        });
        assert_eq!(violation_count(), 0, "{app} fixed={fixed}");
    }
}
