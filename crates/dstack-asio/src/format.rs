// SPDX-License-Identifier: MIT OR Apache-2.0

//! An ASIO driver's sample types (ASIOSampleType, little-endian only: the
//! big-endian ones exist on old Macs, Float64 and DSD on no driver we use)
//! and our channel-major `f64` written into them and read back from them.
//!
//! Lifted from SongPlayer's `asio_format.rs` (songplayer #233, dstack #243)
//! with its exact encode, so an app moving onto this crate writes the same
//! bytes: integers use a SYMMETRIC full scale (±(2^(bits−1) − 1)); a value
//! is clamped to ±1.0 and a non-finite one is silence; a 24-bit value is
//! three little-endian bytes. An `f32` widened to `f64` (exact) encodes to
//! the bytes SongPlayer's `f32` path writes. The decode is the inverse,
//! never clamped (an over-range float reads as what it is), a non-finite
//! float silence.
//!
//! Pure and allocation-free: the driver's buffer switch
//! ([`crate::Engine`]) calls [`Sample::encode`] once per output channel
//! and [`Sample::decode`] once per input channel.

/// The sample types we read and write (asio.h codes 16-19, 24-27).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sample {
    Int16,
    Int24,
    Int32,
    Float32,
    /// A 32-bit container holding a sign-extended 16/18/20/24-bit value.
    Int32In16,
    Int32In18,
    Int32In20,
    Int32In24,
}

impl Sample {
    /// The type of an ASIOSampleType code; an unsupported code is returned.
    pub fn from_code(code: i32) -> Result<Self, i32> {
        Ok(match code {
            16 => Self::Int16,
            17 => Self::Int24,
            18 => Self::Int32,
            19 => Self::Float32,
            24 => Self::Int32In16,
            25 => Self::Int32In18,
            26 => Self::Int32In20,
            27 => Self::Int32In24,
            other => return Err(other),
        })
    }

    /// The type's ASIO name (a status shows it).
    pub fn name(self) -> &'static str {
        match self {
            Self::Int16 => "Int16LSB",
            Self::Int24 => "Int24LSB",
            Self::Int32 => "Int32LSB",
            Self::Float32 => "Float32LSB",
            Self::Int32In16 => "Int32LSB16",
            Self::Int32In18 => "Int32LSB18",
            Self::Int32In20 => "Int32LSB20",
            Self::Int32In24 => "Int32LSB24",
        }
    }

    /// Bytes per sample.
    pub fn bytes(self) -> usize {
        match self {
            Self::Int16 => 2,
            Self::Int24 => 3,
            _ => 4,
        }
    }

    /// The symmetric integer full scale; `None` for Float32.
    fn full_scale(self) -> Option<f64> {
        match self {
            Self::Int16 | Self::Int32In16 => Some(32_767.0),
            Self::Int32In18 => Some(131_071.0),
            Self::Int32In20 => Some(524_287.0),
            Self::Int24 | Self::Int32In24 => Some(8_388_607.0),
            Self::Int32 => Some(2_147_483_647.0),
            Self::Float32 => None,
        }
    }

    /// One channel `src` into the driver's half-buffer `dst`: as many whole
    /// samples as both hold, every byte after them zeroed (a short `src`
    /// plays silence for the rest).
    pub fn encode(self, src: &[f64], dst: &mut [u8]) {
        let bytes = self.bytes();
        let full = self.full_scale();
        let mut written = 0;
        for (out, &x) in dst.chunks_exact_mut(bytes).zip(src) {
            let x = clean(x);
            match (self, full) {
                (_, None) => out.copy_from_slice(&(x as f32).to_le_bytes()),
                (Self::Int16, Some(full)) => {
                    out.copy_from_slice(&((x * full).round() as i16).to_le_bytes());
                }
                (Self::Int24, Some(full)) => {
                    let b = ((x * full).round() as i32).to_le_bytes();
                    out.copy_from_slice(&b[..3]);
                }
                (_, Some(full)) => {
                    out.copy_from_slice(&((x * full).round() as i32).to_le_bytes());
                }
            }
            written += bytes;
        }
        dst[written..].fill(0);
    }

    /// The driver's half-buffer `src` into one channel `dst`: as many whole
    /// samples as both hold, every sample after them silence (an input whose
    /// buffer is missing reads `&[]`).
    pub fn decode(self, src: &[u8], dst: &mut [f64]) {
        let mut read = 0;
        for (s, out) in src.chunks_exact(self.bytes()).zip(dst.iter_mut()) {
            *out = self.decode_one(s);
            read += 1;
        }
        dst[read..].fill(0.0);
    }

    /// One sample of this type (`src` holds at least [`Self::bytes`]): an
    /// integer over its symmetric full scale (a 24-bit one sign-extended),
    /// a float as it is, a non-finite float silence.
    pub fn decode_one(self, src: &[u8]) -> f64 {
        let word = |b: &[u8]| [b[0], b[1], b[2], b[3]];
        match (self, self.full_scale()) {
            (_, None) => {
                let x = f32::from_le_bytes(word(src));
                if x.is_finite() { f64::from(x) } else { 0.0 }
            }
            (Self::Int16, Some(full)) => f64::from(i16::from_le_bytes([src[0], src[1]])) / full,
            (Self::Int24, Some(full)) => {
                f64::from(i32::from_le_bytes([0, src[0], src[1], src[2]]) >> 8) / full
            }
            (_, Some(full)) => f64::from(i32::from_le_bytes(word(src))) / full,
        }
    }
}

/// A sample as it is written: clamped to ±1.0, a non-finite one silence.
pub fn clean(x: f64) -> f64 {
    if x.is_finite() {
        x.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// Why a driver whose sample type is `code` is refused.
pub fn unsupported_sample_text(code: i32) -> String {
    format!(
        "the driver's sample type {code} is not supported (Int16/24/32LSB, Float32LSB, \
         Int32LSB16-24)"
    )
}

#[cfg(test)]
mod tests;
