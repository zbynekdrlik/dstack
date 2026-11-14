// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The media threads' scheduling: what each asks ([`take_priority`], by its
//! timing profile on this OS), what it got ([`priority_outcome`]), MMCSS on
//! Windows ([`join_mmcss`]), and a device's record of it ([`Scheduling`],
//! dstack #281): each engine thread's outcome and MMCSS task index, which
//! the status shows (`engine.threads[]`), so an app can tell a pump that
//! MMCSS dropped from one that was starved.

use std::sync::atomic::{AtomicU64, Ordering};

use tracing::{info, warn};

use super::ask_priority;
use crate::clock::{Os, ThreadPriority};

/// What a media thread got of the priority its profile asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// Real-time priority, as asked: `SCHED_FIFO` on Linux, the real-time
    /// profile's thread priority on Windows.
    Granted,
    /// THREAD_PRIORITY_HIGHEST, as the normal profile asks on Windows
    /// (dstack #90), outside MMCSS (it refused).
    Highest,
    /// The normal profile on Windows in MMCSS "Pro Audio" at
    /// AVRT_PRIORITY_HIGH (dstack #1), HIGHEST beneath it.
    Mmcss,
    /// In MMCSS "Pro Audio", but AVRT_PRIORITY_HIGH refused: the task's
    /// default priority (dstack #281).
    MmcssPriorityRefused,
    /// Normal priority, as the normal profile asks elsewhere (dlink on
    /// Linux).
    Normal,
    /// A raised priority asked and refused: the thread runs at normal.
    Refused,
}

impl Priority {
    /// Every outcome, in the order of [`Priority::code`].
    const ALL: [Priority; 6] = [
        Priority::Granted,
        Priority::Highest,
        Priority::Mmcss,
        Priority::MmcssPriorityRefused,
        Priority::Normal,
        Priority::Refused,
    ];

    /// The outcome's code in a [`Scheduling`] slot, 1 to 6 (0: not
    /// started).
    fn code(self) -> u64 {
        match self {
            Priority::Granted => 1,
            Priority::Highest => 2,
            Priority::Mmcss => 3,
            Priority::MmcssPriorityRefused => 4,
            Priority::Normal => 5,
            Priority::Refused => 6,
        }
    }

    /// The outcome of `code` ([`Priority::code`]); `None` for 0 (not
    /// started) and anything else.
    fn of_code(code: u64) -> Option<Priority> {
        Priority::ALL.into_iter().find(|p| p.code() == code)
    }

    /// The status word on `os`: `sched_fifo` (a real-time grant on Linux;
    /// `realtime` on Windows, where it is a thread priority, not a
    /// scheduler), `highest`, `mmcss_high`, `mmcss_joined_priority_refused`,
    /// `normal`, `refused`.
    pub fn word(self, os: Os) -> &'static str {
        match (self, os) {
            (Priority::Granted, Os::Other) => "sched_fifo",
            (Priority::Granted, Os::Windows) => "realtime",
            (Priority::Highest, _) => "highest",
            (Priority::Mmcss, _) => "mmcss_high",
            (Priority::MmcssPriorityRefused, _) => "mmcss_joined_priority_refused",
            (Priority::Normal, _) => "normal",
            (Priority::Refused, _) => "refused",
        }
    }
}

/// What a thread's MMCSS join gave ([`mmcss_of`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mmcss {
    /// Not in MMCSS: not asked, or the join refused.
    Out,
    /// In "Pro Audio" at AVRT_PRIORITY_HIGH.
    High,
    /// In "Pro Audio", AVRT_PRIORITY_HIGH refused.
    PriorityRefused,
}

/// What [`join_mmcss`]'s answer `joined` means for the thread, and MMCSS's
/// task index when it joined.
pub(crate) fn mmcss_of(joined: &Result<u32, MmcssError>) -> (Mmcss, Option<u32>) {
    match joined {
        Ok(index) => (Mmcss::High, Some(*index)),
        Err(MmcssError::Priority { index, .. }) => (Mmcss::PriorityRefused, Some(*index)),
        Err(MmcssError::Join(_)) => (Mmcss::Out, None),
    }
}

/// The outcome for a thread that asked `asked`, whose ask succeeded
/// (`granted`) and whose MMCSS join gave `mmcss` (asked only with
/// [`ThreadPriority::Highest`], [`wants_mmcss`]).
pub(crate) fn priority_outcome(asked: ThreadPriority, granted: bool, mmcss: Mmcss) -> Priority {
    match (asked, granted, mmcss) {
        (ThreadPriority::Normal, _, _) => Priority::Normal,
        (ThreadPriority::Realtime, true, _) => Priority::Granted,
        (ThreadPriority::Highest, _, Mmcss::High) => Priority::Mmcss,
        (ThreadPriority::Highest, _, Mmcss::PriorityRefused) => Priority::MmcssPriorityRefused,
        (ThreadPriority::Highest, true, Mmcss::Out) => Priority::Highest,
        (_, false, _) => Priority::Refused,
    }
}

