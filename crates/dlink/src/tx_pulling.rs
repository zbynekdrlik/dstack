// SPDX-License-Identifier: MIT OR Apache-2.0

//! A [`Link`] whose TX channels carry the app's audio through a
//! [`bridge::TxPull`] (dstack #363).

use dstack::daemon::Place;
use dstack::host::{ClockSource, HostError};
use dstack::stream::Map;
use dstack_audio_io::{Block, Process};

use crate::{Config, Link, bridge};

/// The pull run by the pump, as every TX bridge is today (dstack #363's
/// RED: the TX thread's fill replaces it).
struct PumpFilled<P> {
    processor: P,
    pull: bridge::TxPull,
}

impl<P: Process> Process for PumpFilled<P> {
    fn process(&mut self, block: &mut Block<'_>) {
        let frames = block.frames();
        let mut none = Block::new(frames, &[], &mut []);
        if let Some(s) = block.start() {
            none = none.at(s);
        }
        self.processor.process(&mut none);
        self.pull.process(block);
    }

    fn discontinuity(&mut self) {
        self.processor.discontinuity();
        self.pull.discontinuity();
    }
}

impl Link {
    /// [`Link::start`] whose TX channels `map.tx` carry the app's audio
    /// from `pull` ([`bridge::TxPull`]).
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

    /// [`Link::start_tx_pulling`] on the clock `source`.
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
        if !map.rx.is_empty() {
            return Err(HostError::Setup("RX beside a TX pull: not yet".into()));
        }
        Self::start_on(config, place, map, PumpFilled { processor, pull }, source)
    }
}
