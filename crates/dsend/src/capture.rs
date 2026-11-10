// SPDX-License-Identifier: MIT OR Apache-2.0

//! The capture side (dstack #164): the loopback callback hands an
//! interleaved packet of the endpoint's channels; it goes into the FIFO
//! frame by frame. It allocates nothing and never waits (the driver's
//! thread); a full FIFO drops the packet's tail and counts it.

use dstack_endpoint::fifo::Fifo;

/// Writes the interleaved `data` of `channels` channels into `fifo`, each
/// sample made an `f64` by `to` (the driver's sample format). A FIFO
/// channel the packet lacks reads as silence; a partial last frame is
/// left out. Returns the frames written.
pub fn push_interleaved<T: Copy>(
    fifo: &Fifo,
    data: &[T],
    channels: usize,
    to: impl Fn(T) -> f64,
) -> usize {
    let Some(frames) = data.len().checked_div(channels) else {
        return 0;
    };
    fifo.push_with(frames, |f, c| {
        if c < channels {
            to(data[f * channels + c])
        } else {
            0.0
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(fifo: &Fifo) -> Vec<f64> {
        let n = fifo.len();
        let mut out = vec![0.0; n * fifo.channels()];
        assert!(fifo.pop_planar(&mut out, n));
        out
    }

    #[test]
    fn an_interleaved_packet_goes_in_frame_by_frame() {
        let fifo = Fifo::new(2, 16);
        // L = 0.1 0.2 0.3, R = -0.1 -0.2 -0.3, interleaved.
        let data = [0.1f32, -0.1, 0.2, -0.2, 0.3, -0.3];
        assert_eq!(push_interleaved(&fifo, &data, 2, f64::from), 3);
        let want = [0.1f32, 0.2, 0.3, -0.1, -0.2, -0.3].map(f64::from);
        assert_eq!(all(&fifo), want);
    }

    #[test]
    fn the_sample_format_is_converted_by_the_caller_s_function() {
        let fifo = Fifo::new(1, 8);
        let data = [16_384i16, -32_768];
        push_interleaved(&fifo, &data, 1, |x| f64::from(x) / 32_768.0);
        assert_eq!(all(&fifo), [0.5, -1.0]);
    }

    #[test]
    fn a_fifo_wider_than_the_packet_fills_silence_and_a_partial_frame_is_dropped() {
        let fifo = Fifo::new(2, 8);
        assert_eq!(
            push_interleaved(&fifo, &[0.5f32, 0.25, 0.75], 1, f64::from),
            3
        );
        assert_eq!(all(&fifo), [0.5, 0.25, 0.75, 0.0, 0.0, 0.0]);
        // 5 samples of 2 channels: 2 whole frames.
        let wide = Fifo::new(2, 8);
        assert_eq!(push_interleaved(&wide, &[1.0f32; 5], 2, f64::from), 2);
        assert_eq!(wide.arrived(), 2);
    }

    #[test]
    fn zero_channels_write_nothing() {
        let fifo = Fifo::new(2, 8);
        assert_eq!(push_interleaved(&fifo, &[1.0f32; 4], 0, f64::from), 0);
        assert_eq!(fifo.arrived(), 0);
    }
}
