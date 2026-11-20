// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The media threads' scheduling: what each asks ([`take_priority`], by its
//! timing profile on this OS), what it got ([`priority_outcome`]), MMCSS on
//! Windows ([`join_mmcss`]), and a device's record of it ([`Scheduling`],
//! dstack #281): each engine thread's outcome and MMCSS task index, which
//! the status shows (`engine.threads[]`), so an app can tell a pump that
//! MMCSS dropped from one that was starved.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use tracing::{info, warn};

use super::TX_PRIORITY;
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

/// The task index MMCSS answers when it is absent (dstack #461: FOH with
/// `mmcss.sys` disabled): `AvSetMmThreadCharacteristicsW` still returns a
/// handle, with this index, and raises nothing.
pub const NO_MMCSS_TASK: u32 = u32::MAX;

/// MMCSS's task index of a thread whose join answered `joined` and whose
/// priority read back `read` right after it ([`thread_priority`]), when
/// the thread is in MMCSS; `None` when it is not (dstack #461): the join
/// refused, the index is [`NO_MMCSS_TASK`], or the read-back is not above
/// THREAD_PRIORITY_HIGHEST (a joined thread reads its MMCSS level, 18 to
/// 19 in a BelowNormal process on resolume-snv, 20.11.2026). With no
/// read-back (off Windows, or the read failed) the index alone decides.
pub fn joined_task(joined: &Result<u32, MmcssError>, read: Option<i32>) -> Option<u32> {
    match joined {
        Ok(index) | Err(MmcssError::Priority { index, .. })
            if *index != NO_MMCSS_TASK && read.is_none_or(|level| level > WINDOWS_HIGHEST) =>
        {
            Some(*index)
        }
        _ => None,
    }
}

/// What [`join_mmcss`]'s answer `joined` and the priority read back after
/// it (`read`, [`joined_task`]) mean for the thread, and MMCSS's task
/// index when it is in MMCSS.
pub(crate) fn mmcss_of(
    joined: &Result<u32, MmcssError>,
    read: Option<i32>,
) -> (Mmcss, Option<u32>) {
    match (joined_task(joined, read), joined) {
        (None, _) => (Mmcss::Out, None),
        (Some(index), Ok(_)) => (Mmcss::High, Some(index)),
        (Some(index), Err(_)) => (Mmcss::PriorityRefused, Some(index)),
    }
}

/// Whether a thread that asked Windows' level `wanted`
/// ([`windows_priority_for`]) and is in `mmcss` sets its priority again
/// after it read back `read` (dstack #461): outside MMCSS, when the
/// read-back is under the level asked (on FOH, with MMCSS absent, the
/// joined threads read their class's base). Never in MMCSS: a
/// SetThreadPriority there takes the thread out of MMCSS's band
/// (resolume-snv, 20.11.2026: 25 back to 8). Nothing asked or nothing read:
/// no.
pub(crate) fn needs_again(wanted: Option<i32>, mmcss: Mmcss, read: Option<i32>) -> bool {
    mmcss == Mmcss::Out && matches!((wanted, read), (Some(w), Some(r)) if r < w)
}

/// Whether a thread whose ask answered `granted` holds Windows' level
/// `wanted` by its read-back `read` (dstack #461): the read-back at least
/// the level asked; with nothing wanted or nothing read, the ask's answer.
pub(crate) fn held(granted: bool, wanted: Option<i32>, read: Option<i32>) -> bool {
    granted && !matches!((wanted, read), (Some(w), Some(r)) if r < w)
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
    /// The thread's priority as Windows read it back (`GetThreadPriority`,
    /// dstack #461): the level within its process's class, or its MMCSS
    /// level in MMCSS; `None` where it was not read (off Windows).
    pub priority: Option<i32>,
}

