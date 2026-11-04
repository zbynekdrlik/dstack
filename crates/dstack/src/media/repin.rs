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
//! at the time it already read. A reset it finds is counted
//! ([`pin_resets`]); the daemon's once-a-second tick tells it once
//! ([`PinWatch`]), off the media threads.

use std::sync::atomic::{AtomicU64, Ordering};

use tracing::info;

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

    /// Re-applies the pin when it is due at `now_ns` (glue over
    /// [`RePin::due`] and the OS call; nothing on Linux).
    pub fn keep(&mut self, now_ns: u64) {
        if self.due(now_ns)
            && let Some(cpu) = self.cpu
            && repin(cpu)
        {
            note_reset();
        }
    }
}

/// Resets found by a re-pin in this process since its start.
static PIN_RESETS: AtomicU64 = AtomicU64::new(0);

/// The resets of a media thread's pin that a re-pin found (and undid) in
/// this process.
pub fn pin_resets() -> u64 {
    PIN_RESETS.load(Ordering::Relaxed)
}

/// Counts one reset found by a re-pin.
pub(crate) fn note_reset() {
    PIN_RESETS.fetch_add(1, Ordering::Relaxed);
}

/// On Windows: pins the calling thread to `cpu` again; `true` when its
/// affinity was something else (another program reset it). The previous
/// mask is `SetThreadAffinityMask`'s answer, so the check costs nothing.
#[cfg(windows)]
fn repin(cpu: usize) -> bool {
    use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadAffinityMask};
    if cpu >= 64 {
        return false;
    }
    let mask = 1usize << cpu;
    // SAFETY: the pseudo handle of the calling thread and a one-CPU mask.
    let before = unsafe { SetThreadAffinityMask(GetCurrentThread(), mask) };
    before != 0 && before != mask
}

/// Elsewhere: nothing resets a thread's pin.
#[cfg(not(windows))]
fn repin(_cpu: usize) -> bool {
    false
}

/// The line told when a media thread's pin was found reset.
pub const RESET_LINE: &str = "a media thread's CPU pin was reset by another program (a process affinity rule); it is re-applied every second";

/// Tells, once, that a re-pin found a media thread's pin reset.
#[derive(Debug, Default)]
pub struct PinWatch {
    told: bool,
}

impl PinWatch {
    /// A watch that has told nothing yet.
    pub fn new() -> PinWatch {
        PinWatch::default()
    }

    /// Looks at the count of resets so far ([`pin_resets`]): the first time
    /// it is above zero, one info line.
    pub fn watch(&mut self, resets: u64) {
        if resets > 0 && !self.told {
            self.told = true;
            info!(resets, "{RESET_LINE}");
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
