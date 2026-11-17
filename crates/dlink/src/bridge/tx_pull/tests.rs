// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

use std::sync::Arc;

use crate::bridge::{AppBuffer, Fifo, Stats, fifo_for};

const RATE: u32 = 48_000;
const BLOCK: usize = 128;

/// A 10 ms writer and one block at [`RATE`].
fn buffer() -> AppBuffer {
    AppBuffer::new(10_000_000, crate::bridge::chunk_ns(BLOCK as u64, RATE))
}

fn player(fifo: &Arc<Fifo>, stats: &Arc<Stats>) -> Player {
    let b = buffer();
    Player::new(
        Arc::clone(fifo),
        RATE,
        RATE,
        b,
        b.chunk_ns(),
        Arc::clone(stats),
    )
    .unwrap()
}

/// Pushes `frames` frames of `level` (channel `c` at `level * (c + 1)`)
/// into `fifo`.
fn push(fifo: &Fifo, frames: usize, level: f64) {
    let channels = fifo.channels();
    fifo.push_with(frames, |_, c| level * (c + 1) as f64);
    let _ = channels;
}

/// Runs `blocks` blocks of [`BLOCK`] from start `start` on, a 10 ms push
/// every 480 frames played; returns the last block's outputs and the next
/// start.
fn run(
    pull: &mut TxPull,
    fifo: &Fifo,
    mut start: u64,
    blocks: usize,
    level: f64,
) -> (Vec<f64>, u64) {
    let mut out = vec![0.0; BLOCK * pull.outputs()];
    let mut pushed = 0usize;
    for b in 0..blocks {
        while pushed <= (b + 1) * BLOCK + 960 {
            push(fifo, 480, level);
            pushed += 480;
        }
        pull.process(&mut Block::new(BLOCK, &[], &mut out).at(start));
        start += BLOCK as u64;
    }
    (out, start)
}

#[test]
fn the_pull_plays_the_app_s_fifo_on_its_channels() {
    let fifo = Arc::new(fifo_for(2, RATE));
    let stats: Arc<Stats> = Arc::default();
    let mut pull = TxPull::new(player(&fifo, &stats), 2);
    assert_eq!(pull.outputs(), 2);
    let (out, _) = run(&mut pull, &fifo, 1_000_000, 400, 0.1);
    assert!((out[BLOCK - 1] - 0.1).abs() < 1e-4, "{}", out[BLOCK - 1]);
    assert!(
        (out[2 * BLOCK - 1] - 0.2).abs() < 1e-4,
        "{}",
        out[2 * BLOCK - 1]
    );
    assert_eq!(stats.resyncs(), 0);
}

#[test]
fn a_block_that_does_not_follow_on_resyncs_once_and_pieces_never() {
    let fifo = Arc::new(fifo_for(1, RATE));
    let stats: Arc<Stats> = Arc::default();
    let mut pull = TxPull::new(player(&fifo, &stats), 1);
    let (_, next) = run(&mut pull, &fifo, 1_000_000, 100, 0.1);
    // A block longer than a chunk: filled in pieces, each at its own start.
    let long = 2 * CHUNK + 100;
    push(&fifo, long, 0.1);
    let mut out = vec![9.0; long];
    let played = stats.played();
    pull.process(&mut Block::new(long, &[], &mut out).at(next));
    assert_eq!(stats.played() - played, long as u64, "exactly the block");
    assert!(out.iter().all(|&x| x != 9.0));
    assert_eq!(stats.resyncs(), 0, "pieces of one block");
    // A skipped stretch: one resync.
    run(
        &mut pull,
        &fifo,
        next + long as u64 + 8 * BLOCK as u64,
        1,
        0.1,
    );
    assert_eq!(stats.resyncs(), 1, "the skip");
}

#[test]
fn a_switched_player_takes_over_at_the_next_block_and_the_old_comes_back() {
    let fifo = Arc::new(fifo_for(1, RATE));
    let stats: Arc<Stats> = Arc::default();
    let (mut pull, mut switch) = TxPull::switched(player(&fifo, &stats), 1);
    let (_, next) = run(&mut pull, &fifo, 1_000_000, 50, 0.1);
    let other = Arc::new(fifo_for(1, RATE));
    assert_eq!(switch.send(player(&other, &stats)), Ok(()));
    assert_eq!(switch.collect(), 0, "not taken yet");
    let (out, _) = run(&mut pull, &other, next, 400, 0.3);
    assert_eq!(switch.collect(), 1, "the replaced player, on this thread");
    assert!((out[BLOCK - 1] - 0.3).abs() < 1e-4, "{}", out[BLOCK - 1]);
}

#[test]
fn a_block_with_fewer_channels_than_the_pull_fills_what_it_has() {
    let fifo = Arc::new(fifo_for(2, RATE));
    let stats: Arc<Stats> = Arc::default();
    let mut pull = TxPull::new(player(&fifo, &stats), 2);
    let (_, next) = run(&mut pull, &fifo, 1_000_000, 400, 0.1);
    let mut one = vec![9.0; BLOCK];
    push(&fifo, BLOCK, 0.1);
    pull.process(&mut Block::new(BLOCK, &[], &mut one).at(next));
    assert!((one[BLOCK - 1] - 0.1).abs() < 1e-4, "{}", one[BLOCK - 1]);
}

#[test]
fn idle_writes_nothing() {
    let mut out = vec![9.0; 4];
    Idle.process(&mut Block::new(2, &[], &mut out));
    assert_eq!(out, vec![9.0; 4]);
}
