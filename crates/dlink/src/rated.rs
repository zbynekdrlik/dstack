// SPDX-License-Identifier: MIT OR Apache-2.0

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
//! it needs for exactly the device's block. For a ratio that does not
//! divide the block the FIFO starts with [`PRIME`] frames of silence, so
//! the frame-to-frame variation never runs it dry; a dry FIFO plays
//! silence and is counted. An exact 2:1 or 1:2 gives TX exactly what it
//! takes every block, and starts with none.
//!
//! The converter is [`Converter::low_delay`] (dstack #1, 4.11.2026): an
//! exact 2:1 or 1:2 with an even block steps through dstack-resample's
//! 123-tap half-band (flat to 20 kHz, the folded band and the image below
//! −145 dB, under a 24-bit LSB), any other pair through its 256-tap sinc
//! (THD+N above 120 dB, 128 taps each side at the higher rate). On a
//! 96 kHz network ([`Rated::delay_ns`], measured by an impulse in the
//! tests):
//!
//! | App rate | Filter | RX (network to app) | TX (app to network, with its prime) |
//! |---|---|---|---|
//! | 48 kHz | half-band | 0.63 ms | 0.64 ms |
//! | 44.1 kHz | sinc | 1.32 ms | 3.08 ms |
//!
//! (v0.6.20 to v0.6.27 ran 48 kHz through the sinc too, 1.33 / 2.83 ms;
//! v0.5.20's 2:1 half-band, 63 taps, delayed about 0.3 ms each way.
//! The sinc stays for
//! SongPlayer's own ASRC through `Converter::new` and `drifting`.)
//!
//! **A fixed app block** ([`Rated::with_fixed_app_block`], dstack #330,
//! opt-in; dasio's ASIO hosts): an app whose buffer size is fixed gets
//! exactly `A = ceil(block * app_rate / device_rate)` frames a call. At a
//! ratio that does not divide the block the converter's frames go into a
//! small RX FIFO with no prime, and the app is called once in a device
//! block whose frames fill it to `A` or more, else not at all: 0 or 1 calls
//! a block, `app_rate / A` calls a second on average, never a dry RX FIFO
//! (the model of an ASIO host's own callback, whose cadence is the app's
//! rate). A block with no call adds nothing to the TX FIFO, so its prime
//! grows by `A`. That costs up to `A - 1` app frames RX and `A` TX, which
//! [`Rated::delay_ns`] states (44.1 kHz on 96 kHz with a 128-frame block:
//! `A` = 59, +1.32 ms RX, +1.34 ms TX). An exact 2:1 or 1:2 gives `A` every
//! block: the same path as [`Rated::new`], sample for sample, no delay
//! added.
//!
//! **A restart keeps the app's timeline exact** (dstack #341): a block's
//! media time labels the app's frames `start * app_rate / device_rate`.
//! At a ratio that does not divide the block that is seldom whole, so a
//! restart (the first block, or the first after a discontinuity: a pump
//! re-bootstrap) puts both converters, the FIFOs and the next app label
//! where a `Rated` that ran silence from device frame 0 (a whole app
//! frame) would have them at that start ([`Converter::skip_silence`], no
//! filter work; dstack #343: at any rate pair and any start, in a few
//! calls of the converter's core for the rest of one ratio period). An
//! impulse that crosses a re-bootstrap therefore comes back after the same
//! app samples as one that does not. No delay changes.
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

/// The delay each way, ns, from the converters' delays: RX `rx_frames` at
/// the app's rate, TX `tx_frames` at the device's plus the FIFO's [`PRIME`]
/// at the app's. The one formula [`Rated::delay_ns`] and [`delay_ns`] use,
/// so `dlink::split` states what the converters really delay.
fn delays_ns(
    rx_frames: usize,
    tx_frames: usize,
    prime: usize,
    device_rate: u32,
    app_rate: u32,
) -> (u64, u64) {
    // Both rates are a converter's, which refuses 0.
    let ns = |frames: usize, rate: u32| frames as u64 * 1_000_000_000 / u64::from(rate);
    (
        ns(rx_frames, app_rate),
        ns(tx_frames, device_rate) + ns(prime, app_rate),
    )
}

