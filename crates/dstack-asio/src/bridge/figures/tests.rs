// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::bridge::Spec;
use crate::device::Opened;
use crate::format::Sample;

#[test]
fn a_ms_figure_is_whole_us_rounded_and_saturated() {
    assert_eq!(us_of_ms(4.0), 4000);
    assert_eq!(us_of_ms(1.333_333), 1333);
    assert_eq!(us_of_ms(0.0006), 1);
    assert_eq!(us_of_ms(0.0004), 0);
    assert_eq!(us_of_ms(-1.0), 0);
    assert_eq!(us_of_ms(f64::NAN), 0);
    assert_eq!(us_of_ms(4_294_967.295), u32::MAX);
    assert_eq!(us_of_ms(1e12), u32::MAX);
}

#[test]
fn a_ns_figure_is_whole_us_rounded_and_saturated() {
    assert_eq!(us_of_ns(0), 0);
    assert_eq!(us_of_ns(499), 0);
    assert_eq!(us_of_ns(500), 1);
    assert_eq!(us_of_ns(1_333_333), 1333);
    assert_eq!(us_of_ns(1_333_500), 1334);
    assert_eq!(us_of_ns(4_294_967_294_499), u32::MAX - 1);
    assert_eq!(us_of_ns(4_294_967_295_499), u32::MAX);
    assert_eq!(us_of_ns(4_294_967_295_500), u32::MAX, "saturated");
    assert_eq!(us_of_ns(u64::MAX), u32::MAX);
}

/// The bridge's own figures, read live: a bridge of 256 at 96 kHz (one
/// channel each way, 48 taps, to-ASIO's target set to 3 ms) opened, its
/// targets and converter delays from its players, the lateness from the
/// runner's status as it is at each read.
#[test]
fn a_bridge_publishes_its_targets_delays_and_the_runner_s_lateness() {
    let mut bridge = Bridge::new(Spec {
        dante_rate: 96_000,
        block_frames: 128,
        to_asio: 1,
        from_asio: 1,
        app_buffer_ns: None,
        to_asio_app_buffer_ns: Some(3_000_000),
        from_asio_app_buffer_ns: None,
        converter_taps: Some(48),
    })
    .expect("a bridge");
    let status = Arc::new(Mutex::new(Status::default()));
    let slot = FigureSlot::started_at(42);
    assert_eq!(slot.figures(), None);
    bridge.publish_figures(&slot, Arc::clone(&status));
    let before = slot.figures().expect("published");
    assert_eq!(before.run_start_unix_ms, 42);
    assert_eq!(before.to_asio_target_us, 0, "no driver opened yet");
    let opened = Opened {
        rate: 96_000.0,
        buffer_frames: 256,
        driver_inputs: 1,
        driver_outputs: 1,
        inputs: vec![0],
        outputs: vec![0],
        input_sample: Some(Sample::Int32),
        output_sample: Sample::Int32,
    };
    let _side = bridge.asio_side(&opened).expect("a driver side");
    {
        let mut s = status.lock().unwrap();
        s.late_callbacks = 3;
        s.late_bins = [1, 0, 2, 0, 0, 0, 0, 9];
    }
    let f = slot.figures().unwrap();
    assert_eq!(
        (f.to_asio_target_us, f.from_asio_target_us),
        (3000, 4000),
        "to-ASIO set, from-ASIO the buffer + one block"
    );
    // 48 taps at 96 <-> 96 kHz: 0.250 ms a direction.
    assert_eq!(
        (f.to_asio_converter_delay_us, f.from_asio_converter_delay_us),
        (250, 250)
    );
    assert_eq!(f.late_callbacks, 3);
    assert_eq!(f.late_bins, [1, 0, 2, 0, 0, 0, 0, 9]);
    assert_eq!(
        own_figures(
            &bridge.to_asio_stats(),
            &bridge.from_asio_stats(),
            &status.lock().unwrap()
        ),
        OwnFigures {
            to_asio_target_us: 3000,
            from_asio_target_us: 4000,
            to_asio_converter_delay_us: 250,
            from_asio_converter_delay_us: 250,
            late_callbacks: 3,
            late_bins: [1, 0, 2, 0, 0, 0, 0, 9],
        }
    );
}
