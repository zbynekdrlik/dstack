// SPDX-License-Identifier: GPL-3.0-or-later

//! Sample-rate conversion for our apps (dstack #90, the owner's concept of
//! 3.11.2026): an app keeps its own rate, and the conversion is ours, done
//! once, measured once. No Dante code: dlink uses it between the network's
//! rate and the app's, and an app may use it for its other outputs.
//!
//! One filter core, rubato's band-limited sinc ([`Async`]) with the
//! settings SongPlayer measured (256 taps, a 256× table, BlackmanHarris²,
//! cubic interpolation, the automatic cutoff): THD+N above 120 dB, and the
//! band that folds back held below −120 dBFS ([`PARAMS`]). Equal rates are
//! a plain copy, with no filter and no delay. A fixed ratio on one clock
//! holds the ratio exactly; the ASRC between two clocks (phase 2) steers the
//! same stage.
//!
//! Data is channel-major `f64`: channel `c` of a buffer of `frames` frames
//! sits at `c * frames ..`. [`Converter::process`] allocates nothing.

use rubato::audioadapter_buffers::direct::SequentialSlice;
use rubato::{
    Adjustable, Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

pub mod asrc;

/// The sinc filter's length, taps.
pub const TAPS: usize = 256;

/// The sinc table's oversampling (rubato's default is 128; SongPlayer
/// measured 256).
pub const OVERSAMPLING: usize = 256;

/// The filter core's settings (module doc).
pub const PARAMS: SincInterpolationParameters = SincInterpolationParameters {
    sinc_len: TAPS,
    f_cutoff: None,
    oversampling_factor: OVERSAMPLING,
    interpolation: SincInterpolationType::Cubic,
    window: WindowFunction::BlackmanHarris2,
};

/// How far an ASRC's ratio may leave its nominal value: twice the
/// controller's bound (600 ppm), so a ramp never reaches the core's limit.
const MAX_RELATIVE: f64 = 1.000_6;
const _: () = assert!(asrc::MAX_PPM == 300.0);

/// Which side of a converter has a fixed block size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fixed {
    /// Every call takes the same input frames; the output frames vary
    /// around `input * ratio` (a device's RX block into an app).
    Input,
    /// Every call gives the same output frames; the input it needs varies
    /// ([`Converter::input_frames_next`]; an app's output into a device's
    /// TX block).
    Output,
}

/// Why a converter cannot be made or run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConvertError {
    /// A rate of zero, no channels or an empty block.
    Invalid(String),
    /// The filter core refused (its message).
    Core(String),
}

impl std::fmt::Display for ConvertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConvertError::Invalid(why) => write!(f, "invalid converter: {why}"),
            ConvertError::Core(why) => write!(f, "resampler: {why}"),
        }
    }
}

impl std::error::Error for ConvertError {}

/// The engine behind a converter.
enum Engine {
    /// Equal rates: a copy.
    Copy,
    /// The sinc stage.
    Sinc(Box<Async<f64>>),
}

/// A converter from `from` Hz to `to` Hz on one clock, for `channels`
/// channels, `block` frames on its [`Fixed`] side.
pub struct Converter {
    engine: Engine,
    channels: usize,
    block: usize,
    fixed: Fixed,
    from: u32,
    to: u32,
}

impl std::fmt::Debug for Converter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Converter")
            .field("from", &self.from)
            .field("to", &self.to)
            .field("channels", &self.channels)
            .field("block", &self.block)
            .field("fixed", &self.fixed)
            .field("copy", &matches!(self.engine, Engine::Copy))
            .finish()
    }
}

impl Converter {
    /// A converter from `from` to `to` Hz for `channels` channels, `block`
    /// frames on its `fixed` side.
    pub fn new(
        from: u32,
        to: u32,
        channels: usize,
        block: usize,
        fixed: Fixed,
    ) -> Result<Converter, ConvertError> {
        Self::build(from, to, channels, block, fixed, false)
    }

    /// A converter between two clocks (the ASRC): always the sinc stage,
    /// equal rates too, its ratio adjustable by [`Converter::set_ppm`] up
    /// to ±[`asrc::MAX_PPM`] (with headroom).
    pub fn drifting(
        from: u32,
        to: u32,
        channels: usize,
        block: usize,
        fixed: Fixed,
    ) -> Result<Converter, ConvertError> {
        Self::build(from, to, channels, block, fixed, true)
    }