/// The silence the TX FIFO starts with for the TX converter `tx`: none for
/// the half-band's 2:1 or 1:2 (its RX twin from [`converters`], the same
/// ratio inverted, is a half-band too and gives exactly what TX takes,
/// every block, and the app's output enters the FIFO before TX takes it),
/// else [`PRIME`].
fn prime_for(tx: &Converter) -> usize {
    if tx.is_half_band() { 0 } else { PRIME }
}

/// The app's frames a call with a fixed app block: `block` device frames
/// at the app's rate, rounded up (dstack #330). Both rates are a
/// converter's, which refuses 0.
fn fixed_app_frames(device_rate: u32, app_rate: u32, block: usize) -> usize {
    let frames = (block as u64 * u64::from(app_rate)).div_ceil(u64::from(device_rate.max(1)));
    usize::try_from(frames).unwrap_or(usize::MAX)
}

/// Device frame `start` at the app's rate, rounded down (exact integers:
/// `start * app_rate / device_rate`).
fn app_frame(start: u64, device_rate: u32, app_rate: u32) -> u64 {
    let frames = u128::from(start) * u128::from(app_rate) / u128::from(device_rate);
    u64::try_from(frames).unwrap_or(u64::MAX)
}

/// The fixed app block's RX side at a ratio that does not divide the block
/// (dstack #330): the converter's frames wait here until a whole app block
/// is ready.
struct Reblock {
    /// The app's frames a call, `A`.
    frames: usize,
    /// The converter's frames waiting for the app, per RX channel.
    fifo: Vec<VecDeque<f64>>,
    /// How many frames wait (counted also with no RX channel).
    held: usize,
}

impl Reblock {
    /// Counts `k` new frames (already in `fifo`). When a whole app block
    /// waits, it leaves the count and the frames that waited before the
    /// `k` are returned: the caller takes `frames` frames off each FIFO.
    /// `held < A` before and `k <= A` (A rounds the converter's frames a
    /// block up), so a device block makes at most one app block.
    fn take(&mut self, k: usize) -> Option<usize> {
        let before = self.held;
        self.held += k;
        if self.held < self.frames {
            return None;
        }
        self.held -= self.frames;
        Some(before)
    }
}

/// The RX FIFO's worst wait, app frames: `A - 1` with a fixed app block's
/// FIFO, else none.
fn rx_wait(reblock: Option<&Reblock>) -> usize {
    reblock.map_or(0, |r| r.frames - 1)
}

/// The capacities of the fixed app block's RX FIFO and of the TX FIFO, app
/// frames, from `A` (0 without a fixed block's FIFO), the RX converter's
/// most output, the TX converter's most input and the TX prime: room to
/// spare, so `process` never grows (allocates) either.
fn rooms(a: usize, rx_out_max: usize, tx_in_max: usize, prime: usize) -> (usize, usize) {
    (
        2 * (a + rx_out_max),
        4 * (rx_out_max.max(a) + tx_in_max + prime + PRIME),
    )
}

/// The converters for the device's RX into the app and the app's TX into
/// the device: [`Converter::low_delay`], whose delay is in the app hand-off
/// (dstack #1: the half-band for 2:1 and 1:2).
fn converters(
    device_rate: u32,
    app_rate: u32,
    block: usize,
    inputs: usize,
    outputs: usize,
) -> Result<(Converter, Converter), ConvertError> {
    Ok((
        Converter::low_delay(device_rate, app_rate, inputs, block, Fixed::Input)?,
        Converter::low_delay(app_rate, device_rate, outputs, block, Fixed::Output)?,
    ))
}