/// Whether a media thread that asks `asked` also joins MMCSS "Pro Audio"
/// (dstack #1, songplayer #285): the normal profile on Windows
/// ([`ThreadPriority::Highest`]). Its THREAD_PRIORITY_HIGHEST is base 10 in
/// the app's normal class, under an app's own MMCSS audio threads (the
/// realtime band), which left SongPlayer's pump off the CPU for 22 to 41 ms
/// under load (pump misses: silence in the in-ears). MMCSS asks no
/// privilege and keeps its share of the CPU for the rest of the system.
/// The real-time profile (dasio, iemmixer) is above MMCSS already.
pub(crate) fn wants_mmcss(asked: ThreadPriority) -> bool {
    asked == ThreadPriority::Highest
}

/// A device's media threads whose scheduling [`Scheduling`] keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaThread {
    /// The engine's pump (`crate::stream`).
    Pump,
    /// The TX thread.
    Tx,
    /// The TX thread's relay (the bounded escape, dstack #141).
    TxRelay,
    /// The RX thread.
    Rx,
}

impl MediaThread {
    /// Every thread, in the status's order.
    pub const ALL: [MediaThread; 4] = [
        MediaThread::Pump,
        MediaThread::Tx,
        MediaThread::TxRelay,
        MediaThread::Rx,
    ];

    /// Its status word: `pump`, `tx`, `tx_relay`, `rx`.
    pub fn word(self) -> &'static str {
        match self {
            MediaThread::Pump => "pump",
            MediaThread::Tx => "tx",
            MediaThread::TxRelay => "tx_relay",
            MediaThread::Rx => "rx",
        }
    }

    /// Its name in the log lines.
    fn log_name(self) -> &'static str {
        match self {
            MediaThread::Pump => "engine pump",
            MediaThread::Tx => "TX thread",
            MediaThread::TxRelay => "TX relay thread",
            MediaThread::Rx => "RX thread",
        }
    }

    /// Its slot in a [`Scheduling`].
    fn slot(self) -> usize {
        match self {
            MediaThread::Pump => 0,
            MediaThread::Tx => 1,
            MediaThread::TxRelay => 2,
            MediaThread::Rx => 3,
        }
    }
}

/// One thread's scheduling as a [`Scheduling`] keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scheduled {
    pub thread: MediaThread,
    pub outcome: Priority,
    /// MMCSS's task index, when the thread joined MMCSS.
    pub mmcss_task: Option<u32>,
}

/// A device's record of its media threads' scheduling (dstack #281): each
/// thread's last `take_priority` outcome and MMCSS task index, one atomic
/// word each (the outcome's code in the low 8 bits, the task index + 1
/// above them, 0 for none; a word of 0: not started).
#[derive(Debug, Default)]
pub struct Scheduling {
    slots: [AtomicU64; 4],
}

impl Scheduling {
    pub fn new() -> Self {
        Scheduling::default()
    }

    /// `thread` got `outcome` (and MMCSS's task index `task`).
    pub fn record(&self, thread: MediaThread, outcome: Priority, task: Option<u32>) {
        let task = task.map_or(0, |t| u64::from(t) + 1);
        // Disjoint halves: a sum, never an `|` (its `^` mutant is the same).
        let word = (task << 8) + outcome.code();
        self.slots[thread.slot()].store(word, Ordering::Release);
    }

    /// The threads that took their priority, in [`MediaThread::ALL`]'s
    /// order.
    pub fn threads(&self) -> Vec<Scheduled> {
        MediaThread::ALL
            .into_iter()
            .filter_map(|thread| {
                let word = self.slots[thread.slot()].load(Ordering::Acquire);
                let outcome = Priority::of_code(word & 0xff)?;
                let task = word >> 8;
                Some(Scheduled {
                    thread,
                    outcome,
                    mmcss_task: task.checked_sub(1).map(|t| t as u32),
                })
            })
            .collect()
    }
}

