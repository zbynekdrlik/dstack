// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! A bridge device's own figures over the info port (dstack #391, for
//! dcontroller#108): a dstack-own request on [`super::REQUEST_PORT`]
//! (8700), answered only by a dstack device that holds bridge figures
//! (a dasio bridge instance, `dstack-asio`), with a fixed, versioned binary
//! body. Any other device leaves it unanswered, as it does every opcode it
//! does not know.
//!
//! **Not the heartbeat, not the info group.** The heartbeat group and the
//! info group (224.0.0.231:8702) are read by Dante Controller and every
//! Audinate device, which walk them strictly (dstack #345). So this reply
//! goes back UNICAST to the asker's address and port, and nobody else hears
//! it. A device that shares its address's 8700 with others ([`super::share`])
//! gets the request with its source ([`super::share::Msg::Ask`]) and hands
//! its reply back to the manager to send ([`super::share::Msg::SendTo`]).
//!
//! **The opcodes.** Audinate's message types on 8700 seen in inferno,
//! netaudio and our hardware captures are 0x00xx, 0x01xx, 0x10xx and
//! 0xffxx; dstack's are in 0xd5xx, which none uses:
//!
//! | | opcode |
//! |---|---|
//! | request | `07 xx d5 b1 00 00 00 00` (byte 1 anything, as Audinate's), no body |
//! | reply | `07 2a d5 b0 00 00 00 00` ([`REPLY_OPCODE`]), the body below |
//!
//! **The body, version 1** ([`figures_body`], [`parse_figures`]):
//! [`FIGURES_LEN`] bytes, big-endian, every field at a fixed offset. A
//! reader refuses any other version and any other length: a change of the
//! layout is a new version.
//!
//! | offset | type | key | unit |
//! |---|---|---|---|
//! | 0x00 | u16 | version | 1 |
//! | 0x02 | u8 | `to_asio_cause` | the latest to-ASIO underrun's cause ([`UnderrunCause`]) |
//! | 0x03 | u8 | `from_asio_cause` | the latest from-ASIO underrun's cause |
//! | 0x04 | u32 | `sequence` | this reply's number, from 1 at the device's start |
//! | 0x08 | u64 | `run_start_unix_ms` | when the device's figures started, ms since 1970 |
//! | 0x10 | u32 | `to_asio_target_us` | the Dante -> ASIO FIFO's target, us |
//! | 0x14 | u32 | `from_asio_target_us` | the ASIO -> Dante FIFO's target, us |
//! | 0x18 | u32 | `to_asio_converter_delay_us` | the to-ASIO converter's delay, us |
//! | 0x1c | u32 | `from_asio_converter_delay_us` | the from-ASIO converter's delay, us |
//! | 0x20 | u64 | `to_asio_underruns` | blocks played as silence to ASIO, since the run's start |
//! | 0x28 | u64 | `from_asio_underruns` | blocks played as silence to Dante, since the run's start |
//! | 0x30 | u64 | `late_callbacks` | driver callbacks over two periods late, this driver run |
//! | 0x38 | 8 × u64 | `late_250us` .. `late_2000us` | driver callbacks per 0.25 ms lateness bin, this driver run ([`LATE_BIN_KEYS`]) |
//!
//! A restart of the device shows as a new `run_start_unix_ms` and a
//! `sequence` that falls; the late counts start again at each driver open
//! (they fall then while the run's start stays). A target or a delay of 0
//! is not known yet (no driver opened).
//!
//! **Where the figures come from** ([`FigureSlot`], one per device,
//! `Media::bridge_figures`): the bridge's own figures (targets, delays,
//! lateness bins, late callbacks) from a source the bridge sets
//! ([`FigureSlot::set_source`], dstack-asio's `Bridge::publish_figures`),
//! read at each request; the underruns and their causes from the app's
//! loop ([`FigureSlot::set_underruns`]). A device whose slot has no source
//! answers nothing.

use std::sync::{Arc, Mutex, PoisonError};