/// The delay each way, ns, of a [`Rated`] for an app at `app_rate` on a
/// device at `device_rate` with `block` device frames a block (RX, TX; as
/// [`Rated::delay_ns`] reads them): (0, 0) for one rate, which needs no
/// `Rated`. A rate pair the converter refuses is its error.
pub fn delay_ns(device_rate: u32, app_rate: u32, block: usize) -> Result<(u64, u64), ConvertError> {
    if device_rate == app_rate {
        return Ok((0, 0));
    }
    let (rx, tx) = converters(device_rate, app_rate, block, 1, 1)?;
    Ok(delays_ns(
        rx.delay_frames(),
        tx.delay_frames(),
        prime_for(&tx),
        device_rate,
        app_rate,
    ))
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
    /// The device start the next block follows on at, and its app start.
    next: Option<(u64, u64)>,
    /// The device's rate and the app's, Hz.
    rates: (u32, u32),
    /// The silence the TX FIFO starts with, app frames ([`prime_for`]).
    prime: usize,
    /// TX FIFO runs dry, frames (shared with the app's status).
    dry: Arc<AtomicU64>,
    /// The RX converter's delay, ns ([`Rated::delay_ns`]'s RX): the app's
    /// block plays that much after the device's ([`Block::play_at`]). At a
    /// ratio whose app blocks vary in size (44.1 kHz on 96 kHz: 58 or 59
    /// frames), an app block's first sample sits up to one app sample off
    /// its device block's start; the play time follows the device's blocks
    /// and never drifts from them. With a fixed app block at such a ratio
    /// it holds the RX FIFO's worst case too ([`Rated::delay_ns`]).
    rx_delay_ns: i64,
    /// The app's fixed frames a call ([`Rated::with_fixed_app_block`]).
    app_block: Option<usize>,
    /// The fixed app block's RX FIFO, at a ratio that does not divide the
    /// block only.
    reblock: Option<Reblock>,
    /// The converters start again at the next block (new, or after a
    /// discontinuity): when it has a media time, they step to it first
    /// ([`Rated::restart_at`], dstack #341, #343); without one they run
    /// that block from their reset as before.
    restart: bool,
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
        Self::build(
            inner,
            device_rate,
            app_rate,
            block,
            (inputs, outputs),
            false,
        )
    }

    /// As [`Rated::new`], with the app called with exactly
    /// `A = ceil(block * app_rate / device_rate)` frames each time (dstack
    /// #330: a fixed-buffer app, an ASIO host). At a ratio that does not
    /// divide the block the app is called 0 or 1 times a device block, at
    /// `app_rate / A` calls a second on average, and [`Rated::delay_ns`]
    /// adds up to `A - 1` app frames RX and `A` TX; an exact 2:1 or 1:2 is
    /// [`Rated::new`]'s path unchanged. [`Rated::app_block`] gives `A`.
    pub fn with_fixed_app_block(
        inner: P,
        device_rate: u32,
        app_rate: u32,
        block: usize,
        inputs: usize,
        outputs: usize,
    ) -> Result<Self, ConvertError> {
        Self::build(inner, device_rate, app_rate, block, (inputs, outputs), true)
    }

    fn build(
        inner: P,
        device_rate: u32,
        app_rate: u32,
        block: usize,
        (inputs, outputs): (usize, usize),
        fixed: bool,
    ) -> Result<Self, ConvertError> {
        let (rx, tx) = converters(device_rate, app_rate, block, inputs.max(1), outputs.max(1))?;
        let app_block = fixed.then(|| fixed_app_frames(device_rate, app_rate, block));
        // The half-band's 2:1 or 1:2 (and one rate) gives exactly `A` every
        // block: no FIFO.
        let a = app_block.filter(|_| !rx.is_half_band() && !rx.is_copy());
        let prime = prime_for(&tx) + a.unwrap_or(0);
        let (rx_fifo_room, fifo_room) = rooms(
            a.unwrap_or(0),
            rx.output_frames_max(),
            tx.input_frames_max(),
            prime,
        );
        let reblock = a.map(|a| Reblock {
            frames: a,
            fifo: (0..inputs)
                .map(|_| VecDeque::with_capacity(rx_fifo_room))
                .collect(),
            held: 0,
        });
        let (rx_delay, _) = delays_ns(
            rx.delay_frames() + rx_wait(reblock.as_ref()),
            tx.delay_frames(),
            prime,
            device_rate,
            app_rate,
        );
        let rx_room = rx.output_frames_max().max(a.unwrap_or(0));
        let mut fifo: Vec<VecDeque<f64>> = (0..outputs)
            .map(|_| VecDeque::with_capacity(fifo_room))
            .collect();
        for f in &mut fifo {
            f.extend(std::iter::repeat_n(0.0, prime));
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
            next: None,
            rates: (device_rate, app_rate),
            prime,
            dry: Arc::new(AtomicU64::new(0)),
            rx_delay_ns: i64::try_from(rx_delay).unwrap_or(i64::MAX),
            app_block,
            reblock,
            restart: true,
        })
    }

    /// The delay each way, ns: RX (the device's input to the app's) and TX
    /// (the app's output to the device's, with the FIFO's [`PRIME`]).
    /// [`delay_ns`] gives the same for [`Rated::new`] before a `Rated`
    /// exists. With a fixed app block at a ratio that does not divide the
    /// block, RX adds the RX FIFO's worst case (`A - 1` app frames) and TX
    /// the prime's `A`; the free [`delay_ns`] does not include those.
    pub fn delay_ns(&self) -> (u64, u64) {
        delays_ns(
            self.rx.delay_frames() + rx_wait(self.reblock.as_ref()),
            self.tx.delay_frames(),
            self.prime,
            self.rates.0,
            self.rates.1,
        )
    }

    /// The app's frames each call when it was made with
    /// [`Rated::with_fixed_app_block`] (`A`), else `None`.
    pub fn app_block(&self) -> Option<usize> {
        self.app_block
    }

    /// The TX FIFO's dry frames since the start (each played as silence).
    pub fn dry(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.dry)
    }

    /// The app's start for a device block at `start`: the one the last
    /// block or a restart left when the device's blocks follow on, else
    /// the device's start at the app's rate.
    fn app_start(&self, start: u64) -> u64 {
        match self.next {
            Some((dev, app)) if dev == start => app,
            _ => app_frame(start, self.rates.0, self.rates.1),
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
        // A restart (dstack #341, #343): see [`Rated::restart_at`].
        if std::mem::take(&mut self.restart)
            && let Some(s) = block.start()
        {
            self.restart_at(s);
        }
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
        // The app at its rate. `app_start` is the media time of this device
        // block's converter frames; the play time is the device's + the RX
        // converter's delay: the app's block plays at P + (c) as `split`
        // states it (#295).
        let start = block.start();
        let app_start = start.map(|s| self.app_start(s));
        let play_at = block
            .play_at()
            .map(|at| at.saturating_add(self.rx_delay_ns));
        self.next = start
            .zip(app_start)
            .map(|(s, a)| (s.saturating_add(n as u64), a.saturating_add(k as u64)));
        let Some(rb) = self.reblock.as_mut() else {
            for c in 0..self.inputs {
                self.app_in[c * k..(c + 1) * k]
                    .copy_from_slice(&self.rx_out[c * room..c * room + k]);
            }
            call_app(
                &mut self.inner,
                (&self.app_in, &mut self.app_out),
                (self.inputs, self.outputs),
                k,
                (app_start, play_at),
                &mut self.fifo,
            );
            self.send(block);
            return;
        };
        // A fixed app block (dstack #330): the converter's frames join the
        // ones waiting; each whole block of `A` goes to the app. The waiting
        // frames came before this device block's: the app's block starts
        // that many frames earlier, in media time and in play time.
        let a = rb.frames;
        for (c, fifo) in rb.fifo.iter_mut().enumerate() {
            fifo.extend(self.rx_out[c * room..c * room + k].iter().copied());
        }
        if let Some(before) = rb.take(k) {
            for (c, fifo) in rb.fifo.iter_mut().enumerate() {
                for (d, v) in self.app_in[c * a..(c + 1) * a]
                    .iter_mut()
                    .zip(fifo.drain(..a))
                {
                    *d = v;
                }
            }
            let before = before as i64;
            let early_ns = before * 1_000_000_000 / i64::from(self.rates.1);
            call_app(
                &mut self.inner,
                (&self.app_in, &mut self.app_out),
                (self.inputs, self.outputs),
                a,
                (
                    app_start.map(|s| s.saturating_add_signed(-before)),
                    play_at.map(|at| at.saturating_sub(early_ns)),
                ),
                &mut self.fifo,
            );
        }
        self.send(block);
    }

    fn discontinuity(&mut self) {
        self.rx.reset();
        self.tx.reset();
        for f in &mut self.fifo {
            f.clear();
            f.extend(std::iter::repeat_n(0.0, self.prime));
        }
        if let Some(rb) = self.reblock.as_mut() {
            for f in &mut rb.fifo {
                f.clear();
            }
            rb.held = 0;
        }
        self.next = None;
        self.restart = true;
        self.inner.discontinuity();
    }
}

