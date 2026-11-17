// SPDX-License-Identifier: MIT OR Apache-2.0

//! A [`Link`] whose TX channels carry the app's audio through a
//! [`bridge::TxPull`] run by the device's TX thread (dstack #363), and the
//! launch that hands it over ([`TxPulled`]).

use std::sync::Arc;

use dstack::daemon::{Place, PlaceWait};
use dstack::device::DeviceConfig;
use dstack::embed::Embedded;
use dstack::host::{ClockSource, HostError};
use dstack::media::bypass::FrameSender;
use dstack::stream::Map;
use dstack_audio_io::Process;

use crate::pulling::Launch;
use crate::{Config, Link, Rated, bridge};

/// The TX thread fills the TX channels `tx` from `pull`
/// ([`Embedded::start_tx_pulling`]); behind `Rated`'s TX converter when
/// the app's rate is not the device's (`rates`: the device's and the
/// app's).
pub(crate) struct TxPulled {
    tx: Vec<usize>,
    pull: bridge::TxPull,
    rates: Option<(u32, u32)>,
}

impl Launch for TxPulled {
    type Out = ();

    fn launch<P: Process + 'static>(
        self,
        cfg: DeviceConfig,
        place: &Place,
        wait: PlaceWait<'_>,
        map: Map,
        processor: P,
        source: ClockSource,
        sender: Option<Arc<dyn FrameSender>>,
    ) -> Result<(Embedded, ()), HostError> {
        let outputs = self.tx.len();
        let fill: Box<dyn Process> = match self.rates {
            // The same TX converter (and prime) as on the pump's path, so
            // the stated (c) TX stays what it was: `Rated` with no inputs.
            Some((device, app)) => {
                let block = usize::try_from(cfg.block).unwrap_or(usize::MAX);
                Box::new(
                    Rated::new(self.pull, device, app, block, 0, outputs).map_err(|e| {
                        HostError::Setup(format!("dlink rate conversion (TX pull): {e}"))
                    })?,
                )
            }
            None => Box::new(self.pull),
        };
        Embedded::start_tx_pulling(
            cfg,
            place,
            wait,
            map,
            processor,
            source,
            sender,
            (self.tx, fill),
        )
        .map(|device| (device, ()))
    }
}

impl Link {
    /// [`Link::start`] whose TX channels `map.tx` carry the app's audio
    /// through `pull` ([`bridge::TxPull`]: the [`bridge::Player`] of the
    /// app's FIFO, pull output `i` on TX channel `map.tx[i]`), run by the
    /// device's TX thread (dstack #363): it fills each block at the pump's
    /// own due time and stamp just before it sends, so a late pump no
    /// longer leaves the sent stamps silent, with the same latency (the
    /// same D, W, P, app buffer and TX converter). The player's output
    /// rate is the app's ([`Config::sample_rate`]); a device that converts
    /// runs the pull behind its TX converter, as the pump's path does.
    ///
    /// The pump still runs `processor` every block, on the RX channels
    /// `map.rx` and no TX channel ([`bridge::Idle`] for an app with no RX).
    /// A TX computed from RX in the same block (a mixer) cannot be pulled:
    /// its audio does not exist before the block runs; it keeps
    /// [`Link::start`].
    ///
    /// A pull whose outputs are not `map.tx`'s or a TX channel past the
    /// device's is [`HostError::Setup`]; every other error as
    /// [`Link::start`].
    pub fn start_tx_pulling<P: Process + 'static>(
        config: &Config,
        place: &Place,
        map: Map,
        processor: P,
        pull: bridge::TxPull,
    ) -> Result<Link, HostError> {
        Self::start_tx_pulling_on(
            config,
            place,
            map,
            processor,
            pull,
            config.clock(ClockSource::Linked),
        )
    }

    /// [`Link::start_tx_pulling`] on the clock `source` (as
    /// [`Link::start_on`]).
    pub fn start_tx_pulling_on<P: Process + 'static>(
        config: &Config,
        place: &Place,
        map: Map,
        processor: P,
        pull: bridge::TxPull,
        source: ClockSource,
    ) -> Result<Link, HostError> {
        bridge::tx_pull::check(&map.tx, config.tx.len(), pull.outputs())
            .map_err(HostError::Setup)?;
        let launch = TxPulled {
            tx: map.tx,
            pull,
            rates: config
                .converts()
                .then(|| (config.device_rate(), config.sample_rate)),
        };
        let pump = Map {
            rx: map.rx,
            tx: Vec::new(),
        };
        Self::start_device(
            launch,
            config,
            place,
            PlaceWait::Now,
            pump,
            processor,
            source,
        )
        .map(|(link, ())| link)
    }
}
