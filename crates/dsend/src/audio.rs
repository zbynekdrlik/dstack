// SPDX-License-Identifier: MIT OR Apache-2.0

//! The captured output through cpal (WASAPI shared on Windows): thin glue,
//! which needs a sound card and so runs by hand, never in CI. Its decisions
//! are the library's: the choice of output
//! ([`dstack_endpoint::outputs`]), the push ([`dsend::capture`]), the
//! conversion and steering ([`dsend::source`]), each tested.
//!
//! An output device opened as an input stream is WASAPI loopback (cpal's
//! WASAPI host sets `AUDCLNT_STREAMFLAGS_LOOPBACK` for a render endpoint).
//! Loopback hands over no packets while nothing plays on the output, so a
//! second stream renders silence on it: the keep-alive, which keeps the
//! capture running on the output's clock (an idle output is sent as
//! silence, never as an underrun).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use dsend::capture::push_interleaved;
use dstack_endpoint::fifo::Fifo;

pub use dstack_endpoint::endpoints::{config_of, outputs};

/// A running capture of one output: dropping it stops both streams.
pub struct Capturing {
    _capture: cpal::Stream,
    _keep_alive: cpal::Stream,
}

/// Opens and starts the loopback capture of `device` at its mix `config`
/// into `fifo`, and the silent keep-alive stream on the same output. A
/// stream error (the output lost, its format changed) sets `failed`: the
/// session opens the output again ([`dsend::status::reopen_due`]).
pub fn start(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    fifo: Arc<Fifo>,
    failed: &Arc<AtomicBool>,
) -> Result<Capturing, String> {
    let (capture, keep_alive) = match config.sample_format() {
        SampleFormat::F32 => (
            capture::<f32>(device, config, fifo, Arc::clone(failed))?,
            keep_alive::<f32>(device, config, Arc::clone(failed))?,
        ),
        SampleFormat::I16 => (
            capture::<i16>(device, config, fifo, Arc::clone(failed))?,
            keep_alive::<i16>(device, config, Arc::clone(failed))?,
        ),
        SampleFormat::I32 => (
            capture::<i32>(device, config, fifo, Arc::clone(failed))?,
            keep_alive::<i32>(device, config, Arc::clone(failed))?,
        ),
        SampleFormat::U16 => (
            capture::<u16>(device, config, fifo, Arc::clone(failed))?,
            keep_alive::<u16>(device, config, Arc::clone(failed))?,
        ),
        SampleFormat::F64 => (
            capture::<f64>(device, config, fifo, Arc::clone(failed))?,
            keep_alive::<f64>(device, config, Arc::clone(failed))?,
        ),
        other => {
            return Err(format!(
                "the output's sample format {other} is not supported"
            ));
        }
    };
    keep_alive
        .play()
        .map_err(|e| format!("the keep-alive stream did not start: {e}"))?;
    capture
        .play()
        .map_err(|e| format!("the loopback capture did not start: {e}"))?;
    Ok(Capturing {
        _capture: capture,
        _keep_alive: keep_alive,
    })
}

fn capture<T: SizedSample + Send + 'static>(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    fifo: Arc<Fifo>,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, String>
where
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels());
    device
        .build_input_stream::<T, _, _>(
            config.config(),
            move |data: &[T], _| {
                push_interleaved(&fifo, data, channels, |x| f64::from(x.to_sample::<f32>()));
            },
            // The driver's own thread: a lost device or a driver error.
            move |e| {
                tracing::warn!("dsend: the loopback capture reported: {e}");
                if dstack_endpoint::stream::needs_reopen(e.kind()) {
                    failed.store(true, Ordering::Relaxed);
                }
            },
            None,
        )
        .map_err(|e| format!("the loopback capture could not be opened: {e}"))
}

fn keep_alive<T: SizedSample + Send + 'static>(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, String> {
    device
        .build_output_stream::<T, _, _>(
            config.config(),
            |data: &mut [T], _| data.fill(T::EQUILIBRIUM),
            move |e| {
                tracing::warn!("dsend: the keep-alive stream reported: {e}");
                if dstack_endpoint::stream::needs_reopen(e.kind()) {
                    failed.store(true, Ordering::Relaxed);
                }
            },
            None,
        )
        .map_err(|e| format!("the keep-alive stream could not be opened: {e}"))
}
