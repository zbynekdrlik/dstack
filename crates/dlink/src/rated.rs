// SPDX-License-Identifier: GPL-3.0-or-later

//! The app at its own rate (dstack #90, the owner's concept of 3.11.2026):
//! the device runs at the network's rate (96 kHz on the site, which every
//! transmitter there sends), and the app keeps its own (48 kHz for the
//! intercom and OBS, 44.1 kHz for a CD source). A Dante flow needs both
//! ends at one rate, so the conversion sits here, once, for every app.
//!
//! Both rates run on the device's one media clock, so the ratio is exact
//! and fixed: no drift to follow. RX goes through a [`Converter`] with a
//! fixed input (the device's block); the app gets what it gives, about
//! `block * ratio` frames, which varies by a frame around that for a ratio
//! that does not divide the block (44.1 kHz from 96 kHz). The app's
//! output goes into a small FIFO, from which the TX converter takes what
//! it needs for exactly the device's block. The FIFO starts with
//! [`PRIME`] frames of silence, so the frame-to-frame variation never runs
//! it dry; a dry FIFO plays silence and is counted.
//!
//! The filter is dstack-resample's sinc (THD+N above 120 dB). It delays RX
//! by half its length at the app's rate and TX by half at the network's
//! (2.7 ms and 1.3 ms at 48 and 96 kHz), plus the FIFO's [`PRIME`].
//!
//! `process` allocates nothing: every buffer is sized at the start.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dstack_audio_io::{Block, Process};
use dstack_resample::{ConvertError, Converter, Fixed};

/// The silence the TX FIFO starts with, app frames: more than the
/// frame-to-frame variation of the RX side can ever take away.
pub const PRIME: usize = 8;

/// Fills `dst` from the front of `fifo`, and the rest with silence;
/// returns the frames it had to fill with silence.
fn drain_into(fifo: &mut VecDeque<f64>, dst: &mut [f64]) -> usize {
    let have = fifo.len().min(dst.len());
    for (d, v) in dst.iter_mut().zip(fifo.drain(..have)) {
        *d = v;
    }
    dst[have..].fill(0.0);
    dst.len() - have
}

/// The app's processor at its own rate, behind the two converters.
pub struct Rated<P> {
    inner: P,
    rx: Converter,
    tx: Converter,
    block: usize,
    inputs: usize,
    outputs: usize,
    /// The device block's RX channels, channel-major at the block.
    staged: Vec<f64>,
    /// RX converter output, channel-major at the converter's room.
    rx_out: Vec<f64>,
    /// TX converter output, channel-major at the block.
    tx_out: Vec<f64>,
    /// The app's input and output, channel-major at this block's frames.
    app_in: Vec<f64>,
    app_out: Vec<f64>,
    /// The app's output waiting for the TX converter, per channel.
    fifo: Vec<VecDeque<f64>>,
    /// The TX converter's input, channel-major at its next frames.
    tx_in: Vec<f64>,
    /// The device's start of the last block and the app's.
    last: Option<(u64, u64, usize)>,
    /// TX FIFO runs dry, frames (shared with the app's status).
    dry: Arc<AtomicU64>,
}

impl<P: Process> Rated<P> {
    /// `inner` at `app_rate` on a device at `device_rate`, `block` device
    /// frames a block, `inputs` RX and `outputs` TX channels.
    pub fn new(
        inner: P,
        device_rate: u32,
        app_rate: u32,
        block: usize,
        inputs: usize,
        outputs: usize,
    ) -> Result<Self, ConvertError> {
        let rx = Converter::new(device_rate, app_rate, inputs.max(1), block, Fixed::Input)?;
        let tx = Converter::new(app_rate, device_rate, outputs.max(1), block, Fixed::Output)?;
        let rx_room = rx.output_frames_max();
        let fifo_room = 4 * (rx_room + tx.input_frames_max() + PRIME);
        let mut fifo: Vec<VecDeque<f64>> = (0..outputs)
            .map(|_| VecDeque::with_capacity(fifo_room))
            .collect();
        for f in &mut fifo {
            f.extend(std::iter::repeat_n(0.0, PRIME));
        }
        Ok(Rated {
            inner,
            block,
            inputs,
            outputs,
            staged: vec![0.0; inputs.max(1) * block],
            rx_out: vec![0.0; inputs.max(1) * rx_room],
            tx_out: vec![0.0; outputs.max(1) * block],
            app_in: vec![0.0; inputs * rx_room],
            app_out: vec![0.0; outputs * rx_room],
            tx_in: vec![0.0; outputs.max(1) * tx.input_frames_max()],
            fifo,
            rx,
            tx,
            last: None,
            dry: Arc::new(AtomicU64::new(0)),
        })
    }

