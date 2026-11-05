// SPDX-License-Identifier: MIT OR Apache-2.0

//! The dlink side (dstack #139): the device's processor, run by its pump
//! every block on the RX channels, writes the block into the [`Fifo`]. It
//! allocates nothing and never waits (the pump's rules, I7); a full FIFO
//! drops the block's tail and counts it.

use std::sync::Arc;

use dlink::{Block, Process};

use crate::fifo::Fifo;

/// The device's processor (module doc).
pub struct Feed {
    fifo: Arc<Fifo>,
}

impl Feed {
    pub fn new(fifo: Arc<Fifo>) -> Self {
        Feed { fifo }
    }
}

impl Process for Feed {
    fn process(&mut self, block: &mut Block<'_>) {
        let block = &*block;
        // A channel the block lacks reads as silence.
        self.fifo.push_with(block.frames(), |f, c| {
            block.input(c).get(f).copied().unwrap_or(0.0)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_block_goes_into_the_fifo_frame_by_frame() {
        let fifo = Arc::new(Fifo::new(2, 16));
        let mut feed = Feed::new(Arc::clone(&fifo));
        // L = 0.1 0.2 0.3, R = -0.1 -0.2 -0.3 (channel-major).
        let input = [0.1, 0.2, 0.3, -0.1, -0.2, -0.3];
        let mut output: [f64; 0] = [];
        feed.process(&mut Block::new(3, &input, &mut output));
        assert_eq!(fifo.len(), 3);
        let mut out = [0.0; 6];
        assert!(fifo.pop_planar(&mut out, 3));
        let want = input.map(|x: f64| f64::from(x as f32));
        assert_eq!(out, want);
    }

    #[test]
    fn a_block_with_fewer_channels_fills_the_rest_with_silence() {
        let fifo = Arc::new(Fifo::new(2, 16));
        let mut feed = Feed::new(Arc::clone(&fifo));
        let input = [0.5, 0.25];
        let mut output: [f64; 0] = [];
        feed.process(&mut Block::new(2, &input, &mut output));
        let mut out = [9.0; 4];
        assert!(fifo.pop_planar(&mut out, 2));
        assert_eq!(out, [0.5, 0.25, 0.0, 0.0]);
    }
}
