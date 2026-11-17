// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

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
//!   start). Its timing is [`Timing::of`] the device as it runs (a
//!   latency Dante Controller set included, followed live).
//!
//! [`Embedded::stop`] ends the pump, then the device (its mDNS goodbyes),
//! within a bound. Start and stop may repeat in one process (an OBS source
//! turned off and on); one device of a process sets the timing profile for
//! the others ([`crate::clock::set_profile`]).
//!
//! Call [`Embedded::start`] outside any tokio runtime: the device runs its
//! own on its `dstack-device` thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use dstack_audio_io::Process;
use tracing::{info, warn};

use crate::daemon::media::Media;
use crate::daemon::{Place, PlaceWait};
use crate::device::DeviceConfig;
use crate::host::{self, ClockSource, Host, HostError, Signals, Stopped};
use crate::media::bypass::FrameSender;
use crate::media::rx::Intake;
use crate::media::source::Source;
use crate::media::tx::{TxFill, engine_channels};
use crate::stream::{self, Map, Pump, PumpStats, Timing};

/// A device running inside the app: serving, and its pump running the
/// app's processor.
pub struct Embedded {
    host: Host,
    media: Arc<Media>,
    stop: Arc<AtomicBool>,
    pump: Option<JoinHandle<()>>,
    timing: Timing,
}

