// SPDX-License-Identifier: MIT OR Apache-2.0
//! The media threads' CPU pins, kept: a pinned thread (`pump_cpu`,
//! `io_cpu`, `rx_cpu`) re-applies its pin about once a second. Windows
//! resets every thread's affinity to the process mask whenever a program
//! sets the process's (`SetProcessAffinityMask`); Process Lasso does so
//! after start and keeps re-applying its rule (dasio's FOH PC, dstack #1:
//! two media threads ended up on one CPU). On Linux nobody resets a
//! thread's affinity, so the re-pin is Windows only.
//!
//! The re-pin is one syscall and no allocation, on the thread's own loop
//! (at its monotonic clock: one `Instant` read). What it finds is counted
//! for the process ([`pin_resets`], [`pin_refusals`]); the daemon's
//! once-a-second tick tells each kind once per device ([`PinWatch`]), off
//! the media threads.

use std::sync::atomic::{AtomicU64, Ordering};

use tracing::{info, warn};

/// How often a pinned thread re-applies its pin, ns.
pub const REPIN_NS: u64 = 1_000_000_000;

/// When a media thread re-applies its pin: at its first look, then
/// [`REPIN_NS`] after the last re-pin; never without a configured CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RePin {
    /// The configured CPU (`pump_cpu`, `io_cpu`, `rx_cpu`).
    pub cpu: Option<usize>,
    /// When the next re-pin is due, on the thread's monotonic clock.
    pub next_ns: u64,
}

impl RePin {
    /// The schedule for a thread configured with `cpu`: due at once.
    pub fn new(cpu: Option<usize>) -> RePin {
        RePin { cpu, next_ns: 0 }
    }

    /// Whether the pin is due at `now_ns`; when it is, the next one is a
    /// second from now.
    pub fn due(&mut self, now_ns: u64) -> bool {
        if self.cpu.is_none() || now_ns < self.next_ns {
            return false;
        }
        self.next_ns = now_ns.saturating_add(REPIN_NS);
        true
    }

    /// Re-applies the pin when it is due at `now_ns` and counts what it
    /// found (glue over [`RePin::due`] and the OS call; nothing on Linux).
    pub fn keep(&mut self, now_ns: u64) {
        if self.due(now_ns)
            && let Some(cpu) = self.cpu
        {
            note(repin(cpu));
        }
    }
}

/// What a re-pin found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinOutcome {
    /// The thread was still on its CPU alone.
    Kept,
    /// The thread's affinity was something else (another program set the
    /// process's); it is pinned again.
    Reset,
    /// The OS refused the pin: the CPU is outside the process's affinity
    /// (or past the CPUs a thread can be pinned to).
    Refused,
}

/// The outcome of a re-pin from the affinity mask the thread had `before`
/// (`SetThreadAffinityMask`'s answer, 0 for a refusal) and the one-CPU
/// `mask` it asked.
pub fn outcome(before: usize, mask: usize) -> PinOutcome {
    if before == 0 {
        PinOutcome::Refused
    } else if before == mask {
        PinOutcome::Kept
    } else {
        PinOutcome::Reset
    }
}

/// The affinity mask of `cpu` alone on Windows, whose thread masks cover
/// the first processor group (64 CPUs); `None` past it.
pub fn one_cpu_mask(cpu: usize) -> Option<usize> {
    (cpu < 64).then(|| 1usize << cpu)
}

/// Resets found by a re-pin in this process since its start.
static PIN_RESETS: AtomicU64 = AtomicU64::new(0);
/// Re-pins the OS refused in this process since its start.
static PIN_REFUSALS: AtomicU64 = AtomicU64::new(0);

/// The resets of a media thread's pin that a re-pin found (and undid) in
/// this process.
pub fn pin_resets() -> u64 {
    PIN_RESETS.load(Ordering::Relaxed)
}

/// The re-pins of a media thread the OS refused in this process.
pub fn pin_refusals() -> u64 {
    PIN_REFUSALS.load(Ordering::Relaxed)
}

/// Counts what a re-pin found.
pub(crate) fn note(found: PinOutcome) {
    match found {
        PinOutcome::Kept => {}
        PinOutcome::Reset => {
            PIN_RESETS.fetch_add(1, Ordering::Relaxed);
        }
        PinOutcome::Refused => {
            PIN_REFUSALS.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// On Windows: pins the calling thread to `cpu` again. The previous mask
/// is `SetThreadAffinityMask`'s answer, so the check costs nothing. (The
/// same call as `media::tx::pin_to_cpu`, which only needs a yes or no.)
#[cfg(windows)]
fn repin(cpu: usize) -> PinOutcome {
    use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadAffinityMask};
    let Some(mask) = one_cpu_mask(cpu) else {
        return PinOutcome::Refused;
    };
    // SAFETY: the pseudo handle of the calling thread and a one-CPU mask.
    let before = unsafe { SetThreadAffinityMask(GetCurrentThread(), mask) };
    outcome(before, mask)
}

/// Elsewhere: nothing resets a thread's pin.
#[cfg(not(windows))]
fn repin(_cpu: usize) -> PinOutcome {
    PinOutcome::Kept
}

/// The line told when a media thread's pin was found lost.
pub const RESET_LINE: &str = "a media thread's CPU pin was found lost (another program set the process affinity); it is re-applied every second";

/// The line told when the OS refused a media thread's re-pin.
pub const REFUSED_LINE: &str = "a media thread's CPU pin was refused: its CPU is outside the process affinity (another program's rule?); the thread runs unpinned";

/// Tells, once each, that a re-pin found a media thread's pin lost (info)
/// or was refused (a warning).
#[derive(Debug, Default)]
pub struct PinWatch {
    told_reset: bool,
    told_refused: bool,
}

impl PinWatch {
    /// A watch that has told nothing yet.
    pub fn new() -> PinWatch {
        PinWatch::default()
    }

    /// Looks at the counts so far ([`pin_resets`], [`pin_refusals`]): the
    /// first time each is above zero, one line.
    pub fn watch(&mut self, resets: u64, refusals: u64) {
        if resets > 0 && !self.told_reset {
            self.told_reset = true;
            info!(resets, "{RESET_LINE}");
        }
        if refusals > 0 && !self.told_refused {
            self.told_refused = true;
            warn!(refusals, "{REFUSED_LINE}");
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