/// Asks the priority for the calling media `thread` (its profile's on
/// this OS, [`crate::clock::thread_priority_for`], through
/// [`ask_priority`]), records what it got in `scheduling` and logs it:
/// info when it is what was asked, a warning when a raised priority or
/// MMCSS's was refused (the thread runs on).
pub(crate) fn take_priority(thread: MediaThread, priority: i32, scheduling: &Scheduling) {
    let asked =
        crate::clock::thread_priority_for(crate::clock::profile(), crate::clock::Os::current());
    let granted = ask_priority(asked, priority);
    let joined = wants_mmcss(asked).then(join_mmcss);
    let (mmcss, task) = joined.as_ref().map_or((Mmcss::Out, None), mmcss_of);
    let outcome = priority_outcome(asked, granted, mmcss);
    scheduling.record(thread, outcome, task);
    let error = joined.as_ref().and_then(|j| j.as_ref().err());
    let thread = thread.log_name();
    match outcome {
        // Windows' real-time grant is a thread priority, not a scheduler.
        Priority::Granted => match Os::current() {
            Os::Other => info!(thread, priority, "runs SCHED_FIFO"),
            Os::Windows => info!(thread, priority, "runs at the real-time thread priority"),
        },
        Priority::Mmcss => info!(
            thread,
            mmcss_task = ?task,
            "runs in MMCSS Pro Audio at AVRT_PRIORITY_HIGH (the normal timing profile on Windows)"
        ),
        Priority::MmcssPriorityRefused => warn!(
            thread,
            mmcss_task = ?task,
            error = ?error,
            "runs in MMCSS Pro Audio, but AVRT_PRIORITY_HIGH was refused (the normal timing profile on Windows)"
        ),
        Priority::Highest => warn!(
            thread,
            error = ?error,
            "MMCSS refused: runs at THREAD_PRIORITY_HIGHEST (the normal timing profile on Windows)"
        ),
        Priority::Normal => info!(
            thread,
            "runs at normal priority (the normal timing profile)"
        ),
        Priority::Refused => warn!(
            thread,
            asked = ?asked,
            priority,
            error = ?error,
            "the asked priority was refused: runs at normal priority"
        ),
    }
}

/// Why [`join_mmcss`] failed: the join itself, or the priority after it.
#[derive(Debug)]
pub enum MmcssError {
    /// `AvSetMmThreadCharacteristicsW` refused (its service stopped, or no
    /// MMCSS on this OS, [`NO_MMCSS`]): the thread is not in MMCSS.
    Join(std::io::Error),
    /// The thread joined (MMCSS's task `index`), but
    /// `AvSetMmThreadPriority(AVRT_PRIORITY_HIGH)` refused: it is in MMCSS
    /// at the task's default priority (dstack #281).
    Priority { index: u32, error: std::io::Error },
}

impl std::fmt::Display for MmcssError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MmcssError::Join(e) => write!(f, "{e}"),
            MmcssError::Priority { index, error } => write!(
                f,
                "joined MMCSS (task index {index}), AVRT_PRIORITY_HIGH refused: {error}"
            ),
        }
    }
}

impl std::error::Error for MmcssError {}

/// On Windows: the calling thread joins MMCSS's "Pro Audio" task at
/// AVRT_PRIORITY_HIGH, for the thread's life, and gets MMCSS's task index;
/// [`MmcssError::Join`] when MMCSS refuses the thread (its service
/// stopped: the thread keeps its priority), [`MmcssError::Priority`] when
/// it joined but the priority was refused. The media threads of the normal
/// timing profile join it at their start (dstack #1); dsend's loopback
/// capture thread (cpal's) joins it from its first callback (dstack #198).
/// It allocates nothing.
#[cfg(windows)]
pub fn join_mmcss() -> Result<u32, MmcssError> {
    use windows_sys::Win32::System::Threading::{
        AVRT_PRIORITY_HIGH, AvSetMmThreadCharacteristicsW, AvSetMmThreadPriority,
    };
    let mut index = 0u32;
    // SAFETY: `MMCSS_TASK` is a NUL-terminated UTF-16 string; `index` is a
    // valid u32 the call writes.
    let handle = unsafe { AvSetMmThreadCharacteristicsW(MMCSS_TASK.as_ptr(), &raw mut index) };
    if handle.is_null() {
        return Err(MmcssError::Join(std::io::Error::last_os_error()));
    }
    // SAFETY: the handle MMCSS just gave this thread.
    if unsafe { AvSetMmThreadPriority(handle, AVRT_PRIORITY_HIGH) } == 0 {
        return Err(MmcssError::Priority {
            index,
            error: std::io::Error::last_os_error(),
        });
    }
    Ok(index)
}

/// MMCSS's task "Pro Audio", NUL-terminated UTF-16 (no allocation: a
/// media thread joins at its start).
#[cfg(windows)]
const MMCSS_TASK: [u16; 10] = [
    0x50, 0x72, 0x6f, 0x20, 0x41, 0x75, 0x64, 0x69, 0x6f, 0, // "Pro Audio\0"
];

/// Elsewhere there is no MMCSS: always [`MmcssError::Join`] with
/// [`NO_MMCSS`].
#[cfg(not(windows))]
pub fn join_mmcss() -> Result<u32, MmcssError> {
    Err(MmcssError::Join(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        NO_MMCSS,
    )))
}

/// [`join_mmcss`]'s error off Windows.
pub const NO_MMCSS: &str = "no MMCSS on this OS";

#[cfg(test)]
mod tests;
