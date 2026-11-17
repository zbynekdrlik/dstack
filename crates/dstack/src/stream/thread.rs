// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The `dstack-engine` thread: [`spawn`] and its loop, and the free
//! pacing's deadline ([`free_deadline`]). Moved out of `stream.rs` as is
//! (dstack #326, the file's size).

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dstack_audio_io::Process;

use super::{ENGINE_PRIORITY, FREE_RESYNC_BLOCKS, Pump};
use crate::clock::{PtpClock, to_samples};
use crate::media::repin::RePin;
use crate::media::tx::{MediaThread, ns_of, place, take_priority};

/// The deadline of the free block after one due at `deadline`, at `now`: a
/// period later; more than [`FREE_RESYNC_BLOCKS`] periods behind it, `now`
/// (no burst; NullRt's rule).
pub fn free_deadline(deadline: Instant, now: Instant, period: Duration) -> Instant {
    let next = deadline + period;
    if now > next + period * FREE_RESYNC_BLOCKS {
        now
    } else {
        next
    }
}

/// Starts the `dstack-engine` thread: `pump` on `clock` until `stop` is set
/// (within one block, or one free period, of it).
pub fn spawn<P: Process + 'static>(
    clock: Arc<dyn PtpClock>,
    pump: Pump<P>,
    stop: Arc<AtomicBool>,
) -> io::Result<JoinHandle<()>> {
    let stats = Arc::clone(&pump.stats);
    stats.running.store(true, Ordering::Release);
    let spawned = std::thread::Builder::new()
        .name("dstack-engine".to_string())
        .spawn(move || run(clock.as_ref(), pump, &stop));
    if spawned.is_err() {
        stats.running.store(false, Ordering::Release);
    }
    spawned
}

/// The pump thread's loop (glue: every decision is [`Pump`]'s, [`Blocks`]'
/// and [`free_deadline`]'s): on the media clock while it is ready, paced on
/// `Instant` deadlines a block apart while it is not.
fn run<P: Process>(clock: &dyn PtpClock, mut pump: Pump<P>, stop: &AtomicBool) {
    take_priority(MediaThread::Pump, ENGINE_PRIORITY, pump.tx.scheduling());
    place("engine pump", pump.tx.cfg.pump_cpu);
    let rate = pump.tx.cfg.sample_rate;
    // The bounded escape (dstack #1, Windows): only when W covers its block.
    let escapes = crate::clock::pump_may_escape(pump.timing.margin, pump.timing.block, rate);
    #[cfg(any(target_os = "linux", windows))]
    crate::clock::allow_escape(escapes);
    // The spin's capped give (dstack #351, Windows, only with the device's
    // `spin_no_yield_tail`): on the timer only when W absorbs the timer's
    // late wake, else a yield.
    #[cfg(any(target_os = "linux", windows))]
    crate::clock::give_on_timer(crate::clock::pump_gives_on_timer(pump.timing.margin, rate));
    #[cfg(windows)]
    if let Some(line) = crate::clock::pump_give_start_line(
        crate::clock::wait_mode(),
        crate::clock::no_yield_tail_on(),
        pump.timing.margin,
        rate,
    ) {
        tracing::info!("{line}");
    }
    if let Some(line) = crate::clock::pump_escape_line(
        crate::clock::escape_on(),
        escapes,
        pump.timing.margin,
        pump.timing.block,
    ) {
        tracing::info!("{line}");
    }
    let period = Duration::from_nanos(pump.stats.period_ns);
    let mut deadline: Option<Instant> = None;
    let mut pin = RePin::new(pump.tx.cfg.pump_cpu);
    while !stop.load(Ordering::Acquire) {
        pin.keep(pump.tx.now_ns());
        // The epoch before the time (`crate::media::tx::shift`).
        let anchor = crate::clock::Anchor::of(clock);
        // The app's clock, media now and the app's clock again, back to
        // back (dstack #295).
        #[cfg(any(target_os = "linux", windows))]
        let now = pump.read_clocks(clock, crate::clock::app_monotonic_ns);
        #[cfg(not(any(target_os = "linux", windows)))]
        let now = clock.now_ns();
        if let Some(now_ns) = now {
            deadline = None;
            let epoch = anchor.epoch;
            let wake = pump.media(to_samples(now_ns, rate), anchor);
            let target = ns_of(wake, rate);
            // How late the sleep returns, read right around it (dstack
            // #354), on one epoch (a re-anchor moved the clock).
            let asked = clock.now_ns();
            clock.sleep_until(target);
            let woke = clock.now_ns();
            let same = crate::clock::Anchor::of(clock).epoch == epoch;
            pump.slept(target, asked, woke, same);
        } else {
            pump.free();
            let now = Instant::now();
            let next = free_deadline(deadline.unwrap_or(now), now, period);
            deadline = Some(next);
            std::thread::sleep(next.saturating_duration_since(now));
        }
    }
    pump.stats.running.store(false, Ordering::Release);
}
