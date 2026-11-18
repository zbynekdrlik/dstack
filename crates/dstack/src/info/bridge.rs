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
//! **The body, version 2** ([`figures_body`], [`parse_figures`], dstack
//! #419): [`FIGURES_LEN_V2`] bytes, big-endian, every field at a fixed
//! offset. Version 1 (dstack #391, [`FIGURES_LEN`] bytes) is its first
//! 0x78 bytes byte for byte, the version field aside; version 2 appends
//! the bridge's through-latency each way and its ASIO driver's name. A
//! reply is always version 2. A reader takes version 1 at [`FIGURES_LEN`]
//! bytes (the version 2 fields then read `None`) and version 2 at
//! [`FIGURES_LEN_V2`] bytes, and refuses any other version or length: a
//! change of the layout is a new version.
//!
//! | offset | type | key | unit |
//! |---|---|---|---|
//! | 0x00 | u16 | version | 2 (1 from a dstack before #419) |
//! | 0x02 | u8 | `to_asio_cause` | the latest to-ASIO underrun's cause ([`UnderrunCause`]) |
//! | 0x03 | u8 | `from_asio_cause` | the latest from-ASIO underrun's cause |
//! | 0x04 | u32 | `sequence` | this reply's number, from 1 at the device's start |
//! | 0x08 | u64 | `run_start_unix_ms` | when the device's figures started, ms since 1970 |
//! | 0x10 | u32 | `to_asio_target_us` | the Dante -> ASIO FIFO's target, us |
//! | 0x14 | u32 | `from_asio_target_us` | the ASIO -> Dante FIFO's target, us |
//! | 0x18 | u32 | `to_asio_converter_delay_us` | the to-ASIO converter's delay, us |
//! | 0x1c | u32 | `from_asio_converter_delay_us` | the from-ASIO converter's delay, us |
//! | 0x20 | u64 | `to_asio_underruns` | blocks played as silence to ASIO, since the device's start |
//! | 0x28 | u64 | `from_asio_underruns` | blocks played as silence to Dante, since the device's start |
//! | 0x30 | u64 | `late_callbacks` | driver callbacks over two periods late, this driver run |
//! | 0x38 | 8 × u64 | `late_250us` .. `late_2000us` | driver callbacks per 0.25 ms lateness bin, this driver run ([`LATE_BIN_KEYS`]) |
//! | 0x78 | u32 | `asio_to_dante_us` | a sample's time from its arrival at the ASIO card to its departure on Dante, us; 0 = not known (version 2) |
//! | 0x7c | u32 | `dante_to_asio_us` | a sample's time from its arrival on Dante to its departure at the ASIO card, us; 0 = not known (version 2) |
//! | 0x80 | u8 | `driver_len` | the driver name's length in bytes, 0 to [`DRIVER_MAX`]; 0 = not known (version 2) |
//! | 0x81 | 63 bytes | `driver` | the ASIO driver's name, UTF-8, cut at a char boundary to [`DRIVER_MAX`] bytes, zero-padded (version 2) |
//!
//! A restart of the device shows as a new `run_start_unix_ms` and a
//! `sequence` that falls; the late counts start again at each driver open
//! (they fall then while the run's start stays). A target or a delay of 0
//! is not known yet (no driver opened). The targets, delays and late
//! counts are read at each request; the underruns and causes, the
//! through-latency and the driver name are the app's last hand-over
//! (dasio: its counts tick, every 10 s), so a cause always belongs to the
//! counts beside it. The two parts are read under separate locks, so one
//! reply may join a hand-over a moment older than its lateness figures.
//! [`BridgeFigures`] holds a 0 latency and an empty driver name as `None`,
//! as it holds a version 1 body's missing ones.
//!
//! **The bound:** at most [`super::limit::PER_SPAN`] replies a second,
//! one bound for every asker (as the info replies'): a host that floods
//! the request can take them all, and the others' reads then time out (no
//! audio effect). A reader that asks again after a timeout may still get
//! the earlier ask's late reply: `sequence` tells it apart.
//!
//! **Where the figures come from** ([`FigureSlot`], one per device,
//! `Media::bridge_figures`): the bridge's own figures (targets, delays,
//! lateness bins, late callbacks) from a source the bridge sets
//! ([`FigureSlot::set_source`], dstack-asio's `Bridge::publish_figures`),
//! read at each request; the underruns and their causes, the
//! through-latency and the driver's name from the app
//! ([`FigureSlot::set_underruns`], [`FigureSlot::set_path`],
//! [`FigureSlot::set_driver`]). A device whose slot has no source answers
//! nothing.