/// The request opcode's bytes 2 and 3 (`07 xx d5 b1 00 00 00 00`).
pub const REQUEST_CODE: [u8; 2] = [0xd5, 0xb1];
/// The reply opcode's bytes 2 and 3.
pub const REPLY_CODE: [u8; 2] = [0xd5, 0xb0];
/// The reply's whole opcode.
pub const REPLY_OPCODE: [u8; 8] = [0x07, 0x2a, 0xd5, 0xb0, 0x00, 0x00, 0x00, 0x00];
/// The body's version this stack writes and reads.
pub const VERSION: u16 = 1;
/// The version 1 body's length.
pub const FIGURES_LEN: usize = 120;
/// The lateness bins (0.25 ms each, the top one open).
pub const LATE_BINS: usize = 8;
/// The lateness bins' key names, as dasio's status names them.
pub const LATE_BIN_KEYS: [&str; LATE_BINS] = [
    "late_250us",
    "late_500us",
    "late_750us",
    "late_1000us",
    "late_1250us",
    "late_1500us",
    "late_1750us",
    "late_2000us",
];

/// Where the body's fields sit.
mod at {
    pub const VERSION: usize = 0x00;
    pub const TO_CAUSE: usize = 0x02;
    pub const FROM_CAUSE: usize = 0x03;
    pub const SEQUENCE: usize = 0x04;
    pub const RUN_START: usize = 0x08;
    pub const TO_TARGET: usize = 0x10;
    pub const FROM_TARGET: usize = 0x14;
    pub const TO_DELAY: usize = 0x18;
    pub const FROM_DELAY: usize = 0x1c;
    pub const TO_UNDERRUNS: usize = 0x20;
    pub const FROM_UNDERRUNS: usize = 0x28;
    pub const LATE_CALLBACKS: usize = 0x30;
    pub const LATE_BINS: usize = 0x38;
}

const _: () = assert!(at::LATE_BINS + 8 * LATE_BINS == FIGURES_LEN);

/// The cause of a side's latest underrun, as the app's underrun watch read
/// it (dstack-asio's `UnderrunWatch`, dstack #386).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnderrunCause {
    /// No underrun on that side yet (wire 0).
    #[default]
    None,
    /// No driver callback came late enough: the bridge's (wire 1).
    Bridge,
    /// A driver callback came late enough to cost that side its audio
    /// (wire 2).
    Driver,
    /// Not known: the from-ASIO target was not known yet (wire 3).
    Unknown,
}

impl UnderrunCause {
    /// The wire byte.
    pub fn code(self) -> u8 {
        match self {
            UnderrunCause::None => 0,
            UnderrunCause::Bridge => 1,
            UnderrunCause::Driver => 2,
            UnderrunCause::Unknown => 3,
        }
    }

    /// The cause of wire byte `code`; `None` for a byte version 1 never
    /// writes.
    pub fn of_code(code: u8) -> Option<UnderrunCause> {
        match code {
            0 => Some(UnderrunCause::None),
            1 => Some(UnderrunCause::Bridge),
            2 => Some(UnderrunCause::Driver),
            3 => Some(UnderrunCause::Unknown),
            _ => None,
        }
    }
}

/// A bridge device's figures (the module's table names each field's key
/// and unit).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BridgeFigures {
    /// This reply's number, from 1 at the device's start.
    pub sequence: u32,
    /// When the device's figures started, ms since 1970.
    pub run_start_unix_ms: u64,
    pub to_asio_target_us: u32,
    pub from_asio_target_us: u32,
    pub to_asio_converter_delay_us: u32,
    pub from_asio_converter_delay_us: u32,
    pub to_asio_underruns: u64,
    pub from_asio_underruns: u64,
    pub to_asio_cause: UnderrunCause,
    pub from_asio_cause: UnderrunCause,
    /// Driver callbacks over two periods late, this driver run.
    pub late_callbacks: u64,
    /// Driver callbacks per 0.25 ms lateness bin, this driver run
    /// ([`LATE_BIN_KEYS`]).
    pub late_bins: [u64; LATE_BINS],
}