/// Calls the app with `frames` frames of `app_in` at `(start, play_at)`,
/// its outputs zeroed first, and puts its output into the TX FIFO.
fn call_app<P: Process>(
    inner: &mut P,
    (app_in, app_out): (&[f64], &mut [f64]),
    (inputs, outputs): (usize, usize),
    frames: usize,
    (start, play_at): (Option<u64>, Option<i64>),
    fifo: &mut [VecDeque<f64>],
) {
    // Silence where the app writes nothing, as the pump gives it (dstack
    // #335): never the previous block again.
    app_out[..outputs * frames].fill(0.0);
    let mut app = Block::new(
        frames,
        &app_in[..inputs * frames],
        &mut app_out[..outputs * frames],
    );
    if let Some(s) = start {
        app = app.at(s);
    }
    if let Some(at) = play_at {
        app = app.played_at(at);
    }
    inner.process(&mut app);
    for (c, fifo) in fifo.iter_mut().enumerate() {
        fifo.extend(app_out[c * frames..(c + 1) * frames].iter().copied());
    }
}

impl<P: Process> Rated<P> {
    /// Starts the converters at device frame `s` (the first block with a
    /// media time after `new` or a discontinuity; dstack #341, #343) as if
    /// the device and a silent app had run from device frame 0, which is a
    /// whole app frame: every app frame of the new run is then labelled
    /// exactly, on RX and TX alike, wherever `s` lands.
    ///
    /// Each converter steps its phase to `s` ([`Converter::skip_silence`],
    /// no filter work): RX gives `j` app frames for the device's `s`, TX
    /// takes `i` app frames for them. The rest of that run's state follows
    /// from those counts: the next app frame is `j`, a fixed app block's RX
    /// FIFO holds `j mod A` frames (the app took every whole `A` of them),
    /// and the TX FIFO holds the prime plus what the app made, less what TX
    /// took. All of it is silence. A skip that is refused (a half-band step
    /// at an odd start, which the pump never makes) leaves the reset state
    /// and the start's floor label, as before #341.
    ///
    /// It runs inside the restart's block: a few us at 2 channels for the
    /// common pairs, up to about 1.1 ms with 64 channels for 11.025 kHz on
    /// 192 kHz at 16-frame blocks (13 block periods; the skip's calls are
    /// `(s mod period) / block`, CHANGELOG #343).
    fn restart_at(&mut self, s: u64) {
        let rx = self.rx.skip_silence(s, &self.staged, &mut self.rx_out);
        let tx = self.tx.skip_silence(s, &self.tx_in, &mut self.tx_out);
        let (Ok((_, j)), Ok((i, _))) = (rx, tx) else {
            self.rx.reset();
            self.tx.reset();
            return;
        };
        let held = self.reblock.as_ref().map_or(0, |rb| j % rb.frames as u64);
        if let Some(rb) = self.reblock.as_mut() {
            rb.held = held as usize;
            for f in &mut rb.fifo {
                f.clear();
                f.extend(std::iter::repeat_n(0.0, rb.held));
            }
        }
        // In i128: j and i are near each other, each up to u64::MAX. A run
        // from frame 0 never runs the FIFO dry (the prime covers the
        // ratio's frame-to-frame variation), so the depth is never below 0.
        let depth = self.prime as i128 + i128::from(j) - i128::from(held) - i128::from(i);
        let have = usize::try_from(depth).unwrap_or(0);
        for f in &mut self.fifo {
            f.clear();
            f.extend(std::iter::repeat_n(0.0, have));
        }
        self.next = Some((s, j));
    }

    /// The TX converter's next input out of the FIFO into `tx_in`, a dry
    /// FIFO's rest as silence (counted); returns its frames.
    fn take_tx(&mut self) -> usize {
        let need = self.tx.input_frames_next();
        for (c, fifo) in self.fifo.iter_mut().enumerate() {
            let short = drain_into(fifo, &mut self.tx_in[c * need..(c + 1) * need]);
            self.dry.fetch_add(short as u64, Ordering::Relaxed);
        }
        need
    }

    /// TX: exactly the device's block out of the FIFO, through the TX
    /// converter, into `block`'s outputs.
    fn send(&mut self, block: &mut Block<'_>) {
        let n = self.block;
        let need = self.take_tx();
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
}

#[cfg(test)]
mod tests;
