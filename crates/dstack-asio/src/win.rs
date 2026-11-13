// SPDX-License-Identifier: MIT OR Apache-2.0

//! The Windows glue over azo 0.4.0 (a pure-Rust COM ASIO host, MIT, no
//! Steinberg SDK), lifted from SongPlayer's `asio_win.rs` (songplayer #233,
//! dstack #243). It only CALLS the driver; every decision is in the
//! Linux-tested `format`, `state`, `device`, `engine` and `runner`, so this
//! file is out of the mutation gate (`.cargo/mutants.toml`).
//!
//! - Every driver call is made on the worker thread that created the device
//!   and pumps its window messages in `poll`. The device holds that
//!   thread's COM apartment (an STA, [`ComApartment`]) from `new` until it
//!   is dropped: azo's `SafeHandle` initialises COM in `new` but
//!   uninitialises it in its own `Drop`, BEFORE its interface field is
//!   released, so without an outer apartment the driver's `Release` would
//!   run after COM went down.
//! - It reads the driver's rate, preferred buffer, channels and their sample
//!   types, and never sets the rate, the clock source or the buffer, nor
//!   opens the control panel.
//! - One holder per driver in the process ([`crate::hold`]): `open` holds
//!   the driver's name before it loads the driver and `close` gives it back
//!   after the release.
//! - ASIO callbacks carry no user pointer: [`SLOTS`] static slots, each
//!   with its own four callbacks, hold the running streams. An in-flight
//!   counter lets `close` free a stream only after the last callback left
//!   it, and a slot is released only after the buffers are disposed and the
//!   driver dropped. A callback still inside after 1 s of waiting (pumping
//!   the thread's messages, which a driver may need to finish it) PARKS the
//!   device: the stream, the buffers, the driver, the slot and the driver's
//!   hold are never freed, disposed, released or reused until the process
//!   ends, and the device refuses every later open.
//! - The buffer switch runs the stream's [`Engine`] over the driver's
//!   half-buffers and times itself (a monotonic read against
//!   [`CALLBACK_EPOCH`]); driver messages are answered by
//!   [`crate::state::reply`] and counted for `poll`.

use std::cell::UnsafeCell;
use std::ffi::{CStr, c_long, c_void};
use std::ptr;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use azo::driver::{Driver, Metadata, SafeHandle};
use azo::dto::ChannelId;
use azo::sys::{Bool, Callbacks, MessageSelector, SampleRate, Time};
use tracing::{error, warn};
use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
};

use crate::device::{Channels, Device, Opened, Started, admit_buffer, check_channels, one_sample};
use crate::engine::{Counters, Engine};
use crate::hold::{DriverHold, DriverHolds};
use crate::state::{
    DeviceEvents, Reason, admit_rate, registry_key_absent, reply, sample_rate_error, selector,
};

/// Static callback slots: drivers this process may run at once (a
/// replaced device's old stream may still hold its slot while its successor
/// starts).
pub const SLOTS_COUNT: usize = 8;

/// One running stream, owned by a slot while its driver runs.
struct Stream {
    /// Touched only by this slot's callbacks (one driver's callbacks never
    /// overlap).
    engine: UnsafeCell<Engine>,
    /// Every driver output channel's two half-buffers, in channel order.
    outputs: Vec<[*mut c_void; 2]>,
    /// The used inputs' two half-buffers, in `Channels::inputs` order.
    inputs: Vec<[*mut c_void; 2]>,
    output_bytes: usize,
    input_bytes: usize,
}

/// A slot's stream pointer and the driver's messages (static: a message
/// that comes before the stream exists is still counted).
struct Slot {
    stream: AtomicPtr<Stream>,
    in_flight: AtomicUsize,
    claimed: AtomicBool,
    reset: AtomicBool,
    resync: AtomicBool,
    size_change: AtomicBool,
    latencies: AtomicBool,
    overloads: AtomicU64,
    /// The last rate `sampleRateDidChange` reported (f64 bits), valid while
    /// `rate_changed` is set (a 0 Hz report, a lost clock, counts too).
    rate_bits: AtomicU64,
    rate_changed: AtomicBool,
}