/// Why a body does not read as version 1 figures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FiguresError {
    #[error("bridge figures: {0} bytes, version 1 has {FIGURES_LEN}")]
    Length(usize),
    #[error("bridge figures: version {0}, this reader reads {VERSION}")]
    Version(u16),
    #[error("bridge figures: cause byte {0} is no cause")]
    Cause(u8),
}

/// The version 1 body of `f`.
pub fn figures_body(f: &BridgeFigures) -> [u8; FIGURES_LEN] {
    let mut c = [0u8; FIGURES_LEN];
    c[at::VERSION..at::VERSION + 2].copy_from_slice(&VERSION.to_be_bytes());
    c[at::TO_CAUSE] = f.to_asio_cause.code();
    c[at::FROM_CAUSE] = f.from_asio_cause.code();
    c[at::SEQUENCE..at::SEQUENCE + 4].copy_from_slice(&f.sequence.to_be_bytes());
    c[at::RUN_START..at::RUN_START + 8].copy_from_slice(&f.run_start_unix_ms.to_be_bytes());
    let words: [(usize, u32); 4] = [
        (at::TO_TARGET, f.to_asio_target_us),
        (at::FROM_TARGET, f.from_asio_target_us),
        (at::TO_DELAY, f.to_asio_converter_delay_us),
        (at::FROM_DELAY, f.from_asio_converter_delay_us),
    ];
    for (o, v) in words {
        c[o..o + 4].copy_from_slice(&v.to_be_bytes());
    }
    let counts: [(usize, u64); 3] = [
        (at::TO_UNDERRUNS, f.to_asio_underruns),
        (at::FROM_UNDERRUNS, f.from_asio_underruns),
        (at::LATE_CALLBACKS, f.late_callbacks),
    ];
    for (o, v) in counts {
        c[o..o + 8].copy_from_slice(&v.to_be_bytes());
    }
    for (i, v) in f.late_bins.iter().enumerate() {
        let o = at::LATE_BINS + 8 * i;
        c[o..o + 8].copy_from_slice(&v.to_be_bytes());
    }
    c
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    let mut w = [0u8; 8];
    w.copy_from_slice(&b[o..o + 8]);
    u64::from_be_bytes(w)
}

/// The figures in a reply body: version 1 and exactly [`FIGURES_LEN`]
/// bytes, each cause byte a known one.
pub fn parse_figures(body: &[u8]) -> Result<BridgeFigures, FiguresError> {
    if body.len() != FIGURES_LEN {
        return Err(FiguresError::Length(body.len()));
    }
    let version = u16::from_be_bytes([body[at::VERSION], body[at::VERSION + 1]]);
    if version != VERSION {
        return Err(FiguresError::Version(version));
    }
    let cause = |o: usize| UnderrunCause::of_code(body[o]).ok_or(FiguresError::Cause(body[o]));
    Ok(BridgeFigures {
        sequence: u32_at(body, at::SEQUENCE),
        run_start_unix_ms: u64_at(body, at::RUN_START),
        to_asio_target_us: u32_at(body, at::TO_TARGET),
        from_asio_target_us: u32_at(body, at::FROM_TARGET),
        to_asio_converter_delay_us: u32_at(body, at::TO_DELAY),
        from_asio_converter_delay_us: u32_at(body, at::FROM_DELAY),
        to_asio_underruns: u64_at(body, at::TO_UNDERRUNS),
        from_asio_underruns: u64_at(body, at::FROM_UNDERRUNS),
        to_asio_cause: cause(at::TO_CAUSE)?,
        from_asio_cause: cause(at::FROM_CAUSE)?,
        late_callbacks: u64_at(body, at::LATE_CALLBACKS),
        late_bins: std::array::from_fn(|i| u64_at(body, at::LATE_BINS + 8 * i)),
    })
}

