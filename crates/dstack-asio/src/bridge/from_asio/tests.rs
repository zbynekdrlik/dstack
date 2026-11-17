// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::atomic::{AtomicI64, Ordering};

use dlink::bridge::Clock;

use super::*;
use crate::bridge::{Bridge, Spec};
use crate::device::Opened;
use crate::format::Sample;

const RATE: u32 = 96_000;

fn spec(to_asio: usize, from_asio: usize) -> Spec {
    Spec {
        dante_rate: RATE,
        block_frames: 128,
        to_asio,
        from_asio,
        app_buffer_ns: None,
        to_asio_app_buffer_ns: None,
        from_asio_app_buffer_ns: None,
        converter_taps: None,
    }
}

fn opened(outputs: usize, inputs: usize) -> Opened {
    Opened {
        rate: f64::from(RATE),
        buffer_frames: 256,
        driver_inputs: 2,
        driver_outputs: 2,
        inputs: (0..inputs as u32).collect(),
        outputs: (0..outputs as u32).collect(),
        input_sample: Some(Sample::Int32),
        output_sample: Sample::Int32,
    }
}

/// A clock the test sets, ns.
fn fake() -> (Arc<AtomicI64>, Clock) {
    let t = Arc::new(AtomicI64::new(0));
    let read = Arc::clone(&t);
    (t, Arc::new(move || read.load(Ordering::Relaxed)))
}

/// dstack #364: pulled, the TX thread's fill plays the from-ASIO player
/// (each open's, through the bridge's switch, the one it replaces given
/// back to be freed) on the bridge's from-ASIO outputs, and the pump's
/// side plays none of it.
#[test]
fn the_pull_plays_each_open_s_player_and_the_pump_plays_none() {
    let mut bridge = Bridge::new(spec(1, 2)).unwrap();
    let mut from = bridge.pull_from_asio().expect("the pull");
    assert_eq!(TxProcess::outputs(&from), 2);
    let text = format!("{from:?}");
    assert!(
        text.starts_with("FromAsio") && text.contains("outputs: 2"),
        "{text}"
    );
    let mut dante = bridge.dante_side().unwrap();
    let mut asio = bridge.asio_side(&opened(1, 2)).unwrap();
    let input = vec![0.5; 2 * 256];
    let mut out = vec![0.0; 256];
    let mut tx = vec![1.0; 2 * 128];
    let mut pump = vec![1.0; 128];
    let mut start = 1_000_000;
    for _ in 0..40 {
        asio.process(&mut Block::new(256, &input, &mut out));
        for _ in 0..2 {
            from.process(&mut Block::new(128, &[], &mut tx).at(start));
            dante.process(&mut Block::new(128, &[0.25; 128], &mut pump).at(start));
            start += 128;
        }
    }
    assert!(
        tx.iter().all(|&x| (x - 0.5).abs() < 1e-3),
        "the driver's input on both TX channels: {:?}",
        &tx[..4]
    );
    assert!(pump.iter().all(|&x| x == 0.0), "the pump plays none of it");
    assert_eq!(bridge.collect(), 1, "the idle player came back");
    assert_eq!(bridge.from_asio_stats().underruns(), 0);
    assert_eq!(from.player().stats().underruns(), 0);
    // The next open's player through the pull's switch.
    let _second = bridge.asio_side(&opened(1, 2)).unwrap();
    from.process(&mut Block::new(128, &[], &mut tx).at(start));
    assert_eq!(bridge.collect(), 1, "the first open's player came back");
}

/// An open the TX thread has not taken refuses the next, as the pump's
/// side did (dlink's switch).
#[test]
fn an_open_the_pull_has_not_taken_is_refused() {
    let mut bridge = Bridge::new(spec(0, 1)).unwrap();
    let mut from = bridge.pull_from_asio().unwrap();
    let _dante = bridge.dante_side().unwrap();
    assert!(bridge.asio_side(&opened(0, 1)).is_ok());
    assert_eq!(
        bridge.asio_side(&opened(0, 1)).err().as_deref(),
        Some(dlink::bridge::switch::NOT_TAKEN)
    );
    let mut tx = vec![0.0; 128];
    from.process(&mut Block::new(128, &[], &mut tx));
    assert!(bridge.asio_side(&opened(0, 1)).is_ok());
}

/// The pull is taken once, before the pump's side and the driver's first
/// open, from a bridge with channels from ASIO.
#[test]
fn the_pull_is_refused_when_late_twice_or_without_channels() {
    let mut b = Bridge::new(spec(1, 0)).unwrap();
    assert_eq!(
        b.pull_from_asio().err().as_deref(),
        Some("the bridge has no channel from ASIO to pull")
    );
    let mut b = Bridge::new(spec(1, 1)).unwrap();
    let _dante = b.dante_side();
    assert_eq!(
        b.pull_from_asio().err().as_deref(),
        Some("the pump's side was taken already: take the from-ASIO pull before it")
    );
    let mut b = Bridge::new(spec(1, 1)).unwrap();
    let _first = b.pull_from_asio().unwrap();
    assert_eq!(
        b.pull_from_asio().err().as_deref(),
        Some("the bridge pulls from ASIO already")
    );
    let mut b = Bridge::new(spec(1, 1)).unwrap();
    let _asio = b.asio_side(&opened(1, 1)).unwrap();
    assert_eq!(
        b.pull_from_asio().err().as_deref(),
        Some("the driver opened already: take the from-ASIO pull before its first open")
    );
    // A bridge that was refused late still plays on the pump's path: its
    // pump side takes the open's player (and gives the idle one back).
    let mut dante = b.dante_side().expect("the pump's side");
    let mut out = vec![1.0; 128];
    dante.process(&mut Block::new(128, &[0.0; 128], &mut out));
    assert_eq!(b.collect(), 1, "the pump took the open's player");
}

/// dstack #325's modelled block age: the TX thread's fill takes the
/// driver's hand-over (its call period + the wait until the fill).
#[test]
fn the_fill_takes_the_driver_s_hand_over_for_the_block_age() {
    let (t, clock) = fake();
    let mut bridge = Bridge::with_clock(spec(0, 1), clock).unwrap();
    let mut from = bridge.pull_from_asio().unwrap();
    let mut asio = bridge.asio_side(&opened(0, 1)).unwrap();
    let age = bridge.from_asio_age();
    assert_eq!(age.report(), None);
    t.store(1_000_000, Ordering::Relaxed);
    let mut out = [0.0; 0];
    asio.process(&mut Block::new(256, &[0.5; 256], &mut out));
    t.store(1_500_000, Ordering::Relaxed);
    let mut tx = vec![0.0; 128];
    from.process(&mut Block::new(128, &[], &mut tx));
    let report = age.report().expect("a block taken");
    // 256 frames at 96 kHz (2.667 ms) + 0.5 ms.
    assert_eq!(report.block_age_us, Some(3_166));
    assert_eq!(report.block_age_max_us, Some(3_166));
}
