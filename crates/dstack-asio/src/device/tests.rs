// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use dstack_audio_io::{Block, Process};

#[test]
fn the_channels_must_exist_and_be_used_once() {
    let ch = |inputs: &[u32], outputs: &[u32]| Channels {
        inputs: inputs.to_vec(),
        outputs: outputs.to_vec(),
    };
    assert_eq!(check_channels(&ch(&[0, 7], &[0, 1, 63]), 8, 64), Ok(()));
    assert_eq!(check_channels(&ch(&[], &[]), 0, 0), Ok(()));
    assert_eq!(
        check_channels(&ch(&[8], &[]), 8, 64),
        Err(Reason::Refused(
            "the driver has 8 input channels (asked for input 9)".into()
        ))
    );
    assert_eq!(
        check_channels(&ch(&[], &[0, 64]), 8, 64),
        Err(Reason::Refused(
            "the driver has 64 output channels (asked for output 65)".into()
        ))
    );
    assert_eq!(
        check_channels(&ch(&[1, 2, 1], &[]), 8, 64),
        Err(Reason::Refused("input channel 2 is asked for twice".into()))
    );
    assert_eq!(
        check_channels(&ch(&[], &[3, 3]), 8, 64),
        Err(Reason::Refused(
            "output channel 4 is asked for twice".into()
        ))
    );
}

#[test]
fn the_channels_used_have_one_supported_sample_type() {
    assert_eq!(one_sample(&[], "input"), Ok(None));
    assert_eq!(one_sample(&[18, 18, 18], "output"), Ok(Some(Sample::Int32)));
    assert_eq!(one_sample(&[27], "input"), Ok(Some(Sample::Int32In24)));
    assert_eq!(
        one_sample(&[18, 19], "output"),
        Err(Reason::Refused(
            "the driver's output channels mix sample types".into()
        ))
    );
    assert_eq!(
        one_sample(&[18, 20], "input"),
        Err(Reason::Refused(unsupported_sample_text(20)))
    );
}

#[test]
fn the_preferred_buffer_must_be_positive() {
    assert_eq!(admit_buffer(256), Ok(256));
    assert_eq!(admit_buffer(1), Ok(1));
    for bad in [0, -1, i32::MIN] {
        assert_eq!(
            admit_buffer(bad),
            Err(Reason::Refused(format!(
                "the driver prefers a buffer of {bad} frames"
            )))
        );
    }
}

#[test]
fn off_windows_every_open_is_refused() {
    let mut d = Unsupported;
    assert_eq!(d.open("x", &Channels::default()), Err(Reason::WindowsOnly));
    assert_eq!(d.poll(), DeviceEvents::default());
    assert_eq!(d.consumed_frames(), 0);
    assert_eq!(d.latencies(), Started::default());
    d.close();
}

struct Nothing;

impl Process for Nothing {
    fn process(&mut self, _block: &mut Block<'_>) {}
}

#[test]
fn off_windows_no_engine_starts() {
    let opened = Opened {
        rate: 48_000.0,
        buffer_frames: 64,
        driver_inputs: 0,
        driver_outputs: 2,
        input_sample: None,
        output_sample: Some(Sample::Int32),
    };
    let engine = Engine::new(Box::new(Nothing), &opened, &Channels::default());
    assert_eq!(Unsupported.start(engine), Err(Reason::WindowsOnly));
}