/// A device's record of its media threads' scheduling (dstack #281): each
/// thread's last `take_priority` outcome and MMCSS task index, one atomic
/// word each (the outcome's code in the low 8 bits, the task index + 1
/// above them, 0 for none; a word of 0: not started), and its read-back
/// priority (dstack #461).
#[derive(Debug, Default)]
pub struct Scheduling {
    slots: [AtomicU64; 4],
    /// Each thread's read-back priority (dstack #461), [`UNREAD`] for none;
    /// written before its slot's word, read only for a started thread.
    levels: [AtomicI64; 4],
}

/// A [`Scheduling`] level with no read-back: outside `i32`'s range.
const UNREAD: i64 = i64::MIN;

impl Scheduling {
    pub fn new() -> Self {
        Scheduling::default()
    }

    /// `thread` got `outcome` (and MMCSS's task index `task`), and its
    /// priority read back `read` (dstack #461).
    pub fn record(
        &self,
        thread: MediaThread,
        outcome: Priority,
        task: Option<u32>,
        read: Option<i32>,
    ) {
        // The read-back first: a reader that sees the word sees it too.
        let level = read.map_or(UNREAD, i64::from);
        self.levels[thread.slot()].store(level, Ordering::Release);
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
                    priority: i32::try_from(self.levels[thread.slot()].load(Ordering::Acquire))
                        .ok(),
                })
            })
            .collect()
    }
}

