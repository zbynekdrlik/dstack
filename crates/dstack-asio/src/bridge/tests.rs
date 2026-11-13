// SPDX-License-Identifier: MIT OR Apache-2.0

use std::f64::consts::TAU;

use super::*;
use crate::format::Sample;

const RATE: u32 = 96_000;

fn spec(to_asio: usize, from_asio: usize) -> Spec {
    Spec {
        dante_rate: RATE,
        block_frames: 128,
        to_asio,
        from_asio,
        app_buffer_ns: None,
    }
}

fn opened(rate: f64, buffer_frames: u32) -> Opened {
    Opened {
        rate,
        buffer_frames,
        driver_inputs: 2,
        driver_outputs: 2,
        input_sample: Some(Sample::Int32),
        output_sample: Some(Sample::Int32),
    }
}

/// dstack #247 for an ASIO bridge: the target is the ASIO buffer + one pump
/// block (256 frames at 96 kHz 2.667 ms + 128 frames 1.333 ms = 4 ms), or
/// the configured value verbatim.
#[test]
fn the_fifo_target_is_the_asio_buffer_and_one_pump_block_or_the_set_value() {
    let b = Bridge::new(spec(1, 1));
    let t = b.app_buffer(&opened(96_000.0, 256));
    assert_eq!(
        (t.chunk_ns(), t.block_ns(), t.target_ns()),
        (2_666_667, 1_333_333, 4_000_000)
    );
    let t = b.app_buffer(&opened(48_000.0, 1024));
    assert_eq!(t.target_ns(), 21_333_333 + 1_333_333);
    let mut s = spec(1, 1);
    s.app_buffer_ns = Some(7_000_000);
    let b = Bridge::new(s);
    assert_eq!(b.spec(), s);
    assert_eq!(b.app_buffer(&opened(96_000.0, 256)).target_ns(), 7_000_000);
    s.app_buffer_ns = Some(400_000_000);
    let b = Bridge::new(s);
    assert_eq!(
        b.asio_side(&opened(96_000.0, 256)).err(),
        b.app_buffer(&opened(96_000.0, 256)).problem()
    );
    assert!(b.asio_side(&opened(96_000.0, 256)).is_err());
}

/// A sine at `hz` and `rate`, frame `n`.
fn tone(hz: f64, rate: f64, n: u64) -> f64 {
    0.5 * (TAU * hz * n as f64 / rate).sin()
}

/// A tone's frequency from its rising zero crossings, Hz.
fn frequency(x: &[f64], rate: f64) -> f64 {
    let ups: Vec<f64> = x
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w[0] < 0.0 && w[1] >= 0.0)
        .map(|(i, w)| i as f64 + w[0] / (w[0] - w[1]))
        .collect();
    let (first, last) = (ups[0], ups[ups.len() - 1]);
    (ups.len() - 1) as f64 * rate / (last - first)
}

/// Both directions over simulated time: the pump's 128-frame blocks on the
/// network's clock, the driver's `buffer`-frame switches on its own,
/// `drift` ppm fast. Dante sends 1 kHz to the driver, the driver 2 kHz to
/// Dante.
struct Sim {
    bridge: Bridge,
    dante: DanteSide,
    asio: AsioSide,
    drift: f64,
    buffer: usize,
    dante_frames: u64,
    asio_frames: u64,
    /// The last seconds of each output, for the tone check.
    to_asio_out: Vec<f64>,
    from_asio_out: Vec<f64>,
    /// The FIFO depths, sampled after each step, frames (mean and the time).
    to_depth: (f64, f64),
    from_depth: (f64, f64),
}

impl Sim {
    fn new(drift: f64, buffer: usize, app_buffer_ns: Option<u64>) -> Self {
        let mut bridge = Bridge::new(Spec {
            app_buffer_ns,
            ..spec(1, 1)
        });
        let dante = bridge.dante_side().expect("once");
        assert!(bridge.dante_side().is_none(), "only once");
        let asio = bridge
            .asio_side(&opened(f64::from(RATE), buffer as u32))
            .expect("asio side");
        Sim {
            bridge,
            dante,
            asio,
            drift,
            buffer,
            dante_frames: 0,
            asio_frames: 0,
            to_asio_out: Vec::new(),
            from_asio_out: Vec::new(),
            to_depth: (0.0, 0.0),
            from_depth: (0.0, 0.0),
        }
    }

