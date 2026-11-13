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

/// The driver channels a host uses, 0-based: the inputs become the app's
/// `Process` inputs in their order, `outputs` its outputs in this order.
/// Every other driver output plays silence; other inputs are not opened.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Channels {
    pub inputs: Inputs,
    pub outputs: Vec<u32>,
}

/// Which driver inputs a host reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inputs {
    /// These, each of them (a bridge's channel map): a missing one, or ones
    /// that mix sample types, refuse the open.
    Exact(Vec<u32>),
    /// The driver's first `max` inputs when they share one supported sample
    /// type, else none, never a refusal (SongPlayer's loop check taps
    /// `min(inputs, 8)` this way and plays unchecked without them).
    FirstReadable { max: u32 },
}

impl Default for Inputs {
    fn default() -> Self {
        Inputs::Exact(Vec::new())
    }
}

/// What an open read from the driver.
#[derive(Clone, Debug, PartialEq)]
pub struct Opened {
    /// The driver's rate, Hz (admitted: 8–384 kHz).
    pub rate: f64,
    /// Its preferred buffer, frames (the buffer a start creates).
    pub buffer_frames: u32,
    /// Its channel counts.
    pub driver_inputs: u32,
    pub driver_outputs: u32,
    /// The inputs read (the app's `Process` inputs in this order), as
    /// [`resolve_inputs`] settled them.
    pub inputs: Vec<u32>,
    /// The outputs the app's `Process` outputs go to, in this order.
    pub outputs: Vec<u32>,
    /// The sample type of the inputs read (`None`: none read).
    pub input_sample: Option<Sample>,
    /// The sample type of every output (all are written).
    pub output_sample: Sample,
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

/// The one sample type of every driver output (`codes`): a driver with
/// none is refused, as SongPlayer refuses it.
pub fn admit_outputs(codes: &[i32]) -> Result<Sample, Reason> {
    one_sample(codes, "output")?
        .ok_or_else(|| Reason::Refused("the driver has no output channel".into()))
}

/// The inputs read and their sample type, for `inputs` on a driver with
/// `driver_inputs` channels; `codes` reads the sample type codes of the
/// given channels (a driver call). Exact inputs refuse a mix or an
/// unsupported type; the first readable ones are none then, never a
/// refusal (nor when reading their types fails).
pub fn resolve_inputs(
    inputs: &Inputs,
    driver_inputs: u32,
    codes: impl Fn(&[u32]) -> Result<Vec<i32>, Reason>,
) -> Result<(Vec<u32>, Option<Sample>), Reason> {
    match inputs {
        Inputs::Exact(list) => Ok((list.clone(), one_sample(&codes(list)?, "input")?)),
        Inputs::FirstReadable { max } => {
            let list: Vec<u32> = (0..driver_inputs.min(*max)).collect();
            match codes(&list).map(|c| one_sample(&c, "input")) {
                Ok(Ok(Some(sample))) => Ok((list, Some(sample))),
                _ => Ok((Vec::new(), None)),
            }
        }
    }
}

/// Whether `channels` exist on a driver with `inputs` and `outputs`
/// channels, each used once; the refusal names the first that does not.
/// First-readable inputs are never out of range.
pub fn check_channels(channels: &Channels, inputs: u32, outputs: u32) -> Result<(), Reason> {
    let exact = match &channels.inputs {
        Inputs::Exact(list) => list.as_slice(),
        Inputs::FirstReadable { .. } => &[],
    };
    for (list, count, what) in [
        (exact, inputs, "input"),
        (channels.outputs.as_slice(), outputs, "output"),
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