use std::sync::{Arc, Mutex, PoisonError};

/// The request opcode's bytes 2 and 3 (`07 xx d5 b1 00 00 00 00`).
pub const REQUEST_CODE: [u8; 2] = [0xd5, 0xb1];
/// The reply opcode's bytes 2 and 3.
pub const REPLY_CODE: [u8; 2] = [0xd5, 0xb0];
/// The reply's whole opcode.
pub const REPLY_OPCODE: [u8; 8] = [0x07, 0x2a, 0xd5, 0xb0, 0x00, 0x00, 0x00, 0x00];
/// The body's version this stack writes (it reads [`VERSION_1`] too).
pub const VERSION: u16 = 2;
/// The first version (dstack #391), which this stack still reads.
pub const VERSION_1: u16 = 1;
/// The version 1 body's length.
pub const FIGURES_LEN: usize = 120;
/// The version 2 body's length (dstack #419).
pub const FIGURES_LEN_V2: usize = 0xc0;
/// The longest driver name a body carries, in bytes.
pub const DRIVER_MAX: usize = 63;
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
    pub const ASIO_TO_DANTE: usize = 0x78;
    pub const DANTE_TO_ASIO: usize = 0x7c;
    pub const DRIVER_LEN: usize = 0x80;
    pub const DRIVER: usize = 0x81;
}

const _: () = assert!(at::LATE_BINS + 8 * LATE_BINS == FIGURES_LEN);
const _: () = assert!(at::ASIO_TO_DANTE == FIGURES_LEN);
const _: () = assert!(at::DRIVER + DRIVER_MAX == FIGURES_LEN_V2);

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
/// and unit). Not `Copy` since dstack #419 (the driver's name).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
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
    /// A sample's time from its arrival at the ASIO card to its departure
    /// on Dante, us; `None` when not known (0 on the wire) or from a
    /// version 1 body.
    pub asio_to_dante_us: Option<u32>,
    /// A sample's time from its arrival on Dante to its departure at the
    /// ASIO card, us; `None` as [`Self::asio_to_dante_us`].
    pub dante_to_asio_us: Option<u32>,
    /// The ASIO driver's name, at most [`DRIVER_MAX`] bytes ([`driver_cut`]);
    /// `None` when not known (empty on the wire) or from a version 1 body.
    pub driver: Option<String>,
}

/// Why a body does not read as bridge figures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FiguresError {
    #[error(
        "bridge figures: {0} bytes, version 1 has {FIGURES_LEN} and version 2 {FIGURES_LEN_V2}"
    )]
    Length(usize),
    #[error("bridge figures: version {0}, this reader reads {VERSION_1} and {VERSION}")]
    Version(u16),
    #[error("bridge figures: cause byte {0} is no cause")]
    Cause(u8),
    #[error("bridge figures: a driver name of {0} bytes, at most {DRIVER_MAX}")]
    DriverLength(u8),
    #[error("bridge figures: the driver name is no UTF-8")]
    DriverText,
}

