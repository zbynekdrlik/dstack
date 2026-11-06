// SPDX-License-Identifier: MIT OR Apache-2.0

//! The output device through cpal (WASAPI shared on Windows, ALSA on
//! Linux): thin glue, which needs a sound card and so runs by hand, never
//! in CI. Its decisions are the library's: the choice of output
//! ([`dlisten::outputs`]), the conversion and steering
//! ([`dlisten::player`]), each tested.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use dlisten::outputs::Output;
use dlisten::player::Player;

/// The host's output devices, with their names as the library sees them.
/// An error when the audio system cannot list them.
pub fn outputs() -> Result<(Vec<cpal::Device>, Vec<Output>), String> {
    let host = cpal::default_host();
    let default = host.default_output_device();
    let devices: Vec<cpal::Device> = host
        .output_devices()
        .map_err(|e| {
            format!(
                "the audio system ({}) lists no outputs: {e}",
                host.id().name()
            )
        })?
        .collect();
    let described = devices
        .iter()
        .map(|d| Output {
            name: d
                .description()
                .map_or_else(|e| format!("<unnamed: {e}>"), |n| n.name().to_string()),
            default: default.as_ref() == Some(d),
            config: d.default_output_config().map_or_else(
                |e| format!("no default config: {e}"),
                |c| {
                    format!(
                        "{} Hz, {} ch, {}",
                        c.sample_rate(),
                        c.channels(),
                        c.sample_format()
                    )
                },
            ),
        })
        .collect();
    Ok((devices, described))
}

/// A running output stream and its config.
pub struct Playing {
    /// Kept for its life: dropping it stops the output.
    _stream: cpal::Stream,
}

/// The output's default config: its rate and channels (the player is
/// made for that rate).
pub fn config_of(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig, String> {
    device
        .default_output_config()
        .map_err(|e| format!("the output has no default config: {e}"))
}

/// Opens (does not start) a stream of `player` on `device` at `config`
/// (its default): the driver's callback will pull the frames, each made
/// into the driver's sample format.
pub fn open(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    player: Player,
) -> Result<cpal::Stream, String> {
    match config.sample_format() {
        SampleFormat::F32 => build::<f32>(device, config, player),
        SampleFormat::I16 => build::<i16>(device, config, player),
        SampleFormat::I32 => build::<i32>(device, config, player),
        SampleFormat::U16 => build::<u16>(device, config, player),
        SampleFormat::F64 => build::<f64>(device, config, player),
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
) -> Result<cpal::Stream, String> {
    let outputs = usize::from(config.channels());
    device
        .build_output_stream::<T, _, _>(
            config.config(),
            move |data: &mut [T], _| {
                player.fill_with(data, outputs, |x| T::from_sample(x as f32));
            },
            // The driver's own thread: a lost device or a driver error.
            |e| tracing::warn!("dlisten: the output stream reported: {e}"),
            None,
        )
        .map_err(|e| format!("the output stream could not be opened: {e}"))
}
