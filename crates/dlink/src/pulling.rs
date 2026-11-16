// SPDX-License-Identifier: MIT OR Apache-2.0

//! A [`Link`] whose app pulls its RX channels on its own thread with the
//! deadline (dstack #307, [`bridge::RxPull`]), and the two ways the start
//! launches the embedded device: the pump writing the RX rings
//! ([`Pushed`], every other start) or the app ([`Pulled`]).

use std::sync::Arc;

use dstack::daemon::{Place, PlaceWait};
use dstack::device::DeviceConfig;
use dstack::embed::Embedded;
use dstack::host::{ClockSource, HostError};
use dstack::media::bypass::FrameSender;
use dstack::media::rx::Intake;
use dstack::stream::Map;
use dstack_audio_io::Process;

use crate::{Config, Link, bridge};

/// How a start launches its embedded device, and what else it hands the
/// app.
pub(crate) trait Launch {
    /// What the app gets beside the device.
    type Out;

    /// The embedded device of `cfg` (as [`Embedded::start_waiting`]).
    fn launch<P: Process + 'static>(
        cfg: DeviceConfig,
        place: &Place,
        wait: PlaceWait<'_>,
        map: Map,
        processor: P,
        source: ClockSource,
        sender: Option<Arc<dyn FrameSender>>,
    ) -> Result<(Embedded, Self::Out), HostError>;
}

/// The pump (or the RX thread) writes the RX rings: [`Link::start`] and
/// its kin.
pub(crate) struct Pushed;

impl Launch for Pushed {
    type Out = ();

    fn launch<P: Process + 'static>(
        cfg: DeviceConfig,
        place: &Place,
        wait: PlaceWait<'_>,
        map: Map,
        processor: P,
        source: ClockSource,
        sender: Option<Arc<dyn FrameSender>>,
    ) -> Result<(Embedded, ()), HostError> {
        Embedded::start_waiting(cfg, place, wait, map, processor, source, sender)
            .map(|device| (device, ()))
    }
}

/// The app writes the RX rings ([`Embedded::start_pulling`]): it gets
/// their writer.
pub(crate) struct Pulled;

impl Launch for Pulled {
    type Out = Intake;

    fn launch<P: Process + 'static>(
        cfg: DeviceConfig,
        place: &Place,
        wait: PlaceWait<'_>,
        map: Map,
        processor: P,
        source: ClockSource,
        sender: Option<Arc<dyn FrameSender>>,
    ) -> Result<(Embedded, Intake), HostError> {
        Embedded::start_pulling(cfg, place, wait, map, processor, source, sender)
    }
}

impl Link {
    /// [`Link::start`] whose RX channels `map.rx` reach the app through
    /// the bridge `fifo`, pulled by the app's own thread with the deadline
    /// (an output callback; dstack #307): the [`bridge::RxPull`] it
    /// returns drains the flow sockets into the RX rings and reads them by
    /// position, at the media clock's now less the device's latency P (the
    /// pump's playout point), into `fifo` (FIFO channel `i` is RX channel
    /// `map.rx[i]`, at [`Config::device_rate`]: make it with
    /// [`bridge::fifo_for`]). Call [`bridge::RxPull::pull`] at the top of
    /// each callback, before [`bridge::Player::fill_with`]. The pump runs
    /// `processor` every block on no RX inputs (they are the app's now)
    /// and the TX channels `map.tx`, so a late pump no longer empties the
    /// bridge, with the same latency (the same P and bridge target).
    ///
    /// The pull is the RX rings' only writer, so start calling it at once:
    /// the device hears a flow's audio only through it. While nothing
    /// pulls (no output open, a stopped callback), the packets wait unread,
    /// no keepalive goes to the transmitter, and the flow reads silent: the
    /// channel stays `InProgress` instead of `Receiving`, and a granted flow
    /// silent for 2 s (once 6 s old) is closed and asked again. An app
    /// whose consumer may stop calls [`bridge::RxPull::drain_idle`] from
    /// another thread about once a second while
    /// [`bridge::pull::PullStats::pulls`] stands still (dlisten's main
    /// thread does); the flow then stays alive and the audio returns at the
    /// next pull.
    ///
    /// A FIFO whose channels are not `map.rx`'s or an RX channel past the
    /// device's is [`HostError::Setup`]; every other error as
    /// [`Link::start`].
    pub fn start_pulling<P: Process + 'static>(
        config: &Config,
        place: &Place,
        map: Map,
        processor: P,
        fifo: Arc<bridge::Fifo>,
    ) -> Result<(Link, bridge::RxPull), HostError> {
        Self::start_pulling_on(
            config,
            place,
            map,
            processor,
            fifo,
            config.clock(ClockSource::Linked),
        )
    }

    /// [`Link::start_pulling`] on the clock `source` (as [`Link::start_on`]).
    pub fn start_pulling_on<P: Process + 'static>(
        config: &Config,
        place: &Place,
        map: Map,
        processor: P,
        fifo: Arc<bridge::Fifo>,
        source: ClockSource,
    ) -> Result<(Link, bridge::RxPull), HostError> {
        bridge::pull::check(&map.rx, config.rx.len(), fifo.channels()).map_err(HostError::Setup)?;
        let pump = Map {
            rx: Vec::new(),
            tx: map.tx,
        };
        let (link, intake) = Self::start_device::<Pulled, P>(
            config,
            place,
            PlaceWait::Now,
            pump,
            processor,
            source,
        )?;
        let playout = link.device.timing().playout;
        let media = link.media();
        // Checked before the start (above): refused here only if the device
        // has fewer RX rings than its config's channels, never seen.
        match bridge::RxPull::new(
            intake,
            Arc::clone(&media.clock),
            Arc::clone(&media.rx),
            map.rx,
            fifo,
            playout,
            config.device_rate(),
        ) {
            Ok(pull) => Ok((link, pull)),
            Err(e) => {
                let stopped = link.stop();
                tracing::warn!(?stopped, "the pulling device stopped: {e}");
                Err(HostError::Setup(e))
            }
        }
    }
}