impl Slot {
    const fn new() -> Self {
        Self {
            stream: AtomicPtr::new(ptr::null_mut()),
            in_flight: AtomicUsize::new(0),
            claimed: AtomicBool::new(false),
            reset: AtomicBool::new(false),
            resync: AtomicBool::new(false),
            size_change: AtomicBool::new(false),
            latencies: AtomicBool::new(false),
            overloads: AtomicU64::new(0),
            rate_bits: AtomicU64::new(0),
            rate_changed: AtomicBool::new(false),
        }
    }

    fn clear(&self) {
        self.overloads.store(0, Ordering::SeqCst);
        self.rate_bits.store(0, Ordering::SeqCst);
        for f in [
            &self.reset,
            &self.resync,
            &self.size_change,
            &self.latencies,
            &self.rate_changed,
        ] {
            f.store(false, Ordering::SeqCst);
        }
    }
}

static SLOTS: [Slot; SLOTS_COUNT] = [const { Slot::new() }; SLOTS_COUNT];

/// The ASIO drivers this process holds.
static HELD: DriverHolds = DriverHolds::new();

/// The epoch of every buffer switch's instant, set by the first `start`
/// (outside any callback: a callback only reads it).
static CALLBACK_EPOCH: OnceLock<Instant> = OnceLock::new();

/// ns since `epoch` (`Instant` is QueryPerformanceCounter: no allocation,
/// lock or log).
fn since_epoch_ns(epoch: Instant) -> i64 {
    i64::try_from(epoch.elapsed().as_nanos()).unwrap_or(i64::MAX)
}

/// The buffer switch of `slot`: the engine over the driver's half-buffers.
fn on_buffer(slot: &Slot, second: bool) {
    slot.in_flight.fetch_add(1, Ordering::SeqCst);
    let raw = slot.stream.load(Ordering::SeqCst);
    // SAFETY: a non-null slot points to a live stream: `close` clears the
    // slot and waits for `in_flight == 0` before it frees the stream, and
    // this callback counted itself before it read the slot.
    if let Some(stream) = unsafe { raw.as_ref() } {
        // SAFETY: one driver's callbacks never overlap; only they touch the
        // engine.
        let engine = unsafe { &mut *stream.engine.get() };
        let half = usize::from(second);
        for (i, halves) in stream.inputs.iter().enumerate() {
            let p = halves[half];
            let src: &[u8] = if p.is_null() {
                &[]
            } else {
                // SAFETY: the driver's input half-buffer of `frames` samples
                // of its type, filled before this callback.
                unsafe { std::slice::from_raw_parts(p.cast::<u8>(), stream.input_bytes) }
            };
            engine.decode_input(i, src);
        }
        engine.run();
        for (ch, halves) in stream.outputs.iter().enumerate() {
            let p = halves[half];
            if p.is_null() {
                continue;
            }
            // SAFETY: the driver's output half-buffer of `frames` samples of
            // its type, which the driver does not touch during the callback.
            let dst =
                unsafe { std::slice::from_raw_parts_mut(p.cast::<u8>(), stream.output_bytes) };
            engine.encode_output(ch, dst);
        }
        if let Some(&epoch) = CALLBACK_EPOCH.get() {
            engine.note_callback(since_epoch_ns(epoch));
        }
    }
    slot.in_flight.fetch_sub(1, Ordering::SeqCst);
}

/// A driver message to `slot`: counted, answered.
fn on_message(slot: &Slot, sel: c_long, value: c_long) -> c_long {
    match sel {
        selector::RESET_REQUEST => slot.reset.store(true, Ordering::SeqCst),
        selector::BUFFER_SIZE_CHANGE => slot.size_change.store(true, Ordering::SeqCst),
        selector::RESYNC_REQUEST => slot.resync.store(true, Ordering::SeqCst),
        selector::LATENCIES_CHANGED => slot.latencies.store(true, Ordering::SeqCst),
        selector::OVERLOAD => {
            slot.overloads.fetch_add(1, Ordering::Relaxed);
        }
        _ => {}
    }
    reply(sel, value)
}

