// SPDX-License-Identifier: GPL-3.0-or-later

//! A Dante device inside an application's own process (the owner's
//! connection model, 1.11.2026, dstack #20): every app carries its own
//! connector and appears on the Dante network as a device of its own,
//! under its own name; several on one PC is normal.
//!
//! [`Embedded::start`] is the whole life of such a device in one call:
//!
//! - the setup from a config the app built ([`host::prepare_config`]), with
//!   no control socket;
//! - serving with [`Signals::Host`]: no signal handler is registered, so
//!   the app keeps its own (inside OBS, a SIGTERM still ends OBS after our
//!   device stopped);
//! - the engine's pump ([`stream::spawn`]) on the media clock, running the
//!   app's [`Process`] every block: its inputs are the RX channels the
//!   [`Map`] names, its outputs the TX channels' engine rings, which those
//!   channels send ([`Source::Engine`], set here; a channel is silent at
//!   start). Its timing is [`Timing::of`] the config.
//!
//! [`Embedded::stop`] ends the pump, then the device (its mDNS goodbyes),
//! within a bound. Start and stop may repeat in one process (an OBS source
//! turned off and on); one device of a process sets the timing profile for
//! the others ([`crate::clock::set_profile`]).
//!
//! Call [`Embedded::start`] outside any tokio runtime: the device runs its
//! own on its `dante-device` thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use dante_audio_io::Process;
use tracing::{info, warn};

use crate::daemon::Place;
use crate::daemon::media::Media;
use crate::device::DeviceConfig;
use crate::host::{self, ClockSource, Host, HostError, Signals, Stopped};
use crate::media::source::Source;
use crate::stream::{self, Map, Pump, PumpStats, Timing};

/// A device running inside the app: serving, and its pump running the
/// app's processor.
pub struct Embedded {
    host: Host,
    media: Arc<Media>,
    stop: Arc<AtomicBool>,
    pump: Option<JoinHandle<()>>,
}

impl Embedded {
    /// The device of `cfg` on `place`, serving, with `processor` run every
    /// block on the RX channels `map.rx` (inputs, from 0) into the TX
    /// channels `map.tx` (outputs). A channel past the device's is
    /// [`HostError::Setup`].
    pub fn start<P: Process + 'static>(
        cfg: DeviceConfig,
        place: &Place,
        map: Map,
        processor: P,
    ) -> Result<Embedded, HostError> {
        Self::start_with(cfg, place, map, processor, ClockSource::Dantesync)
    }

    /// [`Embedded::start`] on the clock `source` ([`ClockSource::System`]
    /// is for tests only).
    pub fn start_with<P: Process + 'static>(
        cfg: DeviceConfig,
        place: &Place,
        map: Map,
        processor: P,
        source: ClockSource,
    ) -> Result<Embedded, HostError> {
        if let Some(&rx) = map.rx.iter().find(|&&rx| rx >= cfg.rx.len()) {
            return Err(HostError::Setup(format!(
                "the app's RX channel {} is past the device's {} RX channels",
                rx + 1,
                cfg.rx.len()
            )));
        }
        let timing = Timing::of(&cfg);
        let bits = cfg.encoding;
        let rate = cfg.sample_rate;
        let prepared = host::prepare_config(cfg, place, None, &map.tx, source)?;
        let media = prepared.media();
        // Every TX channel is silent at start: the app's outputs go out as
        // their channels' engine rings (each has one: `prepare_config`
        // made a ring on every channel of `map.tx`).
        for &tx in &map.tx {
            media.tx.set_source(tx, Source::Engine);
        }
        let stats = Arc::new(PumpStats::new(timing, rate));
        media.attach_engine(Arc::clone(&stats));
        let pump = Pump::new(
            processor,
            map,
            timing,
            bits,
            Arc::clone(&media.rx),
            Arc::clone(&media.tx),
            stats,
        );
        let host = prepared.start(Signals::Host)?;
        let stop = Arc::new(AtomicBool::new(false));
        let pump = match stream::spawn(Arc::clone(&media.clock), pump, Arc::clone(&stop)) {
            Ok(thread) => thread,
            Err(e) => {
                let stopped = host.stop(STOP_WAIT);
                warn!(
                    ?stopped,
                    "the embedded device stopped: its pump did not start"
                );
                return Err(HostError::Run(format!("the engine's pump thread: {e}")));
            }
        };
        info!(
            block = timing.block,
            playout = timing.playout,
            margin = timing.margin,
            "embedded device serving, its pump running"
        );
        Ok(Embedded {
            host,
            media,
            stop,
            pump: Some(pump),
        })
    }

    /// The device's media side: its status for the app (the pump's
    /// numbers through [`Media::engine`], the RX and TX counts).
    pub fn media(&self) -> &Arc<Media> {
        &self.media
    }

    /// Why serving ended by itself, if it did ([`Host::ended`]).
    pub fn ended(&self) -> Option<String> {
        self.host.ended()
    }

    /// Ends the pump (within one block), then the device: its serving and
    /// its goodbyes, waited for `within` ([`Host::stop`]).
    pub fn stop(mut self, within: Duration) -> Stopped {
        self.stop.store(true, Ordering::Release);
        if let Some(pump) = self.pump.take()
            && pump.join().is_err()
        {
            warn!("the embedded device's pump thread panicked");
        }
        self.host.stop(within)
    }
}

/// The bound on the device's stop after its pump failed to start.
const STOP_WAIT: Duration = Duration::from_secs(5);
