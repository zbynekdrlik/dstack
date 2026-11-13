// SPDX-License-Identifier: MIT OR Apache-2.0

//! The dlink side (dstack #164): the device's processor, run by its pump
//! every block, fills the TX channels (L, R) from the captured output's
//! FIFO through the [`Player`] (the converter steered on the FIFO's depth,
//! [`dstack_endpoint::player`]). The pump calls it on the network's clock,
//! so the player's "output" is the network here and its "network" (the
//! FIFO's producer) the capture.
//!
//! Another output chosen in dtray's menu brings another rate and channel
//! count, so another FIFO and player. The main thread makes them and hands
//! the player over through a [`Switch`] (`dlink::bridge::switch`, shared
//! with dstack-asio's bridge): a bounded channel of one, which the pump
//! reads with `try_recv` (no allocation, no lock); the player it replaces
//! goes back through a second one, so it is freed on the main thread
//! ([`Switch::collect`]), never in the pump.
//!
//! [`Source::process`] allocates nothing, takes no lock and makes no system
//! call (the pump's rules, I7).

use dlink::bridge::{Taker, switch};
use dlink::{Block, Process};
use dstack_endpoint::player::Player;

/// dsend's TX channels: L and R.
pub const TX_CHANNELS: usize = 2;
/// The TX channels' names.
pub const TX_NAMES: [&str; TX_CHANNELS] = ["L", "R"];
/// The most frames one player call fills; a longer block takes several.
pub const CHUNK: usize = 1024;
pub use dlink::bridge::switch::{GONE, NOT_TAKEN, RETIRED};

/// The main thread's end: hands a new player to the pump and frees the
/// ones it replaced ([`dlink::bridge::Switch`]).
pub type Switch = dlink::bridge::Switch<Player>;

/// The device's processor (module doc).
pub struct Source {
    player: Player,
    taker: Taker<Player>,
    /// One player call's frames, interleaved.
    scratch: Vec<f64>,
}

/// The processor playing `first`, and its [`Switch`].
pub fn source(first: Player) -> (Source, Switch) {
    let (taker, switch) = switch();
    (
        Source {
            player: first,
            taker,
            scratch: vec![0.0; CHUNK * TX_CHANNELS],
        },
        switch,
    )
}

impl Process for Source {
    fn process(&mut self, block: &mut Block<'_>) {
        self.taker.take(&mut self.player);
        let frames = block.frames();
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(CHUNK);
            let buf = &mut self.scratch[..n * TX_CHANNELS];
            self.player.fill_with(buf, TX_CHANNELS, |x| x);
            for c in 0..TX_CHANNELS {
                // A channel the block lacks (fewer TX mapped) is skipped.
                let out = block.output(c);
                for (f, slot) in out.iter_mut().skip(done).take(n).enumerate() {
                    *slot = buf[f * TX_CHANNELS + c];
                }
            }
            done += n;
        }
    }
}

#[cfg(test)]
mod tests;