/// Whether `opcode` is dstack's bridge-figures request (byte 1 anything).
pub fn is_request(opcode: &[u8; 8]) -> bool {
    matches!(opcode, [0x07, _, 0xd5, 0xb1, 0, 0, 0, 0])
}

/// What the bridge itself publishes ([`FigureSlot::set_source`]): its
/// targets and converter delays, and the driver callbacks' lateness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OwnFigures {
    pub to_asio_target_us: u32,
    pub from_asio_target_us: u32,
    pub to_asio_converter_delay_us: u32,
    pub from_asio_converter_delay_us: u32,
    pub late_callbacks: u64,
    pub late_bins: [u64; LATE_BINS],
}

/// What the app hands over ([`FigureSlot::set_underruns`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Underruns {
    to_asio: u64,
    from_asio: u64,
    to_cause: UnderrunCause,
    from_cause: UnderrunCause,
}

/// The bridge's own figures, read at each request.
type Source = Arc<dyn Fn() -> OwnFigures + Send + Sync>;

/// A device's bridge figures (module doc): empty until a bridge sets its
/// source; the info server reads it at each bridge-figures request.
pub struct FigureSlot {
    run_start_unix_ms: u64,
    source: Mutex<Option<Source>>,
    underruns: Mutex<Underruns>,
}

impl std::fmt::Debug for FigureSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FigureSlot")
            .field("run_start_unix_ms", &self.run_start_unix_ms)
            .field("source", &self.has_source())
            .finish_non_exhaustive()
    }
}

impl Default for FigureSlot {
    /// A slot whose run starts now.
    fn default() -> Self {
        FigureSlot::started_at(now_unix_ms())
    }
}

/// The wall clock, ms since 1970 (0 before 1970).
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

impl FigureSlot {
    /// A slot whose run started at `run_start_unix_ms`.
    pub fn started_at(run_start_unix_ms: u64) -> Self {
        FigureSlot {
            run_start_unix_ms,
            source: Mutex::new(None),
            underruns: Mutex::new(Underruns::default()),
        }
    }

    /// The bridge's own figures come from `source` from now on, read at
    /// each request on the info server's task (keep it quick: atomics and
    /// a short lock). Until a source is set the device answers no request.
    /// A later call replaces it.
    pub fn set_source(&self, source: impl Fn() -> OwnFigures + Send + Sync + 'static) {
        *self.source.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(source));
    }

    /// Whether a source is set (the device answers).
    pub fn has_source(&self) -> bool {
        self.source
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    /// The app's underrun counts since the run's start (the bridge's
    /// players' `Stats::underruns`) and each side's latest underrun's cause
    /// ([`UnderrunCause::None`] while that side has had none). The app's
    /// loop calls it at its own pace (dasio: every counts tick, after its
    /// underrun watch looked); the next reply carries the last values.
    pub fn set_underruns(
        &self,
        to_asio: u64,
        from_asio: u64,
        to_asio_cause: UnderrunCause,
        from_asio_cause: UnderrunCause,
    ) {
        *self
            .underruns
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Underruns {
            to_asio,
            from_asio,
            to_cause: to_asio_cause,
            from_cause: from_asio_cause,
        };
    }

    /// The figures now, `sequence` 0 (the reply sets it); `None` without
    /// a source.
    pub fn figures(&self) -> Option<BridgeFigures> {
        let source = self
            .source
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()?;
        let own = source();
        let u = *self
            .underruns
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        Some(BridgeFigures {
            sequence: 0,
            run_start_unix_ms: self.run_start_unix_ms,
            to_asio_target_us: own.to_asio_target_us,
            from_asio_target_us: own.from_asio_target_us,
            to_asio_converter_delay_us: own.to_asio_converter_delay_us,
            from_asio_converter_delay_us: own.from_asio_converter_delay_us,
            to_asio_underruns: u.to_asio,
            from_asio_underruns: u.from_asio,
            to_asio_cause: u.to_cause,
            from_asio_cause: u.from_cause,
            late_callbacks: own.late_callbacks,
            late_bins: own.late_bins,
        })
    }
}
