// SPDX-License-Identifier: MIT OR Apache-2.0

//! The converter's timing test, in its own binary (dstack #462). Under a
//! coverage build the lib's other unit tests convert through the same engine
//! on other threads and share its counters' cache lines
//! (`.claude/rules/resample.md`): main CI 45657637244 (v0.6.217) read
//! 85.266 us at the best of 20 batches beside them, with every batch at
//! 85-170 us. Cargo runs test binaries one at a time, so here nothing else
//! converts meanwhile. The mutation gate runs this binary too
//! (`.cargo/mutants.toml`).

use dstack_resample::{Converter, Fixed};

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
    // The best of 20 batches of 100 calls: the converter's own cost. A
    // whole-run mean also holds what other test threads take: under
    // coverage they share its counters' cache lines (resample.md), and
    // v0.6.202's main CI read 80.486 µs that way. A converter that is slow
    // itself is slow in every batch.
    let batches: Vec<std::time::Duration> = (0..20)
        .map(|_| {
            let t = std::time::Instant::now();
            for _ in 0..100 {
                c.process(&input, 16, &mut out, room).unwrap();
            }
            t.elapsed() / 100
        })
        .collect();
    let each = *batches.iter().min().unwrap();
    assert!(
        each < std::time::Duration::from_micros(80),
        "{each:?} per 16-frame block at best of 20 batches: {batches:?}"
    );
}
