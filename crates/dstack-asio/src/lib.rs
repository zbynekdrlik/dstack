// SPDX-License-Identifier: MIT OR Apache-2.0

//! The ASIO host for our apps (dstack #243): a local ASIO driver opened as a
//! host, every buffer switch running the app's [`Process`] (the contract
//! dstack's pump runs too), and the two sides of a Dante <-> ASIO bridge.
//!
//! Lifted from SongPlayer's ASIO output (songplayer #233, #229: its
//! `asio_win`, `asio_format`, `asio_state` and `asio_hold`), so SongPlayer's
//! ASIO outputs and dasio's bridge instance run one host. The owner's
//! ROZHODNUTÉ of 13.11.2026 on dstack #243 asked for it.
//!
//! - **The driver:** opened with azo 0.4.0 (a pure-Rust COM ASIO host, MIT,
//!   no Steinberg SDK), on one worker thread per driver that holds a COM
//!   STA, pumps the thread's window messages and makes every driver call.
//!   Its rate, preferred buffer and sample types are read, never set
//!   ([`device`]); one holder per driver per process ([`hold`]).
//! - **The buffer switch** ([`Engine`]) runs on the driver's own thread:
//!   decode the used inputs into channel-major `f64`, run the app's
//!   `Process`, encode its outputs into the chosen driver outputs and zero
//!   the rest ([`format`](mod@format), SongPlayer's exact encode). It allocates nothing,
//!   takes no lock, logs nothing and makes no system call; a panicking
//!   `Process` plays silence and is counted.
//! - **The lifecycle** ([`Runner`], decisions in [`state`]): reopen backoff
//!   2 / 10 / 30 / 60 s; close on a reset, a rate change, a stall; a driver
//!   with no clock or one far off its reported rate is a calm wait.
//! - **The bridge** ([`bridge`]): Dante -> ASIO and ASIO -> Dante, each
//!   through `dlink::bridge`'s FIFO and ASRC player at the #247 app buffer
//!   (the ASIO buffer + one pump block by default).
//!
//! Everything but the Windows glue (`win`, Windows only: the azo device,
//! the static callback slots, parking, the driver list) is pure and tested
//! on Linux.
//!
//! **A process that loads an ASIO driver spawns no child process.** A child
//! spawned from a process that has the Waves SoundGrid ASIO driver loaded
//! makes the driver resync (SongPlayer measured it, songplayer #233), which
//! is why SongPlayer hosts its ASIO outputs in a separate process that
//! spawns nothing.

pub mod bridge;
pub mod device;
pub mod engine;
pub mod format;
pub mod hold;
pub mod runner;
pub mod state;
#[cfg(windows)]
pub mod win;

pub use device::{Channels, Device, Opened, Started, Unsupported};
pub use dstack_audio_io::{Block, Process};
pub use engine::{Counters, Engine};
pub use format::Sample;
pub use runner::{MakeProcess, RunState, Runner, Status};
pub use state::{DeviceEvents, Reason};

/// The registered ASIO drivers' names (HKLM\SOFTWARE\ASIO, read only; no
/// driver is loaded). None registered → empty; off Windows → empty.
pub fn drivers() -> Result<Vec<String>, String> {
    #[cfg(windows)]
    {
        win::drivers()
    }
    #[cfg(not(windows))]
    {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    /// The driver list reads (Windows CI: the registry, an absent key is an
    /// empty list); off Windows it is empty.
    #[test]
    fn the_driver_list_reads() {
        let list = super::drivers().expect("the driver list reads");
        if cfg!(not(windows)) {
            assert!(list.is_empty());
        }
    }
}
