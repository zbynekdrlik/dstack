// SPDX-License-Identifier: MIT OR Apache-2.0

//! The from-ASIO side the TX thread runs allocates nothing (dstack #364):
//! [`FromAsio`]'s `process` (the block age's take, the switch's take of a
//! driver open's player, the player's fill) under `assert_no_alloc` (warn
//! mode: the violation count must stay zero), through priming into
//! playing, a reopen's player taken mid-run and a skipped stretch (a
//! resync), beside the driver's side writing the from-ASIO FIFO. Its own
//! test binary, because it installs the global allocator; the first test
//! proves the detector sees an allocation.

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use dstack_asio::bridge::{Bridge, FromAsio, Spec};
use dstack_asio::{Block, Opened, Process, Sample};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

const RATE: u32 = 48_000;
/// The pump's (and the TX fill's) block and the driver's buffer, frames.
const BLOCK: usize = 128;
const BUFFER: usize = 256;

#[test]
fn the_detector_sees_an_allocation() {
    reset_violation_count();
    let v = assert_no_alloc(|| vec![1u8; 4]);
    assert!(violation_count() > 0);
    assert_eq!(v.len(), 4);
}

fn opened() -> Opened {
    Opened {
        rate: f64::from(RATE),
        buffer_frames: BUFFER as u32,
        driver_inputs: 2,
        driver_outputs: 0,
        inputs: vec![0, 1],
        outputs: vec![],
        input_sample: Some(Sample::Int32),
        output_sample: Sample::Int32,
    }
}

/// `switches` driver switches, each followed by its two TX fills from
/// `start` (the fills under `assert_no_alloc`); the next start and the
/// fills whose TX block played the driver's level.
fn run(from: &mut FromAsio, asio: &mut impl Process, start: u64, switches: usize) -> (u64, usize) {
    let input = vec![0.25; 2 * BUFFER];
    let mut out = [0.0; 0];
    let mut tx = vec![0.0; 2 * BLOCK];
    let mut start = start;
    let mut played = 0;
    for _ in 0..switches {
        asio.process(&mut Block::new(BUFFER, &input, &mut out));
        for _ in 0..BUFFER / BLOCK {
            assert_no_alloc(|| from.process(&mut Block::new(BLOCK, &[], &mut tx).at(start)));
            start += BLOCK as u64;
            if tx.iter().all(|&x| (x - 0.25).abs() < 1e-3) {
                played += 1;
            }
        }
    }
    (start, played)
}

#[test]
fn the_tx_thread_s_from_asio_fill_allocates_nothing() {
    let mut bridge = Bridge::new(Spec {
        dante_rate: RATE,
        block_frames: BLOCK as u32,
        to_asio: 0,
        from_asio: 2,
        app_buffer_ns: None,
        to_asio_app_buffer_ns: None,
        from_asio_app_buffer_ns: None,
        converter_taps: None,
    })
    .unwrap();
    let mut from = bridge.pull_from_asio().unwrap();
    let _dante = bridge.dante_side().unwrap();
    let mut asio = bridge.asio_side(&opened()).unwrap();
    reset_violation_count();
    let (start, played) = run(&mut from, &mut asio, 1_000_000, 100);
    assert!(
        played >= 150,
        "it played the driver's input: {played} fills"
    );
    // A reopen: the next fill takes its player and gives the first back.
    let mut asio = bridge.asio_side(&opened()).unwrap();
    let (start, _) = run(&mut from, &mut asio, start, 1);
    assert_eq!(bridge.collect(), 1, "the first open's player came back");
    // A skipped stretch: the player resyncs once.
    let resyncs = bridge.from_asio_stats().resyncs();
    let (_, played) = run(&mut from, &mut asio, start + 48_000, 100);
    assert_eq!(violation_count(), 0, "allocations in the TX thread's fill");
    assert!(played >= 150, "it played on: {played} fills");
    assert_eq!(bridge.from_asio_stats().resyncs(), resyncs + 1, "the skip");
}
