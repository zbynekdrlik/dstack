// SPDX-License-Identifier: MIT OR Apache-2.0

//! The PC's output endpoints through cpal (WASAPI shared on Windows, ALSA
//! on Linux): thin glue, which needs a sound card and so runs by hand,
//! never in CI. dlisten plays on one; dsend captures one's loopback. The
//! choice among them is [`crate::outputs`] (tested).

use cpal::traits::{DeviceTrait, HostTrait};

use crate::outputs::Output;

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

/// The output's default config (its shared mix format: rate, channels,
/// sample format).
pub fn config_of(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig, String> {
    device
        .default_output_config()
        .map_err(|e| format!("the output has no default config: {e}"))
}
