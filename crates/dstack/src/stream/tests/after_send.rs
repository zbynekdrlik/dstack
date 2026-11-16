// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #346: a block that wakes within W but whose outputs reach the
//! engine ring after the TX thread sent their stamps. The TX thread sent
//! silence (its lane's `holes`); the pump neither re-bootstrapped nor woke
//! late, so only a count of its own shows the gap on the pump's side.

use super::*;

use crate::media::source::Source;

/// Copies input 0 to output 0 and, while `send_during` holds, reads the
/// block's output stamps (`start + D`) from TX 1's engine ring as the TX
/// thread does ([`Source::Engine`]) before the block ends: a block whose
/// processing ran past the TX thread's slack. Keeps what each such read
/// found.
struct SendsDuring {
    tx: Arc<TxShared>,
    offset: u64,
    send_during: bool,
    /// Each read's samples and holes.
    sent: Vec<(Vec<i32>, usize)>,
}

impl Process for SendsDuring {
    fn process(&mut self, block: &mut Block<'_>) {
        let input = block.input(0).to_vec();
        block.output(0).copy_from_slice(&input);
        if !self.send_during {
            return;
        }
        let start = block.start().expect("a media block");
        let mut out = vec![0x5555_5500; block.frames()];
        let rings: &[Ring] = self.tx.rings();
        let fill = Source::Engine.fill(
            start + self.offset,
            RATE,
            &mut out,
            rings,
            self.tx.engine_ring(0),
        );
        self.sent.push((out, fill.holes));
    }
}

/// A pump of [`SendsDuring`] over RX 1 into TX 1 (RX 1 holding the
/// counter), with its first media step at `T` (on time, nothing sent
/// during it); returns it, its counts and the time it asks to wake at.
fn started() -> (Pump<SendsDuring>, Arc<PumpStats>, u64) {
    let r = rig(
        Map {
            rx: vec![0],
            tx: vec![0],
        },
        &[0],
        &[0],
    );
    fill(&r, 0, T - 1_000, 4_000, counter);
    let stats = Arc::new(PumpStats::new(TIMING, RATE));
    let mut pump = Pump::new(
        SendsDuring {
            tx: Arc::clone(&r.tx),
            offset: D,
            send_during: false,
            sent: Vec::new(),
        },
        Map {
            rx: vec![0],
            tx: vec![0],
        },
        TIMING,
        24,
        Arc::clone(&r.rx),
        Arc::clone(&r.tx),
        Arc::clone(&stats),
    );
    let wake = pump.media(T, 0);
    (pump, stats, wake)
}

#[test]
fn a_block_whose_outputs_come_after_the_tx_sent_them_counts_on_the_pump() {
    let (mut pump, stats, wake) = started();
    let ran = stats.view().blocks;
    assert!(ran > 0);
    let first = wake - B - P;
    // In time, on time: the TX thread reads the block's stamps after it.
    let tx = Arc::clone(&pump.processor.tx);
    let read_after = |from: u64| {
        let mut out = vec![0x5555_5500; B as usize];
        let holes = Source::Engine
            .fill(from + D, RATE, &mut out, tx.rings(), tx.engine_ring(0))
            .holes;
        (out, holes)
    };
    let before = read_after(first - B);
    assert_eq!(before.1, 0, "the bootstrap's block was written in time");
    // The next block wakes exactly at its due time (0 behind, well within
    // W), and the TX thread sends its stamps while it runs.
    pump.processor.send_during = true;
    assert_eq!(pump.media(wake, 0), wake + B);
    let (out, holes) = pump.processor.sent.pop().expect("one block ran");
    assert_eq!(
        (out, holes),
        (vec![0; B as usize], B as usize),
        "the TX thread sent the block's stamps as silence"
    );
    // The pump's usual counts show nothing: no late wake, no restart, no
    // skipped block.
    let v = stats.view();
    assert_eq!(v.blocks, ran + 1);
    assert_eq!(
        (v.late, v.rebootstraps, v.missed, stats.wakes().late_wakes()),
        (0, 0, 0, 0),
        "{v:?}"
    );
    // Its own count does: one block written after its stamps were sent.
    assert_eq!(v.writes_after_send, 1, "{v:?}");
    // A block whose stamps the TX thread reads after it ends counts
    // nothing.
    pump.processor.send_during = false;
    assert_eq!(pump.media(wake + B, 0), wake + 2 * B);
    let (out, holes) = read_after(first + B);
    assert_eq!(holes, 0);
    assert_eq!(out[0], counter(first + B));
    assert_eq!(stats.view().writes_after_send, 1);
}