/// Asks the priority for the calling media `thread` (its profile's on
/// this OS, [`crate::clock::thread_priority_for`], through
/// [`ask_priority`]), records what it got in `scheduling` and logs it:
/// info when it is what was asked, a warning when a raised priority or
/// MMCSS's was refused (the thread runs on). The thread's priority is read
/// back after the join ([`thread_priority`], dstack #461): a join MMCSS
/// raised nothing for is no join ([`mmcss_of`]), a priority under the one
/// asked outside MMCSS is set again ([`needs_again`]) and read back again,
/// and the outcome follows the read-back ([`held`]).
pub(crate) fn take_priority(thread: MediaThread, priority: i32, scheduling: &Scheduling) {
    let asked =
        crate::clock::thread_priority_for(crate::clock::profile(), crate::clock::Os::current());
    let wanted = windows_priority_for(asked, priority);
    let granted = ask_priority(asked, priority);
    let joined = wants_mmcss(asked).then(join_mmcss);
    let read_after_join = thread_priority();
    let (mmcss, task) = joined
        .as_ref()
        .map_or((Mmcss::Out, None), |j| mmcss_of(j, read_after_join));
    let again = needs_again(wanted, mmcss, read_after_join);
    let (granted, read) = if again {
        (ask_priority(asked, priority), thread_priority())
    } else {
        (granted, read_after_join)
    };
    let outcome = priority_outcome(asked, held(granted, wanted, read), mmcss);
    scheduling.record(thread, outcome, task, read);
    let error = joined.as_ref().and_then(|j| j.as_ref().err());
    let thread = thread.log_name();
    match outcome {
        // Windows' real-time grant is a thread priority, not a scheduler.
        Priority::Granted => match Os::current() {
            Os::Other => info!(thread, priority, "runs SCHED_FIFO"),
            Os::Windows => info!(
                thread,
                priority,
                read_after_join = ?read_after_join,
                set_again = again,
                read = ?read,
                "runs at the real-time thread priority"
            ),
        },
        Priority::Mmcss => info!(
            thread,
            mmcss_task = ?task,
            read = ?read,
            "runs in MMCSS Pro Audio at AVRT_PRIORITY_HIGH (the normal timing profile on Windows)"
        ),
        Priority::MmcssPriorityRefused => warn!(
            thread,
            mmcss_task = ?task,
            read = ?read,
            error = ?error,
            "runs in MMCSS Pro Audio, but AVRT_PRIORITY_HIGH was refused (the normal timing profile on Windows)"
        ),
        Priority::Highest => warn!(
            thread,
            joined = ?joined,
            read_after_join = ?read_after_join,
            set_again = again,
            read = ?read,
            "not in MMCSS (refused, or it raised nothing: MMCSS is off on this PC): runs at THREAD_PRIORITY_HIGHEST (the normal timing profile on Windows)"
        ),
        Priority::Normal => info!(
            thread,
            "runs at normal priority (the normal timing profile)"
        ),
        Priority::Refused => warn!(
            thread,
            asked = ?asked,
            priority,
            joined = ?joined,
            read_after_join = ?read_after_join,
            set_again = again,
            read = ?read,
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

/// The calling thread's priority as Windows reads it back
/// (`GetThreadPriority`, dstack #461): its level within its process's
/// class, or its MMCSS level while MMCSS raises it; `None` when the read
/// fails. Its body is [`thread_priority_of_windows`] (excluded from the
/// mutation gate by that name).
#[cfg(windows)]
pub use self::thread_priority_of_windows as thread_priority;

/// [`thread_priority`] on Windows.
#[cfg(windows)]
pub fn thread_priority_of_windows() -> Option<i32> {
    use windows_sys::Win32::System::Threading::{GetCurrentThread, GetThreadPriority};
    // SAFETY: the calling thread's pseudo handle.
    let level = unsafe { GetThreadPriority(GetCurrentThread()) };
    // THREAD_PRIORITY_ERROR_RETURN (MAXLONG): the read failed.
    (level != i32::MAX).then_some(level)
}

/// Elsewhere no priority is read back: always `None`.
#[cfg(not(windows))]
pub fn thread_priority() -> Option<i32> {
    None
}

/// On Linux: when `asked` is real-time, `SCHED_FIFO` at `priority` for
/// the calling thread (reset in any child it forks); `false` when the
/// kernel refuses it or nothing raised was asked. The RX thread and the
/// pump ask too ([`take_priority`]).
#[cfg(target_os = "linux")]
pub(super) fn ask_priority(asked: ThreadPriority, priority: i32) -> bool {
    if asked != ThreadPriority::Realtime {
        return false;
    }
    let param = libc::sched_param {
        sched_priority: priority,
    };
    // SAFETY: `param` is a valid sched_param the call only reads; pid 0 is
    // the calling thread.
    unsafe {
        libc::sched_setscheduler(0, libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK, &param) == 0
    }
}

/// Windows' thread priority for a media thread asking `priority` (the
/// Linux `SCHED_FIFO` number): THREAD_PRIORITY_TIME_CRITICAL (15; 31 in a
/// REALTIME_PRIORITY_CLASS process) for the TX thread ([`TX_PRIORITY`] and
/// above), THREAD_PRIORITY_HIGHEST (2; 26 there) for the RX thread and the
/// engine's pump, so TX goes first as on Linux (TX 47 > RX 44 > pump 40;
/// dasio, the owner's gen3 list for Windows, 1.11.2026). Both stay above
/// MMCSS's audio threads, which left the pump short of time on the FOH PC
/// (missed blocks and TX holes at W 0.33 ms).
pub fn windows_thread_priority(priority: i32) -> i32 {
    if priority >= TX_PRIORITY {
        WINDOWS_TIME_CRITICAL
    } else {
        WINDOWS_HIGHEST
    }
}

/// THREAD_PRIORITY_TIME_CRITICAL.
pub const WINDOWS_TIME_CRITICAL: i32 = 15;
/// THREAD_PRIORITY_HIGHEST.
pub const WINDOWS_HIGHEST: i32 = 2;

/// Windows' thread priority for a media thread that asks `asked` at
/// `priority` (the Linux `SCHED_FIFO` number): [`windows_thread_priority`]
/// for real-time, THREAD_PRIORITY_HIGHEST within the process's normal
/// class for the normal profile (dstack #90: no privilege, nothing
/// process-wide, no setup asked of the app; the thread then also joins
/// MMCSS 'Pro Audio' on Windows), none at normal.
pub fn windows_priority_for(asked: ThreadPriority, priority: i32) -> Option<i32> {
    match asked {
        ThreadPriority::Realtime => Some(windows_thread_priority(priority)),
        ThreadPriority::Highest => Some(WINDOWS_HIGHEST),
        ThreadPriority::Normal => None,
    }
}

/// On Windows: the thread at [`windows_priority_for`]`(asked, priority)`;
/// `false` when Windows refuses it or nothing raised was asked.
/// Under the normal profile (dlink, #68) the thread also opts out of
/// Windows' power throttling (EcoQoS), which would end its timer waits on
/// the coarse system tick (SongPlayer 31.10: peaks of two 15.6 ms ticks).
/// Our thread only, nothing process-wide.
#[cfg(windows)]
pub(super) fn ask_priority(asked: ThreadPriority, priority: i32) -> bool {
    if asked != ThreadPriority::Realtime && !unthrottle() {
        warn!(
            error = %std::io::Error::last_os_error(),
            "a media thread could not opt out of power throttling: its waits may end late"
        );
    }
    let Some(level) = windows_priority_for(asked, priority) else {
        return false;
    };
    use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority};
    // SAFETY: the calling thread's pseudo handle.
    unsafe { SetThreadPriority(GetCurrentThread(), level) != 0 }
}

/// Takes the calling thread out of Windows' execution-speed throttling;
/// `false` when Windows refuses (before Windows 10 1709).
#[cfg(windows)]
fn unthrottle() -> bool {
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadInformation, THREAD_POWER_THROTTLING_CURRENT_VERSION,
        THREAD_POWER_THROTTLING_EXECUTION_SPEED, THREAD_POWER_THROTTLING_STATE,
        ThreadPowerThrottling,
    };
    let state = THREAD_POWER_THROTTLING_STATE {
        Version: THREAD_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: THREAD_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: 0,
    };
    // SAFETY: the calling thread's pseudo handle; `state` is a valid
    // THREAD_POWER_THROTTLING_STATE of the size given, only read.
    unsafe {
        SetThreadInformation(
            GetCurrentThread(),
            ThreadPowerThrottling,
            (&raw const state).cast(),
            std::mem::size_of::<THREAD_POWER_THROTTLING_STATE>() as u32,
        ) != 0
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
pub(super) fn ask_priority(_asked: ThreadPriority, _priority: i32) -> bool {
    false
}

/// Pins the calling thread, named `thread` in the log, to `cpu` when the
/// device config names one (#72 phase F: the pump and the RX/TX threads on
/// isolated cores); a refusal (no such CPU) leaves it where it was.
pub(crate) fn place(thread: &str, cpu: Option<usize>) {
    let Some(cpu) = cpu else {
        return;
    };
    if pin_to_cpu(cpu) {
        info!(thread, cpu, "pinned to its CPU");
    } else {
        let error = std::io::Error::last_os_error();
        warn!(thread, cpu, %error, "not pinned: the kernel refused that CPU");
    }
}

/// Pins the calling thread to `cpu` alone; `false` when the kernel refuses
/// it (no such CPU, or outside the thread's cpuset).
#[cfg(target_os = "linux")]
pub(super) fn pin_to_cpu(cpu: usize) -> bool {
    if cpu >= crate::device::MAX_CPU {
        return false;
    }
    // SAFETY: an all-zero cpu_set_t is a valid empty set; CPU_SET writes
    // inside it (cpu is below CPU_SETSIZE); pid 0 is the calling thread and
    // the call only reads the set.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(cpu, &mut set);
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) == 0
    }
}

/// On Windows: the thread's affinity to `cpu` alone (the first processor
/// group, 64 CPUs).
#[cfg(windows)]
pub(super) fn pin_to_cpu(cpu: usize) -> bool {
    use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadAffinityMask};
    if cpu >= 64 {
        return false;
    }
    // SAFETY: the pseudo handle of the calling thread and a one-CPU mask.
    unsafe { SetThreadAffinityMask(GetCurrentThread(), 1usize << cpu) != 0 }
}

#[cfg(not(any(target_os = "linux", windows)))]
pub(super) fn pin_to_cpu(_cpu: usize) -> bool {
    false
}

#[cfg(test)]
mod tests;
