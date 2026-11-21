// SPDX-License-Identifier: MIT OR Apache-2.0

//! A [`Link`] whose TX channels carry the app's audio through a
//! [`bridge::TxPull`] (or an app's [`bridge::TxProcess`] around one) run
//! by the device's TX thread (dstack #363), alone or beside the app's RX
//! pull (dstack #364), and the launches that hand it over ([`TxPulled`],
//! [`BothPulled`]).

use std::sync::Arc;

use dstack::daemon::{Place, PlaceWait};
use dstack::device::DeviceConfig;
use dstack::embed::{Embedded, TxPullOf};
use dstack::host::{ClockSource, HostError};
use dstack::media::bypass::FrameSender;
use dstack::media::rx::Intake;
use dstack::stream::Map;
use dstack_audio_io::Process;

use crate::pulling::Launch;
use crate::{Config, Link, Rated, bridge};

/// The TX thread fills the TX channels `tx` from `pull`
/// ([`Embedded::start_tx_pulling`]); behind `Rated`'s TX converter when
/// the app's rate is not the device's (`rates`: the device's and the
/// app's).
pub(crate) struct TxPulled<T> {
    tx: Vec<usize>,
    pull: T,
    rates: Option<(u32, u32)>,
}

impl<T: bridge::TxProcess> TxPulled<T> {
    /// `pull` filling the TX channels `tx` of `config`'s device, with its
    /// rates when the app's is not the device's.
    fn new(config: &Config, tx: Vec<usize>, pull: T) -> Self {
        TxPulled {
            tx,
            pull,
            rates: config
                .converts()
                .then(|| (config.device_rate(), config.sample_rate)),
        }
    }

    /// The device's fill: the pull, behind `Rated` when it converts.
    fn fill(self, cfg: &DeviceConfig) -> Result<TxPullOf, HostError> {
        let outputs = self.tx.len();
        let processor: Box<dyn Process> = match self.rates {
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
        Ok(TxPullOf {
            channels: self.tx,
            processor,
        })
    }
}

impl<T: bridge::TxProcess> Launch for TxPulled<T> {
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
        let fill = self.fill(&cfg)?;
        Embedded::start_tx_pulling(cfg, place, wait, map, processor, source, sender, fill)
            .map(|device| (device, ()))
    }
}

/// The app pulls the RX rings and the TX thread fills the TX pull
/// ([`Embedded::start_pulling_with_tx`], dstack #364): the app gets the
/// RX rings' writer.
pub(crate) struct BothPulled<T>(TxPulled<T>);

impl<T: bridge::TxProcess> Launch for BothPulled<T> {
    type Out = Intake;
    const PULLS_RX: bool = true;

    fn launch<P: Process + 'static>(
        self,
        cfg: DeviceConfig,
        place: &Place,
        wait: PlaceWait<'_>,
        map: Map,
        processor: P,
        source: ClockSource,
        sender: Option<Arc<dyn FrameSender>>,
    ) -> Result<(Embedded, Intake), HostError> {
        let fill = self.0.fill(&cfg)?;
        Embedded::start_pulling_with_tx(cfg, place, wait, map, processor, source, sender, fill)
    }
}

impl Link {
    /// [`Link::start`] whose TX channels `map.tx` carry the app's audio
    /// through `pull` ([`bridge::TxPull`]: the [`bridge::Player`] of the
    /// app's FIFO, pull output `i` on TX channel `map.tx[i]`; or an app's
    /// [`bridge::TxProcess`] around one, dstack #364), run by the
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
    pub fn start_tx_pulling<P: Process + 'static, T: bridge::TxProcess>(
        config: &Config,
        place: &Place,
        map: Map,
        processor: P,
        pull: T,
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
    pub fn start_tx_pulling_on<P: Process + 'static, T: bridge::TxProcess>(
        config: &Config,
        place: &Place,
        map: Map,
        processor: P,
        pull: T,
        source: ClockSource,
    ) -> Result<Link, HostError> {
        bridge::tx_pull::check(&map.tx, config.tx.len(), pull.outputs())
            .map_err(HostError::Setup)?;
        let launch = TxPulled::new(config, map.tx, pull);
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

impl Link {
    /// [`Link::start_pulling`] (the app pulls the RX channels `map.rx`
    /// into `fifo`: the [`bridge::RxPull`] it returns) whose TX channels
    /// `map.tx` carry `pull`, run by the device's TX thread
    /// ([`Link::start_tx_pulling`]): a bridge pulled both ways (dstack
    /// #364, dstack-asio's to- and from-ASIO sides), with the same latency
    /// as either alone. The pump runs `processor` every block on no RX
    /// input and no TX channel. A FIFO or a pull that does not fit its
    /// channels is [`HostError::Setup`]; every other error as
    /// [`Link::start`].
    pub fn start_pulling_with_tx<P: Process + 'static, T: bridge::TxProcess>(
        config: &Config,
        place: &Place,
        map: Map,
        processor: P,
        fifo: Arc<bridge::Fifo>,
        pull: T,
    ) -> Result<(Link, bridge::RxPull), HostError> {
        Self::start_pulling_with_tx_on(
            config,
            place,
            map,
            processor,
            fifo,
            pull,
            config.clock(ClockSource::Linked),
        )
    }

    /// [`Link::start_pulling_with_tx`] on the clock `source` (as
    /// [`Link::start_on`]).
    pub fn start_pulling_with_tx_on<P: Process + 'static, T: bridge::TxProcess>(
        config: &Config,
        place: &Place,
        map: Map,
        processor: P,
        fifo: Arc<bridge::Fifo>,
        pull: T,
        source: ClockSource,
    ) -> Result<(Link, bridge::RxPull), HostError> {
        bridge::pull::check(&map.rx, config.rx.len(), fifo.channels()).map_err(HostError::Setup)?;
        bridge::tx_pull::check(&map.tx, config.tx.len(), pull.outputs())
            .map_err(HostError::Setup)?;
        let launch = BothPulled(TxPulled::new(config, map.tx, pull));
        let started = Self::start_device(
            launch,
            config,
            place,
            PlaceWait::Now,
            Map::default(),
            processor,
            source,
        )?;
        Self::rx_pull_of(started, config, map.rx, fifo)
    }
}

#[cfg(test)]
mod tests;
