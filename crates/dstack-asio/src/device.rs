// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a host needs from an ASIO driver ([`Device`]: the Windows glue
//! `win::WinDevice`, a test's fake, [`Unsupported`] off Windows), and the
//! checks of what an open read.
//!
//! There is no setter: a host follows the driver. Its rate, preferred
//! buffer and sample types are read, never set (Dante Controller or
//! SoundGrid owns them; a live resize made iemmixer's driver reset and
//! fault). The device lives and dies on one worker thread (a COM STA on
//! Windows), so it is not `Send`.

use crate::engine::Engine;
use crate::format::{Sample, unsupported_sample_text};
use crate::state::{DeviceEvents, Reason};

/// The driver channels a host uses, 0-based: `inputs` become the app's
/// `Process` inputs in this order, `outputs` its outputs in this order.
/// Every other driver output plays silence; other inputs are not opened.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Channels {
    pub inputs: Vec<u32>,
    pub outputs: Vec<u32>,
}

/// What an open read from the driver.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Opened {
    /// The driver's rate, Hz (admitted: 8–384 kHz).
    pub rate: f64,
    /// Its preferred buffer, frames (the buffer a start creates).
    pub buffer_frames: u32,
    /// Its channel counts.
    pub driver_inputs: u32,
    pub driver_outputs: u32,
    /// The sample type of the inputs used (`None`: none used).
    pub input_sample: Option<Sample>,
    /// The sample type of every output (all are written; `None`: the
    /// driver has none).
    pub output_sample: Option<Sample>,
}

/// What a start read from the driver: its latencies, frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Started {
    pub input_latency_frames: u32,
    pub output_latency_frames: u32,
}

/// One ASIO driver, on its worker thread.
pub trait Device {
    /// Load the driver and read what it runs at (nothing is set); the
    /// `channels` are checked against it ([`check_channels`]).
    fn open(&mut self, driver: &str, channels: &Channels) -> Result<Opened, Reason>;
    /// Create the buffers and start; every buffer switch runs `engine`.
    fn start(&mut self, engine: Engine) -> Result<Started, Reason>;
    /// The driver's messages and the engine's counts since the last poll
    /// (Windows: pumps the thread's window messages too).
    fn poll(&mut self) -> DeviceEvents;
    /// Frames the driver took since the start.
    fn consumed_frames(&self) -> u64;
    /// The driver's latencies now (read again after its
    /// `kAsioLatenciesChanged`).
    fn latencies(&self) -> Started;
    /// Stop and release the driver (idempotent).
    fn close(&mut self);
}

/// The device off Windows: every open is refused, ASIO runs on Windows
/// only. An app's cross-platform code builds its runner on it.
#[derive(Debug, Default)]
pub struct Unsupported;

impl Device for Unsupported {
    fn open(&mut self, _driver: &str, _channels: &Channels) -> Result<Opened, Reason> {
        Err(Reason::WindowsOnly)
    }

    fn start(&mut self, _engine: Engine) -> Result<Started, Reason> {
        Err(Reason::WindowsOnly)
    }

    fn poll(&mut self) -> DeviceEvents {
        DeviceEvents::default()
    }

    fn consumed_frames(&self) -> u64 {
        0
    }

    fn latencies(&self) -> Started {
        Started::default()
    }

    fn close(&mut self) {}
}

/// The one sample type of `codes` (the ASIOSampleType of each channel used),
/// for `what` ("input", "output"): refused when one is unsupported or they
/// mix types; `None` for no channel.
pub fn one_sample(codes: &[i32], what: &str) -> Result<Option<Sample>, Reason> {
    let mut sample: Option<Sample> = None;
    for &code in codes {
        let this =
            Sample::from_code(code).map_err(|c| Reason::Refused(unsupported_sample_text(c)))?;
        if sample.is_some_and(|s| s != this) {
            return Err(Reason::Refused(format!(
                "the driver's {what} channels mix sample types"
            )));
        }
        sample = Some(this);
    }
    Ok(sample)
}

/// Whether `channels` exist on a driver with `inputs` and `outputs`
/// channels, each used once; the refusal names the first that does not.
pub fn check_channels(channels: &Channels, inputs: u32, outputs: u32) -> Result<(), Reason> {
    for (list, count, what) in [
        (&channels.inputs, inputs, "input"),
        (&channels.outputs, outputs, "output"),
    ] {
        for (i, &c) in list.iter().enumerate() {
            if c >= count {
                return Err(Reason::Refused(format!(
                    "the driver has {count} {what} channels (asked for {what} {})",
                    c + 1
                )));
            }
            if list[..i].contains(&c) {
                return Err(Reason::Refused(format!(
                    "{what} channel {} is asked for twice",
                    c + 1
                )));
            }
        }
    }
    Ok(())
}

/// The preferred buffer a driver reported, as frames: refused unless it is
/// positive.
pub fn admit_buffer(preferred: i32) -> Result<u32, Reason> {
    u32::try_from(preferred)
        .ok()
        .filter(|f| *f > 0)
        .ok_or_else(|| {
            Reason::Refused(format!("the driver prefers a buffer of {preferred} frames"))
        })
}

#[cfg(test)]
mod tests;