impl Embedded {
    /// The device of `cfg` on `place`, serving, with `processor` run every
    /// block on the RX channels `map.rx` (inputs, from 0) into the TX
    /// channels `map.tx` (outputs). A channel past the device's is
    /// [`HostError::Setup`].
    ///
    /// An address or interface not there yet (the network still coming up
    /// at boot or logon) is waited for with no time limit: the call blocks
    /// until it is there, logging the wait once a minute (dstack #225). To
    /// fail at once or stop the wait, use [`Embedded::start_waiting`].
    pub fn start<P: Process + 'static>(
        cfg: DeviceConfig,
        place: &Place,
        map: Map,
        processor: P,
    ) -> Result<Embedded, HostError> {
        Self::start_with(cfg, place, map, processor, ClockSource::default())
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
        Self::start_with_sender(cfg, place, map, processor, source, None)
    }

    /// [`Embedded::start_with`], the TX sent through the host's
    /// `frame_sender` when one is given ([`Media::set_frame_sender`]:
    /// Windows only, set before the device's threads start).
    pub fn start_with_sender<P: Process + 'static>(
        cfg: DeviceConfig,
        place: &Place,
        map: Map,
        processor: P,
        source: ClockSource,
        frame_sender: Option<Arc<dyn FrameSender>>,
    ) -> Result<Embedded, HostError> {
        Self::start_waiting(
            cfg,
            place,
            PlaceWait::Forever,
            map,
            processor,
            source,
            frame_sender,
        )
    }

    /// [`Embedded::start_with_sender`] with a place not there yet met as
    /// `wait` says (dstack #230): [`PlaceWait::Now`] fails at once,
    /// [`PlaceWait::Until`] waits until its flag is set; the other starts
    /// wait for it with no time limit ([`PlaceWait::Forever`]).
    pub fn start_waiting<P: Process + 'static>(
        cfg: DeviceConfig,
        place: &Place,
        wait: PlaceWait<'_>,
        map: Map,
        processor: P,
        source: ClockSource,
        frame_sender: Option<Arc<dyn FrameSender>>,
    ) -> Result<Embedded, HostError> {
        let (device, _) = Self::launch(
            cfg,
            place,
            wait,
            map,
            processor,
            source,
            frame_sender,
            Pulls::default(),
        )?;
        Ok(device)
    }

    /// [`Embedded::start_waiting`] whose app reads the RX rings itself
    /// (dstack #307): the RX rings' writer ([`Intake`], claimed with
    /// [`crate::media::rx::RxShared::app_intake`] before the pump and the
    /// RX thread start, so neither writes them) comes back to the app,
    /// which drains it on the thread with the deadline just before it
    /// reads the rings by position (dlink's `bridge::RxPull`). The pump
    /// still runs `processor` every block on `map` (give it no RX inputs:
    /// the rings are read where the app drains them).
    #[allow(clippy::too_many_arguments)]
    pub fn start_pulling<P: Process + 'static>(
        cfg: DeviceConfig,
        place: &Place,
        wait: PlaceWait<'_>,
        map: Map,
        processor: P,
        source: ClockSource,
        frame_sender: Option<Arc<dyn FrameSender>>,
    ) -> Result<(Embedded, Intake), HostError> {
        let pulls = Pulls { rx: true, tx: None };
        Self::with_intake(Self::launch(
            cfg,
            place,
            wait,
            map,
            processor,
            source,
            frame_sender,
            pulls,
        )?)
    }

    /// [`Embedded::start_pulling`] (the app reads the RX rings itself)
    /// whose TX channels `pull.channels` the TX thread fills from
    /// `pull.processor` ([`Embedded::start_tx_pulling`]): a bridge pulled
    /// both ways (dstack #364, dstack-asio's to- and from-ASIO sides). The
    /// pump still runs `processor` every block on `map` (no RX inputs, no
    /// pulled TX channel).
    #[allow(clippy::too_many_arguments)]
    pub fn start_pulling_with_tx<P: Process + 'static>(
        cfg: DeviceConfig,
        place: &Place,
        wait: PlaceWait<'_>,
        map: Map,
        processor: P,
        source: ClockSource,
        frame_sender: Option<Arc<dyn FrameSender>>,
        pull: TxPullOf,
    ) -> Result<(Embedded, Intake), HostError> {
        let pulls = Pulls {
            rx: true,
            tx: Some(pull),
        };
        Self::with_intake(Self::launch(
            cfg,
            place,
            wait,
            map,
            processor,
            source,
            frame_sender,
            pulls,
        )?)
    }

    /// The started device with the RX rings' writer the app asked for, or
    /// the device stopped when another writer had them.
    fn with_intake(launched: (Embedded, Option<Intake>)) -> Result<(Embedded, Intake), HostError> {
        match launched {
            (device, Some(intake)) => Ok((device, intake)),
            (device, None) => {
                let stopped = device.stop(STOP_WAIT);
                warn!(?stopped, "the embedded device stopped: no RX intake");
                Err(HostError::Setup(
                    "the RX rings already have a writer: the app cannot pull them".to_string(),
                ))
            }
        }
    }

    /// [`Embedded::start_waiting`] whose app's TX channels `pull.channels`
    /// (from 0) carry what `pull.processor` makes (dstack #363): the TX thread runs it in
    /// whole blocks at the pump's due time and stamp, just before it sends
    /// ([`TxFill`], claimed with [`crate::media::tx::TxShared::claim_fill`]
    /// before the TX thread starts), so a late pump no longer leaves those
    /// channels' sent stamps empty. `pull.processor` has no inputs and one
    /// output a channel of `pull.channels`. The pump still runs
    /// `processor` every block on `map`, whose `map.tx` must not name a
    /// pulled channel (one writer a ring); a channel past the device's or
    /// named twice is [`HostError::Setup`].
    #[allow(clippy::too_many_arguments)]
    pub fn start_tx_pulling<P: Process + 'static>(
        cfg: DeviceConfig,
        place: &Place,
        wait: PlaceWait<'_>,
        map: Map,
        processor: P,
        source: ClockSource,
        frame_sender: Option<Arc<dyn FrameSender>>,
        pull: TxPullOf,
    ) -> Result<Embedded, HostError> {
        let pulls = Pulls {
            rx: false,
            tx: Some(pull),
        };
        let (device, _) = Self::launch(
            cfg,
            place,
            wait,
            map,
            processor,
            source,
            frame_sender,
            pulls,
        )?;
        Ok(device)
    }

    /// The start: the device, its pump, the RX rings' writer for the app
    /// when `pulls` asks for it ([`Embedded::start_pulling`]) and the TX
    /// thread's fill ([`Embedded::start_tx_pulling`]).
    #[allow(clippy::too_many_arguments)]
    fn launch<P: Process + 'static>(
        cfg: DeviceConfig,
        place: &Place,
        wait: PlaceWait<'_>,
        map: Map,
        processor: P,
        source: ClockSource,
        frame_sender: Option<Arc<dyn FrameSender>>,
        pulls: Pulls,
    ) -> Result<(Embedded, Option<Intake>), HostError> {
        if let Some(&rx) = map.rx.iter().find(|&&rx| rx >= cfg.rx.len()) {
            return Err(HostError::Setup(format!(
                "the app's RX channel {} is past the device's {} RX channels",
                rx + 1,
                cfg.rx.len()
            )));
        }
        let pulled: &[usize] = pulls.tx.as_ref().map_or(&[], |p| &p.channels);
        let engine = engine_channels(&map.tx, pulled, cfg.tx.len()).map_err(HostError::Setup)?;
        let prepared = host::prepare_config_waiting(cfg, place, wait, None, &engine, source)?;
        // The device as it runs: a latency Dante Controller set holds over
        // the config's, and the RX plays at it.
        let timing = Timing::of(prepared.config());
        let bits = prepared.config().encoding;
        let rate = prepared.config().sample_rate;
        let media = prepared.media();
        if let Some(sender) = frame_sender {
            media.set_frame_sender(sender);
        }
        // Every TX channel is silent at start: the app's outputs go out as
        // their channels' engine rings (each has one: `prepare_config`
        // made a ring on every channel of `map.tx` and of the TX pull).
        for &tx in &engine {
            media.tx.set_source(tx, Source::Engine);
        }
        // dstack #363: the TX pull's claim comes before the TX thread's
        // start (the host's, below).
        let tx_pull = pulls.tx.is_some();
        if let Some(pull) = pulls.tx {
            let fill = TxFill::new(
                pull.processor,
                pull.channels,
                timing,
                bits,
                Arc::clone(&media.rx),
            );
            if let Err(refused) = media.tx.claim_fill(fill) {
                return Err(HostError::Setup(format!(
                    "the app's TX pull: {}",
                    refused.reason
                )));
            }
        }
        let stats = Arc::new(PumpStats::new(timing, rate));
        media.attach_engine(Arc::clone(&stats));
        // dstack #307: the app's claim comes before the pump's (here) and
        // the RX thread's (at the host's start).
        let intake = if pulls.rx {
            media.rx.app_intake()
        } else {
            None
        };
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
            app_reads_rx = intake.is_some(),
            tx_thread_fills = tx_pull,
            "embedded device serving, its pump running"
        );
        Ok((
            Embedded {
                host,
                media,
                stop,
                pump: Some(pump),
                timing,
            },
            intake,
        ))
    }

    /// The pump's timing at the start ([`Timing::of`] the device as it
    /// runs); a latency Dante Controller sets later moves the playout
    /// point ([`crate::media::rx::RxShared::playout`]).
    pub fn timing(&self) -> Timing {
        self.timing
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

/// What the app pulls itself: the RX rings (`rx`, [`Embedded::start_pulling`])
/// and TX channels the TX thread fills (`tx`, [`Embedded::start_tx_pulling`]).
#[derive(Default)]
struct Pulls {
    rx: bool,
    tx: Option<TxPullOf>,
}

/// An app's TX pull for [`Embedded::start_tx_pulling`] (dstack #363): its
/// `processor` (no inputs, one output a channel) fills the TX channels
/// `channels` (from 0) on the TX thread.
pub struct TxPullOf {
    pub channels: Vec<usize>,
    pub processor: Box<dyn Process>,
}

/// The bound on the device's stop after its pump failed to start.
const STOP_WAIT: Duration = Duration::from_secs(5);