    fn build(
        from: u32,
        to: u32,
        channels: usize,
        block: usize,
        fixed: Fixed,
        drifting: bool,
    ) -> Result<Converter, ConvertError> {
        if from == 0 || to == 0 || channels == 0 || block == 0 {
            return Err(ConvertError::Invalid(format!(
                "from {from} Hz, to {to} Hz, {channels} channels, block {block}"
            )));
        }
        let engine = if from == to && !drifting {
            Engine::Copy
        } else {
            let side = match fixed {
                Fixed::Input => FixedAsync::Input,
                Fixed::Output => FixedAsync::Output,
            };
            let ratio = f64::from(to) / f64::from(from);
            let room = if drifting { MAX_RELATIVE } else { 1.0 };
            let stage = Async::<f64>::new_sinc(ratio, room, &PARAMS, block, channels, side)
                .map_err(|e| ConvertError::Core(e.to_string()))?;
            Engine::Sinc(Box::new(stage))
        };
        Ok(Converter {
            engine,
            channels,
            block,
            fixed,
            from,
            to,
        })
    }

    /// Whether this converter only copies (equal rates).
    pub fn is_copy(&self) -> bool {
        matches!(self.engine, Engine::Copy)
    }

    /// The output rate over the input rate.
    pub fn ratio(&self) -> f64 {
        f64::from(self.to) / f64::from(self.from)
    }

    /// The input frames the next call takes.
    pub fn input_frames_next(&self) -> usize {
        match &self.engine {
            Engine::Copy => self.block,
            Engine::Sinc(s) => s.input_frames_next(),
        }
    }

    /// The most input frames any call takes.
    pub fn input_frames_max(&self) -> usize {
        match &self.engine {
            Engine::Copy => self.block,
            Engine::Sinc(s) => s.input_frames_max(),
        }
    }

    /// The most output frames any call gives.
    pub fn output_frames_max(&self) -> usize {
        match &self.engine {
            Engine::Copy => self.block,
            Engine::Sinc(s) => s.output_frames_max(),
        }
    }

    /// The delay, output frames: how far behind its input the output runs
    /// (none for a copy).
    pub fn delay_frames(&self) -> usize {
        match &self.engine {
            Engine::Copy => 0,
            Engine::Sinc(s) => s.output_delay(),
        }
    }

    /// Converts `frames_in` frames of `input` (channel-major, channel `c` at
    /// `c * frames_in`) into `output` (channel-major, channel `c` at
    /// `c * out_frames`, room for `out_frames` frames). `frames_in` must be
    /// [`Converter::input_frames_next`]; `out_frames` at least
    /// [`Converter::output_frames_max`]. Returns the input frames taken
    /// and the output frames written.
    pub fn process(
        &mut self,
        input: &[f64],
        frames_in: usize,
        output: &mut [f64],
        out_frames: usize,
    ) -> Result<(usize, usize), ConvertError> {
        let channels = self.channels;
        match &mut self.engine {
            Engine::Copy => {
                if frames_in > out_frames
                    || input.len() < channels * frames_in
                    || output.len() < channels * out_frames
                {
                    return Err(ConvertError::Invalid(format!(
                        "copy of {frames_in} frames into room for {out_frames}"
                    )));
                }
                for c in 0..channels {
                    output[c * out_frames..c * out_frames + frames_in]
                        .copy_from_slice(&input[c * frames_in..(c + 1) * frames_in]);
                }
                Ok((frames_in, frames_in))
            }
            Engine::Sinc(stage) => {
                let core = |e: &dyn std::fmt::Display| ConvertError::Core(e.to_string());
                let i = SequentialSlice::new(input, channels, frames_in).map_err(|e| core(&e))?;
                let mut o =
                    SequentialSlice::new_mut(output, channels, out_frames).map_err(|e| core(&e))?;
                stage
                    .process_into_buffer(&i, &mut o, None)
                    .map_err(|e| core(&e))
            }
        }
    }

    /// Leaves the nominal ratio by `ppm` (positive: more output per input),
    /// ramped over the next call, within ±[`asrc::MAX_PPM`]. An error for a
    /// converter made by [`Converter::new`] (its ratio is fixed).
    pub fn set_ppm(&mut self, ppm: f64) -> Result<(), ConvertError> {
        let Engine::Sinc(stage) = &mut self.engine else {
            return Err(ConvertError::Invalid(
                "a copy has no ratio to adjust".into(),
            ));
        };
        let ppm = ppm.clamp(-asrc::MAX_PPM, asrc::MAX_PPM);
        stage
            .set_resample_ratio_relative(1.0 + ppm * 1e-6, true)
            .map_err(|e| ConvertError::Core(e.to_string()))
    }

    /// Forgets the filter's history (after a gap in the audio).
    pub fn reset(&mut self) {
        if let Engine::Sinc(s) = &mut self.engine {
            s.reset();
        }
    }
}

#[cfg(test)]
mod tests;