    fn run(&mut self, seconds: f64, keep_from: f64) {
        let net = f64::from(RATE);
        let card = net * (1.0 + self.drift * 1e-6);
        let end = self.dante_frames as f64 / net + seconds;
        let mut input = vec![0.0; self.buffer.max(128)];
        let mut output = vec![0.0; self.buffer.max(128)];
        loop {
            let t_dante = self.dante_frames as f64 / net;
            let t_asio = self.asio_frames as f64 / card;
            if t_dante.min(t_asio) >= end {
                break;
            }
            if t_dante <= t_asio {
                let n = 128;
                for (i, x) in input[..n].iter_mut().enumerate() {
                    *x = tone(1000.0, net, self.dante_frames + i as u64);
                }
                self.dante
                    .process(&mut Block::new(n, &input[..n], &mut output[..n]));
                if t_dante >= keep_from {
                    self.from_asio_out.extend_from_slice(&output[..n]);
                    let d = self.asio.from_asio.as_ref().unwrap().len() as f64;
                    self.from_depth.0 += d;
                    self.from_depth.1 += 1.0;
                }
                self.dante_frames += n as u64;
            } else {
                let n = self.buffer;
                for (i, x) in input[..n].iter_mut().enumerate() {
                    // The card's own clock makes its tone.
                    *x = tone(2000.0, card, self.asio_frames + i as u64);
                }
                self.asio
                    .process(&mut Block::new(n, &input[..n], &mut output[..n]));
                if t_asio >= keep_from {
                    self.to_asio_out.extend_from_slice(&output[..n]);
                    let d = self.bridge.to_asio_fifo().unwrap().len() as f64;
                    self.to_depth.0 += d;
                    self.to_depth.1 += 1.0;
                }
                self.asio_frames += n as u64;
            }
        }
    }
}

fn mean_ms(d: (f64, f64)) -> f64 {
    d.0 / d.1 / f64::from(RATE) * 1000.0
}

/// A card `drift` ppm off the network, 256-frame buffers at 96 kHz, both
/// directions at the target (`set`, or the default 4 ms) over three
/// simulated minutes: no underrun after the priming, no re-centre, no
/// overflow, the depth near the target, each player's correction at the
/// drift, and each tone arriving at its frequency.
fn both_ways(drift: f64, set: Option<u64>) {
    let target_ms = set.map_or(4.0, |ns| ns as f64 / 1e6);
    let mut s = Sim::new(drift, 256, set);
    s.run(180.0, 160.0);
    let to = s.bridge.to_asio_stats();
    let from = s.bridge.from_asio_stats();
    let (to_ms, from_ms) = (mean_ms(s.to_depth), mean_ms(s.from_depth));
    println!(
        "drift {drift} ppm: to ASIO depth {to_ms:.2} ms ppm {:.2} underruns {}; \
         from ASIO depth {from_ms:.2} ms ppm {:.2} underruns {}",
        to.ppm(),
        to.underruns(),
        from.ppm(),
        from.underruns()
    );
    for (what, st) in [("to", &to), ("from", &from)] {
        assert_eq!(st.underruns(), 0, "{what}: no underrun after priming");
        assert_eq!(st.recentres(), 0, "{what}");
        assert!(!st.priming(), "{what}: playing");
        assert!(
            (st.target_ms() - target_ms).abs() < 1e-6,
            "{what}: target {}",
            st.target_ms()
        );
    }
    assert_eq!(s.bridge.to_asio_fifo().unwrap().overflows(), 0);
    assert_eq!(s.asio.from_asio.as_ref().unwrap().overflows(), 0);
    // To ASIO the driver's clock reads the network's audio: a card fast by
    // +d takes more frames (+d ppm); from ASIO the network takes the card's
    // audio, its producer fast by +d: -d ppm.
    assert!((to.ppm() - drift).abs() < 3.0, "to: {} ppm", to.ppm());
    assert!((from.ppm() + drift).abs() < 3.0, "from: {} ppm", from.ppm());
    assert!((to_ms - target_ms).abs() < 2.5, "to: depth {to_ms} ms");
    assert!(
        (from_ms - target_ms).abs() < 2.5,
        "from: depth {from_ms} ms"
    );
    let card = f64::from(RATE) * (1.0 + drift * 1e-6);
    let f_to = frequency(&s.to_asio_out, card);
    let f_from = frequency(&s.from_asio_out, f64::from(RATE));
    assert!((f_to - 1000.0).abs() < 0.05, "to ASIO: {f_to} Hz");
    assert!((f_from - 2000.0).abs() < 0.1, "from ASIO: {f_from} Hz");
}

