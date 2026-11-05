// SPDX-License-Identifier: MIT OR Apache-2.0

//! Our channels on the output's (dstack #139): the output device has its
//! own channel count (a stereo speaker, a 5.1 card, a mono monitor), which
//! need not be ours.
//!
//! - L/R (or our first channels in order) go to the first output channels;
//!   output channels past ours are silent.
//! - A mono source goes to the first two output channels (both speakers).
//! - A mono output gets L and R mixed at half each (neither side lost).

/// Where an output channel's samples come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Our channel (from 0).
    Channel(usize),
    /// Our first two channels at half each (a mono output).
    Mix,
    /// Nothing: silence.
    Silence,
}

/// The source of output channel `out` (from 0) on an output with
/// `outputs` channels, from our `inputs` channels.
pub fn source(out: usize, outputs: usize, inputs: usize) -> Source {
    if inputs == 0 || out >= outputs {
        Source::Silence
    } else if inputs == 1 {
        if out < 2 {
            Source::Channel(0)
        } else {
            Source::Silence
        }
    } else if outputs == 1 {
        Source::Mix
    } else if out < inputs {
        Source::Channel(out)
    } else {
        Source::Silence
    }
}

/// The sample of output channel `out`'s `source` at frame `frame` of a
/// channel-major block of `frames` frames.
pub fn sample(source: Source, block: &[f64], frames: usize, frame: usize) -> f64 {
    match source {
        Source::Channel(c) => block[c * frames + frame],
        Source::Mix => 0.5 * (block[frame] + block[frames + frame]),
        Source::Silence => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stereo_goes_to_the_first_two_outputs_and_the_rest_is_silent() {
        let six: Vec<Source> = (0..6).map(|o| source(o, 6, 2)).collect();
        assert_eq!(
            six,
            [
                Source::Channel(0),
                Source::Channel(1),
                Source::Silence,
                Source::Silence,
                Source::Silence,
                Source::Silence
            ]
        );
        assert_eq!(source(0, 2, 2), Source::Channel(0));
        assert_eq!(source(1, 2, 2), Source::Channel(1));
    }

    #[test]
    fn mono_is_duplicated_on_the_first_two_outputs() {
        assert_eq!(source(0, 2, 1), Source::Channel(0));
        assert_eq!(source(1, 2, 1), Source::Channel(0));
        assert_eq!(source(2, 4, 1), Source::Silence);
        assert_eq!(source(0, 1, 1), Source::Channel(0));
    }

    #[test]
    fn a_mono_output_mixes_l_and_r_at_half_each() {
        assert_eq!(source(0, 1, 2), Source::Mix);
        assert_eq!(source(0, 1, 4), Source::Mix);
        // L = 0.6, R = 0.2 at frame 1 of 2.
        let block = [0.0, 0.6, 0.0, 0.2];
        assert!((sample(Source::Mix, &block, 2, 1) - 0.4).abs() < 1e-12);
    }

    #[test]
    fn more_channels_go_in_order_as_far_as_the_output_has_them() {
        let got: Vec<Source> = (0..3).map(|o| source(o, 3, 4)).collect();
        assert_eq!(
            got,
            [Source::Channel(0), Source::Channel(1), Source::Channel(2)]
        );
        assert_eq!(source(3, 4, 3), Source::Silence);
    }

    #[test]
    fn no_inputs_or_an_output_past_the_count_is_silence() {
        assert_eq!(source(0, 2, 0), Source::Silence);
        assert_eq!(source(2, 2, 2), Source::Silence);
        assert_eq!(source(1, 1, 1), Source::Silence);
        assert_eq!(sample(Source::Silence, &[0.7, 0.7], 1, 0), 0.0);
    }

    #[test]
    fn a_channel_reads_its_own_row_of_the_block() {
        // 2 channels of 3 frames: L = 1 2 3, R = 4 5 6.
        let block = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        assert_eq!(sample(Source::Channel(0), &block, 3, 2), 3.0);
        assert_eq!(sample(Source::Channel(1), &block, 3, 0), 4.0);
        assert_eq!(sample(Source::Channel(1), &block, 3, 2), 6.0);
    }
}
