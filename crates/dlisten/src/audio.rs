// SPDX-License-Identifier: MIT OR Apache-2.0

//! The output device through cpal (WASAPI shared on Windows, ALSA on
//! Linux): thin glue, which needs a sound card and so runs by hand, never
//! in CI. Its decisions are the library's: the choice of output
//! ([`dlisten::outputs`]), the conversion and steering
//! ([`dlisten::player`]), each tested.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use dlisten::player::Player;

pub use dstack_endpoint::endpoints::{config_of, outputs};

/// A running output stream and its config.
pub struct Playing {
    /// Kept for its life: dropping it stops the output.
    _stream: cpal::Stream,
}

/// Opens (does not start) a stream of `player` on `device` at `config`
/// (its default): the driver's callback will pull the frames, each made
/// into the driver's sample format. A stream error (the output lost, its
/// driver reset) sets `failed`: the session opens the output again.
pub fn open(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    player: Player,
    failed: &Arc<AtomicBool>,
) -> Result<cpal::Stream, String> {
    match config.sample_format() {
        SampleFormat::F32 => build::<f32>(device, config, player, Arc::clone(failed)),
        SampleFormat::I16 => build::<i16>(device, config, player, Arc::clone(failed)),
        SampleFormat::I32 => build::<i32>(device, config, player, Arc::clone(failed)),
        SampleFormat::U16 => build::<u16>(device, config, player, Arc::clone(failed)),
        SampleFormat::F64 => build::<f64>(device, config, player, Arc::clone(failed)),
        other => Err(format!(
            "the output's sample format {other} is not supported"
        )),
    }
}

/// Starts an opened stream.
pub fn start(stream: cpal::Stream) -> Result<Playing, String> {
    stream
        .play()
        .map_err(|e| format!("the output did not start: {e}"))?;
    Ok(Playing { _stream: stream })
}

fn build<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    mut player: Player,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, String> {
    let outputs = usize::from(config.channels());
    device
        .build_output_stream::<T, _, _>(
            config.config(),
            move |data: &mut [T], _| {
                player.fill_with(data, outputs, |x| T::from_sample(x as f32));
            },
            // The driver's own thread: a lost device or a driver error.
            move |e| {
                tracing::warn!("dlisten: the output stream reported: {e}");
                failed.store(true, Ordering::Relaxed);
            },
            None,
        )
        .map_err(|e| format!("the output stream could not be opened: {e}"))
}