/// `name` cut to at most [`DRIVER_MAX`] bytes at a char boundary: what a
/// body carries of a driver's name.
pub fn driver_cut(name: &str) -> &str {
    let mut end = name.len().min(DRIVER_MAX);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

/// The version 2 body of `f` (its driver name cut by [`driver_cut`], a
/// `None` written as 0 or empty).
pub fn figures_body(f: &BridgeFigures) -> [u8; FIGURES_LEN_V2] {
    let mut c = [0u8; FIGURES_LEN_V2];
    c[at::VERSION..at::VERSION + 2].copy_from_slice(&VERSION.to_be_bytes());
    c[at::TO_CAUSE] = f.to_asio_cause.code();
    c[at::FROM_CAUSE] = f.from_asio_cause.code();
    c[at::SEQUENCE..at::SEQUENCE + 4].copy_from_slice(&f.sequence.to_be_bytes());
    c[at::RUN_START..at::RUN_START + 8].copy_from_slice(&f.run_start_unix_ms.to_be_bytes());
    let words: [(usize, u32); 6] = [
        (at::TO_TARGET, f.to_asio_target_us),
        (at::FROM_TARGET, f.from_asio_target_us),
        (at::TO_DELAY, f.to_asio_converter_delay_us),
        (at::FROM_DELAY, f.from_asio_converter_delay_us),
        (at::ASIO_TO_DANTE, f.asio_to_dante_us.unwrap_or(0)),
        (at::DANTE_TO_ASIO, f.dante_to_asio_us.unwrap_or(0)),
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
    let driver = driver_cut(f.driver.as_deref().unwrap_or_default()).as_bytes();
    c[at::DRIVER_LEN] = u8::try_from(driver.len()).expect("cut to 63 bytes");
    c[at::DRIVER..at::DRIVER + driver.len()].copy_from_slice(driver);
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

/// A latency word: `None` for 0 (not known).
fn known(us: u32) -> Option<u32> {
    (us != 0).then_some(us)
}

/// The figures in a reply body: version 1 at exactly [`FIGURES_LEN`]
/// bytes (the version 2 fields `None`) or version 2 at exactly
/// [`FIGURES_LEN_V2`] bytes, each cause byte a known one, a version 2
/// driver name at most [`DRIVER_MAX`] bytes of UTF-8.
pub fn parse_figures(body: &[u8]) -> Result<BridgeFigures, FiguresError> {
    let Some(v) = body.get(at::VERSION..at::VERSION + 2) else {
        return Err(FiguresError::Length(body.len()));
    };
    let version = u16::from_be_bytes([v[0], v[1]]);
    let len = match version {
        VERSION_1 => FIGURES_LEN,
        VERSION => FIGURES_LEN_V2,
        other => return Err(FiguresError::Version(other)),
    };
    if body.len() != len {
        return Err(FiguresError::Length(body.len()));
    }
    let cause = |o: usize| UnderrunCause::of_code(body[o]).ok_or(FiguresError::Cause(body[o]));
    let mut f = BridgeFigures {
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
        asio_to_dante_us: None,
        dante_to_asio_us: None,
        driver: None,
    };
    if version == VERSION {
        f.asio_to_dante_us = known(u32_at(body, at::ASIO_TO_DANTE));
        f.dante_to_asio_us = known(u32_at(body, at::DANTE_TO_ASIO));
        let n = body[at::DRIVER_LEN];
        if usize::from(n) > DRIVER_MAX {
            return Err(FiguresError::DriverLength(n));
        }
        let name = &body[at::DRIVER..at::DRIVER + usize::from(n)];
        let name = std::str::from_utf8(name).map_err(|_| FiguresError::DriverText)?;
        f.driver = (!name.is_empty()).then(|| name.to_string());
    }
    Ok(f)
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

/// What the app hands over ([`FigureSlot::set_underruns`],
/// [`FigureSlot::set_path`], [`FigureSlot::set_driver`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Handover {
    to_asio: u64,
    from_asio: u64,
    to_cause: UnderrunCause,
    from_cause: UnderrunCause,
    asio_to_dante_us: Option<u32>,
    dante_to_asio_us: Option<u32>,
    driver: Option<String>,
}

/// The bridge's own figures, read at each request.
type Source = Arc<dyn Fn() -> OwnFigures + Send + Sync>;

/// A device's bridge figures (module doc): empty until a bridge sets its
/// source; the info server reads it at each bridge-figures request.
pub struct FigureSlot {
    run_start_unix_ms: u64,
    source: Mutex<Option<Source>>,
    handover: Mutex<Handover>,
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
            handover: Mutex::new(Handover::default()),
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
        let mut h = self.handover();
        h.to_asio = to_asio;
        h.from_asio = from_asio;
        h.to_cause = to_asio_cause;
        h.from_cause = from_asio_cause;
    }

    /// The bridge's through-latency each way, us (dstack #419): a sample's
    /// time from its arrival at one card to its departure from the other,
    /// 0 while not known. The app hands it at its own pace, as
    /// [`Self::set_underruns`]; the next reply carries the last values.
    pub fn set_path(&self, asio_to_dante_us: u32, dante_to_asio_us: u32) {
        let mut h = self.handover();
        h.asio_to_dante_us = known(asio_to_dante_us);
        h.dante_to_asio_us = known(dante_to_asio_us);
    }

    /// The ASIO driver's name (dstack #419), kept cut to [`DRIVER_MAX`]
    /// bytes at a char boundary ([`driver_cut`]); an empty name is not
    /// known. The app hands it when it opens a driver; the next reply
    /// carries the last one.
    pub fn set_driver(&self, name: &str) {
        let cut = driver_cut(name);
        self.handover().driver = (!cut.is_empty()).then(|| cut.to_string());
    }

    /// The app's hand-over, locked.
    fn handover(&self) -> std::sync::MutexGuard<'_, Handover> {
        self.handover.lock().unwrap_or_else(PoisonError::into_inner)
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
        let u = self.handover().clone();
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
            asio_to_dante_us: u.asio_to_dante_us,
            dante_to_asio_us: u.dante_to_asio_us,
            driver: u.driver,
        })
    }
}