macro_rules! slot_callbacks {
    ($i:literal, $bs:ident, $bsti:ident, $msg:ident, $rate:ident) => {
        unsafe extern "system" fn $bs(index: c_long, _direct: Bool) {
            on_buffer(&SLOTS[$i], index != 0);
        }
        unsafe extern "system" fn $bsti(
            params: *mut Time,
            index: c_long,
            _direct: Bool,
        ) -> *mut Time {
            on_buffer(&SLOTS[$i], index != 0);
            params
        }
        unsafe extern "system" fn $msg(
            sel: MessageSelector,
            value: c_long,
            _message: *const c_void,
            _opt: *const f64,
        ) -> c_long {
            on_message(&SLOTS[$i], sel.0, value)
        }
        unsafe extern "system" fn $rate(rate: SampleRate) {
            SLOTS[$i].rate_bits.store(rate.to_bits(), Ordering::SeqCst);
            SLOTS[$i].rate_changed.store(true, Ordering::SeqCst);
        }
    };
}

slot_callbacks!(0, bs0, bsti0, msg0, rate0);
slot_callbacks!(1, bs1, bsti1, msg1, rate1);
slot_callbacks!(2, bs2, bsti2, msg2, rate2);
slot_callbacks!(3, bs3, bsti3, msg3, rate3);
slot_callbacks!(4, bs4, bsti4, msg4, rate4);
slot_callbacks!(5, bs5, bsti5, msg5, rate5);
slot_callbacks!(6, bs6, bsti6, msg6, rate6);
slot_callbacks!(7, bs7, bsti7, msg7, rate7);

macro_rules! callbacks {
    ($bs:ident, $bsti:ident, $msg:ident, $rate:ident) => {
        Callbacks {
            buffer_switch: $bs,
            sample_rate_did_change: $rate,
            asio_message: $msg,
            buffer_switch_time_info: $bsti,
        }
    };
}

/// Slot `i`'s callbacks (static: they outlive every buffer created on them).
static CALLBACKS: [Callbacks; SLOTS_COUNT] = [
    callbacks!(bs0, bsti0, msg0, rate0),
    callbacks!(bs1, bsti1, msg1, rate1),
    callbacks!(bs2, bsti2, msg2, rate2),
    callbacks!(bs3, bsti3, msg3, rate3),
    callbacks!(bs4, bsti4, msg4, rate4),
    callbacks!(bs5, bsti5, msg5, rate5),
    callbacks!(bs6, bsti6, msg6, rate6),
    callbacks!(bs7, bsti7, msg7, rate7),
];

/// The registered ASIO drivers' names (HKLM\SOFTWARE\ASIO, read only; no
/// driver is loaded). None registered (or no such key) → empty; any other
/// failure of the read → its error.
pub fn drivers() -> Result<Vec<String>, String> {
    match Metadata::enumerate() {
        Ok(d) => Ok(d.iter().map(|m| m.description.to_string_lossy()).collect()),
        Err(e) if registry_key_absent(e.code().0) => Ok(Vec::new()),
        Err(e) => Err(e.to_string()),
    }
}

fn text(s: &CStr) -> String {
    s.to_string_lossy().into_owned()
}

/// A driver call that failed, with the driver's own last error.
fn failed(driver: &SafeHandle, what: &str, e: azo::Error) -> Reason {
    Reason::Failed(format!("{what}: {e} ({})", text(&driver.last_error())))
}