    /// The TX FIFO's dry frames since the start (each played as silence).
    pub fn dry(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.dry)
    }

    /// The app's start for a device block at `start`: the last one's plus
    /// its frames when the device's blocks follow on, else the device's
    /// start at the app's rate.
    fn app_start(&self, start: u64) -> u64 {
        match self.last {
            Some((dev, app, frames)) if dev + self.block as u64 == start => app + frames as u64,
            _ => (start as f64 * self.rx.ratio()) as u64,
        }
    }
}

impl<P: Process> Process for Rated<P> {
    fn process(&mut self, block: &mut Block<'_>) {
        if block.frames() != self.block {
            // A device's block never changes size (dstack::stream::BLOCKS).
            block.zero_outputs();
            return;
        }
        let n = self.block;
        // RX: the device's block in, about n * ratio app frames out.
        let room = self.rx.output_frames_max();
        // No RX channels: the app's frames still follow the ratio, through
        // one channel of `staged` that stays silent (never written).
        for c in 0..self.inputs {
            self.staged[c * n..(c + 1) * n].copy_from_slice(block.input(c));
        }
        let lanes = self.inputs.max(1);
        let k = match self
            .rx
            .process(&self.staged[..lanes * n], n, &mut self.rx_out, room)
        {
            Ok((_, made)) => made,
            Err(_) => 0,
        };
        for c in 0..self.inputs {
            self.app_in[c * k..(c + 1) * k].copy_from_slice(&self.rx_out[c * room..c * room + k]);
        }
        // The app at its rate.
        let start = block.start();
        let app_start = start.map(|s| self.app_start(s));
        {
            let mut app = Block::new(
                k,
                &self.app_in[..self.inputs * k],
                &mut self.app_out[..self.outputs * k],
            );
            if let Some(s) = app_start {
                app = app.at(s);
            }
            self.inner.process(&mut app);
        }
        self.last = start.zip(app_start).map(|(s, a)| (s, a, k));
        // TX: the app's frames into the FIFO, exactly n device frames out.
        for (c, fifo) in self.fifo.iter_mut().enumerate() {
            fifo.extend(self.app_out[c * k..(c + 1) * k].iter().copied());
        }
        let need = self.tx.input_frames_next();
        for (c, fifo) in self.fifo.iter_mut().enumerate() {
            let short = drain_into(fifo, &mut self.tx_in[c * need..(c + 1) * need]);
            self.dry.fetch_add(short as u64, Ordering::Relaxed);
        }
        if self.outputs == 0 {
            return;
        }
        let ok = self
            .tx
            .process(
                &self.tx_in[..self.outputs * need],
                need,
                &mut self.tx_out,
                n,
            )
            .is_ok();
        for c in 0..self.outputs {
            let dst = block.output(c);
            if ok {
                dst.copy_from_slice(&self.tx_out[c * n..(c + 1) * n]);
            } else {
                dst.fill(0.0);
            }
        }
    }

    fn discontinuity(&mut self) {
        self.rx.reset();
        self.tx.reset();
        for f in &mut self.fifo {
            f.clear();
            f.extend(std::iter::repeat_n(0.0, PRIME));
        }
        self.last = None;
        self.inner.discontinuity();
    }
}

#[cfg(test)]
mod tests;
