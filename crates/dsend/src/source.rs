// SPDX-License-Identifier: MIT OR Apache-2.0

//! The dlink side (dstack #164, #363): the device's TX thread fills the TX
//! channels (L, R) from the captured output's FIFO through the [`Player`]
//! (the converter steered on the FIFO's depth,
//! [`dstack_endpoint::player`]), in dlink's [`TxPull`] handed to
//! `Link::start_tx_pulling`. It runs on the network's clock, so the
//! player's "output" is the network here and its "network" (the FIFO's
//! producer) the capture. Up to v0.6.162 the device's pump ran it, and a
//! pump block that ended late sent silence (dstack #346); the TX thread
//! fills each block just before it sends it.
//!
//! Another output chosen in dtray's menu brings another rate and channel
//! count, so another FIFO and player. The main thread makes them and hands
//! the player over through a [`Switch`] (`dlink::bridge::switch`, shared
//! with dstack-asio's bridge): a bounded channel of one, which the TX
//! thread reads with `try_recv` (no allocation, no lock); the player it
//! replaces goes back through a second one, so it is freed on the main
//! thread ([`Switch::collect`]), never in the TX thread.
//!
//! [`TxPull`]'s `process` allocates nothing, takes no lock and makes no
//! system call (the TX thread's rules).

use dlink::bridge::TxPull;
use dstack_endpoint::player::Player;

/// dsend's TX channels: L and R.
pub const TX_CHANNELS: usize = 2;
/// The TX channels' names.
pub const TX_NAMES: [&str; TX_CHANNELS] = ["L", "R"];
pub use dlink::bridge::switch::{GONE, NOT_TAKEN, RETIRED};
/// The most frames one player call fills; a longer block takes several.
pub use dlink::bridge::tx_pull::CHUNK;

/// The main thread's end: hands a new player to the TX thread and frees
/// the ones it replaced ([`dlink::bridge::Switch`]).
pub type Switch = dlink::bridge::Switch<Player>;

/// The device's TX pull (module doc).
pub type Source = TxPull;

/// The TX pull of L and R playing `first`, and its [`Switch`].
pub fn source(first: Player) -> (Source, Switch) {
    TxPull::switched(first, TX_CHANNELS)
}

#[cfg(test)]
mod tests;
