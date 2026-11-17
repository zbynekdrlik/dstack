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
use dlink::bridge::SharedPull;
use dlisten::player::Player;

pub use dstack_endpoint::endpoints::{config_of, outputs};

/// A running output stream and its config.
pub struct Playing {
    /// Kept for its life: dropping it stops the output.
    _stream: cpal::Stream,
}

/// Opens (does not start) a stream of `player` on `device` at `config`
/// (its default): the driver's callback first brings the FIFO up to date
/// from the RX rings (`pull`, dstack #307: the thread with the deadline
/// reads the network audio itself), then pulls the frames, each made into
/// the driver's sample format. A stream error (the output lost, its driver
/// reset) sets `failed`: the session opens the output again.
pub fn open(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    player: Player,
    pull: &Arc<SharedPull>,
    failed: &Arc<AtomicBool>,
) -> Result<cpal::Stream, String> {
    let (pull, failed) = (Arc::clone(pull), Arc::clone(failed));
    match config.sample_format() {
        SampleFormat::F32 => build::<f32>(device, config, player, pull, failed),
        SampleFormat::I16 => build::<i16>(device, config, player, pull, failed),
        SampleFormat::I32 => build::<i32>(device, config, player, pull, failed),
        SampleFormat::U16 => build::<u16>(device, config, player, pull, failed),
        SampleFormat::F64 => build::<f64>(device, config, player, pull, failed),
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
    pull: Arc<SharedPull>,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, String> {
    let outputs = usize::from(config.channels());
    device
        .build_output_stream::<T, _, _>(
            config.config(),
            move |data: &mut [T], _| {
                // Never waits: only an old stream's last callback during an
                // output switch, or the main thread's idle drain, can hold
                // it (dstack #307). The frames it wrote tell the player
                // whether this call follows a gap of the output itself (the
                // callback held past WASAPI's buffer: the card played
                // silence, and the call asks for the whole buffer).
                let pulled = pull.pull();
                player.fill_pulled(pulled, data, outputs, |x| T::from_sample(x as f32));
            },
            // The driver's own thread: a lost device or a driver error.
            move |e| {
                tracing::warn!("dlisten: the output stream reported: {e}");
                if dstack_endpoint::stream::needs_reopen(e.kind()) {
                    failed.store(true, Ordering::Relaxed);
                }
            },
            None,
        )
        .map_err(|e| format!("the output stream could not be opened: {e}"))
}
