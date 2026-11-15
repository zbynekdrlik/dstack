// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #317 (dasio, field 15.11.2026): a pump rebootstrap that skipped
//! blocks started minutes of to-ASIO underruns, one on every ASIO callback.
//! The field's shape: Dante at 96 kHz in 32-frame pump blocks, a USB ASIO
//! card at 44.1 kHz with 512-frame buffers, and one rebootstrap that skipped
//! 38 pump blocks (12.7 ms, about the to-ASIO target).

use super::*;

/// The pump's block in the field, frames at [`RATE`].
const PUMP_BLOCK: usize = 32;
/// The ASIO card's rate and buffer in the field.
const CARD_RATE: f64 = 44_100.0;
const CARD_BUFFER: usize = 512;

/// The to-ASIO direction alone over simulated time: the pump writes each
/// 32-frame block (with its media start) into the to-ASIO FIFO on the
/// network's clock, the driver's 512-frame switches pull it on the card's,
/// `drift` ppm fast. `skip_at_s`: one rebootstrap there that skips
/// `skipped` pump blocks (the pump writes nothing for them).
struct PumpSim {
    bridge: Bridge,
    dante: DanteSide,
    asio: AsioSide,
    card: f64,
    dante_frames: u64,
    asio_frames: u64,
}

impl PumpSim {
    fn new(drift: f64) -> Self {
        let mut bridge = bridge(Spec {
            block_frames: PUMP_BLOCK as u32,
            ..spec(1, 0)
        });
        let dante = bridge.dante_side().expect("once");
        let asio = bridge
            .asio_side(&opened_with(CARD_RATE, CARD_BUFFER as u32, 1, 0))
            .expect("asio side");
        PumpSim {
            bridge,
            dante,
            asio,
            card: CARD_RATE * (1.0 + drift * 1e-6),
            dante_frames: 0,
            asio_frames: 0,
        }
    }

    /// Runs until `until_s` of the network's clock; at `skip_at_s` (once)
    /// the pump skips `skipped` blocks.
    fn run(&mut self, until_s: f64, skip: Option<(f64, u64)>) {
        let net = f64::from(RATE);
        let mut skip = skip;
        let input = vec![0.25; CARD_BUFFER];
        let mut output = vec![0.0; CARD_BUFFER];
        loop {
            let t_dante = self.dante_frames as f64 / net;
            let t_asio = self.asio_frames as f64 / self.card;
            if t_dante.min(t_asio) >= until_s {
                break;
            }
            if t_dante <= t_asio {
                if let Some((at, blocks)) = skip
                    && t_dante >= at
                {
                    self.dante_frames += blocks * PUMP_BLOCK as u64;
                    skip = None;
                    continue;
                }
                let n = PUMP_BLOCK;
                let mut none = [0.0; 0];
                self.dante
                    .process(&mut Block::new(n, &input[..n], &mut none).at(self.dante_frames));
                self.dante_frames += n as u64;
            } else {
                let n = CARD_BUFFER;
                self.asio.process(&mut Block::new(n, &[], &mut output[..n]));
                self.asio_frames += n as u64;
            }
        }
    }
}

/// One rebootstrap that skips 38 pump blocks (12.7 ms) costs the to-ASIO
/// side a re-prime or two, never a storm: before #317 the player took the
/// ASIO buffer for its producer's chunk, re-primed under one callback's
/// take after the skip and ran dry on every callback after it (dasio's
/// field: 864 underruns every 10 s for minutes).
fn one_skip_is_one_reprime(drift: f64) {
    let mut s = PumpSim::new(drift);
    s.run(20.0, None);
    let stats = s.bridge.to_asio_stats();
    let before = stats.underruns();
    s.run(50.0, Some((20.0, 38)));
    let after = stats.underruns() - before;
    println!(
        "drift {drift} ppm: {before} to-ASIO underruns before the skip, {after} after; \
         ppm {:.2}",
        stats.ppm()
    );
    assert_eq!(before, 0, "none before the skip");
    assert!(after <= 2, "{after} to-ASIO underruns after one skip");
    assert!(!stats.priming(), "playing again");
}

#[test]
fn a_pump_skip_of_38_blocks_is_one_reprime_to_asio_card_fast() {
    one_skip_is_one_reprime(20.0);
}

#[test]
fn a_pump_skip_of_38_blocks_is_one_reprime_to_asio_card_slow() {
    one_skip_is_one_reprime(-20.0);
}

/// The to-ASIO player's priming counts the driver's whole buffer, though
/// the side fills it in pieces: a prime half a buffer low (the piece's
/// width) left the controller to drain the offset, +95 ppm 20 s in at no
/// drift at all (review of dstack #317). At no drift the correction stays
/// within a few ppm of 0 through the first 30 s.
#[test]
fn the_to_asio_player_primes_for_the_whole_asio_buffer() {
    let mut s = PumpSim::new(0.0);
    let stats = s.bridge.to_asio_stats();
    let mut worst: f64 = 0.0;
    for second in 1..=30 {
        s.run(f64::from(second), None);
        worst = worst.max(stats.ppm().abs());
    }
    println!("no drift: worst |ppm| {worst:.2} in the first 30 s");
    assert_eq!(stats.underruns(), 0);
    assert!(
        worst < 5.0,
        "the correction reached {worst} ppm at no drift"
    );
}