/// Run every window message queued for this (the driver's STA) thread.
fn pump_messages() {
    // SAFETY: MSG is plain data; a zeroed one is valid.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    // SAFETY: a plain message pump on the calling thread, `msg` outlives it.
    while unsafe { PeekMessageW(&mut msg, ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
        // SAFETY: `msg` is the message PeekMessageW just filled.
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// The calling thread's COM apartment (an STA), held until dropped.
struct ComApartment {
    /// The init succeeded (S_OK, or S_FALSE: already an STA), so the drop
    /// owes one `CoUninitialize`.
    held: bool,
}

impl ComApartment {
    fn enter() -> Self {
        // SAFETY: a plain COM init of the calling thread, balanced in Drop.
        let hr = unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        if hr < 0 {
            error!(
                hr,
                "asio: COM refused this thread as an STA (a driver load will fail)"
            );
        }
        Self { held: hr >= 0 }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.held {
            // SAFETY: balances this guard's successful init, on its thread
            // (the device holding it is !Send: its stream pointer).
            unsafe { CoUninitialize() };
        }
    }
}

/// One ASIO driver, on its worker thread.
pub struct WinDevice {
    /// This device's hold on its driver's name, from before the load until
    /// after the release (kept for good when the device is parked).
    hold: Option<DriverHold<'static>>,
    driver: Option<SafeHandle>,
    channels: Channels,
    opened: Option<Opened>,
    slot: Option<usize>,
    stream: *mut Stream,
    counters: Option<Arc<Counters>>,
    buffers_created: bool,
    /// `start` succeeded: `close` stops only a started driver.
    started: bool,
    /// A callback never left the stream (module doc).
    parked: bool,
    /// The thread's STA, for the device's whole life; declared last, so it
    /// drops last (after `Drop::drop` ran `close`, which releases the
    /// driver).
    com: ComApartment,
}

impl WinDevice {
    /// No driver loaded; the calling (worker) thread is held as an STA.
    pub fn new() -> Self {
        Self {
            hold: None,
            driver: None,
            channels: Channels::default(),
            opened: None,
            slot: None,
            stream: ptr::null_mut(),
            counters: None,
            buffers_created: false,
            started: false,
            parked: false,
            com: ComApartment::enter(),
        }
    }

    /// The sample type codes of the driver's channels `indices` (`input`
    /// side or not).
    fn codes(
        driver: &SafeHandle,
        input: bool,
        indices: impl Iterator<Item = c_long>,
    ) -> Result<Vec<i32>, Reason> {
        indices
            .map(|index| {
                driver
                    .channel_info(ChannelId { input, index })
                    .map(|info| info.sample_type.0)
                    .map_err(|e| failed(driver, "getChannelInfo", e))
            })
            .collect()
    }

    /// The driver's rate, preferred buffer, channels and sample types (read,
    /// never set).
    fn read(driver: &SafeHandle, channels: &Channels) -> Result<Opened, Reason> {
        let rate = driver.get_sample_rate().map_err(|e| {
            sample_rate_error(e.code().0).unwrap_or_else(|| failed(driver, "getSampleRate", e))
        })?;
        admit_rate(rate)?;
        let size = driver
            .buffer_size()
            .map_err(|e| failed(driver, "getBufferSize", e))?;
        let counts = driver
            .channel_counts()
            .map_err(|e| failed(driver, "getChannels", e))?;
        let driver_inputs = u32::try_from(counts.in_).unwrap_or(0);
        let driver_outputs = u32::try_from(counts.out).unwrap_or(0);
        check_channels(channels, driver_inputs, driver_outputs)?;
        let output_codes = Self::codes(driver, false, 0..counts.out.max(0))?;
        let input_codes = Self::codes(driver, true, channels.inputs.iter().map(|&c| c as c_long))?;
        Ok(Opened {
            rate,
            buffer_frames: admit_buffer(size.preferred)?,
            driver_inputs,
            driver_outputs,
            input_sample: one_sample(&input_codes, "input")?,
            output_sample: one_sample(&output_codes, "output")?,
        })
    }
}

impl Default for WinDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl Device for WinDevice {
    fn open(&mut self, name: &str, channels: &Channels) -> Result<Opened, Reason> {
        self.close();
        if self.parked || HELD.is_parked(name) {
            return Err(Reason::Parked);
        }
        // Before anything loads; on a failed open the hold drops after the
        // driver (locals drop in reverse order).
        let Some(hold) = HELD.claim(name) else {
            return Err(Reason::Held);
        };
        let found = Metadata::enumerate().unwrap_or_default();
        let present: Vec<String> = found
            .iter()
            .map(|d| d.description.to_string_lossy())
            .collect();
        let Some(meta) = found
            .iter()
            .find(|d| d.description.to_string_lossy() == name)
        else {
            return Err(Reason::NotFound { present });
        };
        let driver = match SafeHandle::new(&meta.clsid) {
            Ok(driver) => driver,
            Err(e) => {
                // azo counted a COM init before its CoCreateInstance failed
                // and never undoes it; inside the device's STA that init
                // cannot have failed, so it is balanced here.
                if self.com.held {
                    // SAFETY: balances azo's init on this thread.
                    unsafe { CoUninitialize() };
                }
                return Err(Reason::Failed(format!("loading the driver failed: {e}")));
            }
        };
        if !driver.init(None) {
            return Err(Reason::Busy(text(&driver.last_error())));
        }
        let opened = Self::read(&driver, channels)?;
        self.hold = Some(hold);
        self.driver = Some(driver);
        self.channels = channels.clone();
        self.opened = Some(opened);
        Ok(opened)
    }

    fn start(&mut self, engine: Engine) -> Result<Started, Reason> {
        let (Some(driver), Some(opened)) = (self.driver.as_ref(), self.opened) else {
            return Err(Reason::Failed("start before open".into()));
        };
        let slot = (0..SLOTS_COUNT)
            .find(|&i| {
                SLOTS[i]
                    .claimed
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            })
            .ok_or_else(|| Reason::Refused("every ASIO callback slot is in use".into()))?;
        SLOTS[slot].clear();
        // Set before any callback can come: the driver starts below.
        CALLBACK_EPOCH.get_or_init(Instant::now);
        self.slot = Some(slot);
        let frames = opened.buffer_frames as usize;
        let outs = opened.driver_outputs as usize;
        let ids = (0..outs as c_long)
            .map(|index| ChannelId {
                input: false,
                index,
            })
            .chain(self.channels.inputs.iter().map(|&c| ChannelId {
                input: true,
                index: c as c_long,
            }));
        // SAFETY: CALLBACKS is a static, so it outlives the buffers; the
        // buffers are written only by this slot's callback (between start
        // and stop) and are zeroed below before the start.
        let created = unsafe { driver.create_buffers(ids, frames as c_long, &CALLBACKS[slot]) }
            .map_err(|e| failed(driver, "createBuffers", e))?;
        let mut outputs: Vec<[*mut c_void; 2]> = created.collect();
        let inputs = outputs.split_off(outs.min(outputs.len()));
        self.buffers_created = true;
        let output_bytes = frames * opened.output_sample.map_or(0, |s| s.bytes());
        let input_bytes = frames * opened.input_sample.map_or(0, |s| s.bytes());
        for half in outputs.iter().flatten().filter(|p| !p.is_null()) {
            // SAFETY: a fresh half-buffer of `output_bytes`; no callback runs
            // before the start.
            unsafe { ptr::write_bytes(half.cast::<u8>(), 0, output_bytes) };
        }
        self.counters = Some(engine.counters());
        let stream = Box::into_raw(Box::new(Stream {
            engine: UnsafeCell::new(engine),
            outputs,
            inputs,
            output_bytes,
            input_bytes,
        }));
        self.stream = stream;
        SLOTS[slot].stream.store(stream, Ordering::SeqCst);
        driver.start().map_err(|e| failed(driver, "start", e))?;
        self.started = true;
        Ok(self.latencies())
    }

    fn poll(&mut self) -> DeviceEvents {
        pump_messages();
        let (Some(i), Some(c)) = (self.slot, self.counters.as_ref()) else {
            return DeviceEvents::default();
        };
        let s = &SLOTS[i];
        // The flag first: a later report between the two reads is the one
        // read (the newest rate wins), never a lost one.
        let rate_changed = s
            .rate_changed
            .swap(false, Ordering::SeqCst)
            .then(|| f64::from_bits(s.rate_bits.load(Ordering::SeqCst)));
        DeviceEvents {
            reset: s.reset.swap(false, Ordering::SeqCst),
            resync: s.resync.swap(false, Ordering::SeqCst),
            buffer_size_change: s.size_change.swap(false, Ordering::SeqCst),
            latencies_changed: s.latencies.swap(false, Ordering::SeqCst),
            rate_changed,
            overloads: s.overloads.load(Ordering::Relaxed),
            callbacks: c.callbacks(),
            callback_gap_max_ns: c.take_gap_max_ns(),
            late_callbacks: c.late(),
            fast_callbacks: c.fast(),
            faults: c.faults(),
        }
    }

    fn consumed_frames(&self) -> u64 {
        self.counters.as_ref().map_or(0, |c| c.consumed())
    }

    fn latencies(&self) -> Started {
        self.driver
            .as_ref()
            .and_then(|d| d.latencies().ok())
            .map_or_else(Started::default, |l| Started {
                input_latency_frames: u32::try_from(l.in_).unwrap_or(0),
                output_latency_frames: u32::try_from(l.out).unwrap_or(0),
            })
    }

    fn close(&mut self) {
        if self.parked {
            return;
        }
        if self.started
            && let Some(d) = self.driver.as_ref()
            && let Err(e) = d.stop()
        {
            warn!(%e, "asio: stopping the driver failed (it is released anyway)");
        }
        self.started = false;
        if let Some(i) = self.slot {
            let s = &SLOTS[i];
            s.stream.store(ptr::null_mut(), Ordering::SeqCst);
            // Bounded, and pumping: a driver may need this thread's messages
            // to finish a callback. One read of 0 is enough: the slot is
            // already unhooked, so a callback that enters after it sees no
            // stream.
            let mut left = false;
            for _ in 0..1_000 {
                if s.in_flight.load(Ordering::SeqCst) == 0 {
                    left = true;
                    break;
                }
                pump_messages();
                std::thread::sleep(Duration::from_millis(1));
            }
            left = left || s.in_flight.load(Ordering::SeqCst) == 0;
            if !left {
                error!(
                    slot = i,
                    "asio: a callback is still running after 1 s - its stream, buffers, driver, slot and hold are parked until the process restarts"
                );
                self.parked = true;
                self.stream = ptr::null_mut();
                std::mem::forget(self.driver.take());
                self.buffers_created = false;
                self.opened = None;
                return;
            }
            if !self.stream.is_null() {
                // SAFETY: the slot no longer points to it and no callback is
                // inside it.
                drop(unsafe { Box::from_raw(self.stream) });
            }
        }
        self.stream = ptr::null_mut();
        if self.buffers_created
            && let Some(d) = self.driver.as_ref()
            && let Err(e) = d.dispose_buffers()
        {
            warn!(%e, "asio: disposing the driver's buffers failed (it is released anyway)");
        }
        self.buffers_created = false;
        // azo's handle uninitialises its own COM init, then releases the
        // driver: still inside the device's apartment (`com`).
        self.driver = None;
        // Given back only now, after the release.
        self.hold = None;
        self.opened = None;
        if let Some(i) = self.slot.take() {
            SLOTS[i].claimed.store(false, Ordering::SeqCst);
        }
    }
}

impl Drop for WinDevice {
    fn drop(&mut self) {
        self.close();
        if self.parked
            && let Some(hold) = self.hold.take()
        {
            // The parked driver stays loaded: no other device of the process
            // may load a second instance of it.
            hold.park();
        }
    }
}