/// At the default 4 ms a card ±50 ppm off holds both ways (a crystal's
/// usual spread). The correction moves only 5 ppm a second (inaudibly) and
/// the rate fit locks after 60 s, so at ±100 ppm the default drops out
/// twice in the first seconds, in the direction whose consumer is the
/// faster clock (measured, dstack #243); a set 5 ms holds ±100 ppm.
#[test]
fn a_card_50_ppm_fast_runs_both_ways_at_the_default_target() {
    both_ways(50.0, None);
}

#[test]
fn a_card_50_ppm_slow_runs_both_ways_at_the_default_target() {
    both_ways(-50.0, None);
}

#[test]
fn a_card_100_ppm_fast_runs_both_ways_at_a_5_ms_target() {
    both_ways(100.0, Some(5_000_000));
}

#[test]
fn a_card_100_ppm_slow_runs_both_ways_at_a_5_ms_target() {
    both_ways(-100.0, Some(5_000_000));
}

/// A reopen makes a new driver side; the pump's side takes the new
/// from-ASIO player at its next block and gives the old one back to be
/// freed.
#[test]
fn a_reopen_hands_the_pump_a_new_player_and_frees_the_old() {
    let mut bridge = Bridge::new(spec(2, 2));
    let mut dante = bridge.dante_side().unwrap();
    let input = vec![0.0; 2 * 128];
    let mut output = vec![1.0; 2 * 128];
    dante.process(&mut Block::new(128, &input, &mut output));
    assert!(output.iter().all(|&x| x == 0.0), "no driver yet: silence");
    assert!(dante.player.is_none());
    let _first = bridge.asio_side(&opened(96_000.0, 256)).unwrap();
    dante.process(&mut Block::new(128, &input, &mut output));
    assert!(dante.player.is_some());
    assert_eq!(bridge.collect(), 0);
    let _second = bridge.asio_side(&opened(48_000.0, 512)).unwrap();
    dante.process(&mut Block::new(128, &input, &mut output));
    assert_eq!(bridge.collect(), 1, "the first player came back");
    assert_eq!(bridge.to_asio_fifo().unwrap().channels(), 2);
}

/// The handover is bounded: a pump that never runs takes no player, and a
/// fifth open in a row is refused rather than queued without end.
#[test]
fn opens_with_no_pump_running_are_refused_after_the_handover_fills() {
    let bridge = Bridge::new(spec(1, 1));
    for _ in 0..HANDOVERS {
        assert!(bridge.asio_side(&opened(96_000.0, 256)).is_ok());
    }
    assert_eq!(
        bridge.asio_side(&opened(96_000.0, 256)).err().as_deref(),
        Some("the pump has not taken the last players (is the Dante device running?)")
    );
    let mut bridge = Bridge::new(spec(1, 1));
    drop(bridge.dante_side());
    assert_eq!(
        bridge.asio_side(&opened(96_000.0, 256)).err().as_deref(),
        Some("the pump's side is gone")
    );
}

/// A one-way bridge: no FIFO, no player and silence on the unused side.
#[test]
fn a_one_way_bridge_has_nothing_the_other_way() {
    let mut bridge = Bridge::new(spec(0, 1));
    assert!(bridge.to_asio_fifo().is_none());
    let mut dante = bridge.dante_side().unwrap();
    let mut asio = bridge.asio_side(&opened(96_000.0, 64)).unwrap();
    assert!(asio.player.is_none());
    let input = vec![0.25; 64];
    let mut output = vec![1.0; 64];
    asio.process(&mut Block::new(64, &input, &mut output));
    assert!(output.iter().all(|&x| x == 0.0), "nothing to the driver");
    assert_eq!(
        asio.from_asio.as_ref().unwrap().len(),
        64,
        "its input went in"
    );
    let mut out = vec![1.0; 128];
    dante.process(&mut Block::new(128, &[], &mut out));
    assert!(out.iter().all(|&x| x == 0.0), "still priming: silence");
    let bridge = Bridge::new(spec(1, 0));
    let asio = bridge.asio_side(&opened(96_000.0, 64)).unwrap();
    assert!(asio.from_asio.is_none() && asio.player.is_some());
}
