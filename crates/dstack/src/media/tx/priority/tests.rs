// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

use super::*;
use crate::clock::ThreadPriority as Asked;

/// dlink (#68): under the normal profile a thread at normal priority is
/// what was asked; THREAD_PRIORITY_HIGHEST on Windows (dstack #90)
/// likewise; refused only when a raised priority was asked and refused.
#[test]
fn a_thread_s_priority_is_granted_highest_normal_by_profile_or_refused() {
    for mmcss in [Mmcss::Out, Mmcss::High, Mmcss::PriorityRefused] {
        assert_eq!(
            priority_outcome(Asked::Realtime, true, mmcss),
            Priority::Granted
        );
        assert_eq!(
            priority_outcome(Asked::Realtime, false, mmcss),
            Priority::Refused
        );
        for granted in [false, true] {
            assert_eq!(
                priority_outcome(Asked::Normal, granted, mmcss),
                Priority::Normal
            );
        }
    }
    assert_eq!(
        priority_outcome(Asked::Highest, true, Mmcss::Out),
        Priority::Highest
    );
    assert_eq!(
        priority_outcome(Asked::Highest, false, Mmcss::Out),
        Priority::Refused
    );
    // dstack #1: in MMCSS, whatever the level beneath it; dstack #281: a
    // join whose priority was refused is told apart.
    for granted in [false, true] {
        assert_eq!(
            priority_outcome(Asked::Highest, granted, Mmcss::High),
            Priority::Mmcss
        );
        assert_eq!(
            priority_outcome(Asked::Highest, granted, Mmcss::PriorityRefused),
            Priority::MmcssPriorityRefused
        );
    }
}

/// dstack #281: a failed join and a failed priority after the join are
/// different outcomes, and the task index survives the second.
#[test]
fn a_refused_join_and_a_refused_priority_are_told_apart() {
    assert_eq!(mmcss_of(&Ok(4), None), (Mmcss::High, Some(4)));
    let priority = MmcssError::Priority {
        index: 7,
        error: std::io::Error::other("access denied"),
    };
    assert_eq!(
        priority.to_string(),
        "joined MMCSS (task index 7), AVRT_PRIORITY_HIGH refused: access denied"
    );
    assert_eq!(
        mmcss_of(&Err(priority), None),
        (Mmcss::PriorityRefused, Some(7))
    );
    let join = MmcssError::Join(std::io::Error::other("the service is stopped"));
    assert_eq!(join.to_string(), "the service is stopped");
    assert_eq!(mmcss_of(&Err(join), None), (Mmcss::Out, None));
}

/// dstack #1 (songplayer #285): only the normal profile's Windows threads
/// join MMCSS; the real-time profile is above it, and the normal profile
/// elsewhere asks nothing.
#[test]
fn only_the_normal_profile_on_windows_joins_mmcss() {
    assert!(wants_mmcss(Asked::Highest));
    assert!(!wants_mmcss(Asked::Realtime));
    assert!(!wants_mmcss(Asked::Normal));
}

/// No MMCSS off Windows: a thread never claims to have joined it.
#[cfg(not(windows))]
#[test]
fn off_windows_no_thread_joins_mmcss() {
    let Err(MmcssError::Join(e)) = join_mmcss() else {
        panic!("joined MMCSS off Windows");
    };
    assert_eq!(e.kind(), std::io::ErrorKind::Unsupported);
    assert_eq!(e.to_string(), NO_MMCSS);
}

#[test]
fn each_outcome_has_its_status_word() {
    let words: Vec<_> = Priority::ALL
        .into_iter()
        .map(|p| (p.word(Os::Other), p.word(Os::Windows)))
        .collect();
    assert_eq!(
        words,
        [
            ("sched_fifo", "realtime"),
            ("highest", "highest"),
            ("mmcss_high", "mmcss_high"),
            (
                "mmcss_joined_priority_refused",
                "mmcss_joined_priority_refused"
            ),
            ("normal", "normal"),
            ("refused", "refused"),
        ]
    );
    let threads: Vec<_> = MediaThread::ALL
        .into_iter()
        .map(MediaThread::word)
        .collect();
    assert_eq!(threads, ["pump", "tx", "tx_relay", "rx"]);
    let names: Vec<_> = MediaThread::ALL
        .into_iter()
        .map(MediaThread::log_name)
        .collect();
    assert_eq!(
        names,
        ["engine pump", "TX thread", "TX relay thread", "RX thread"]
    );
}

#[test]
fn an_outcome_s_code_reads_back_and_nothing_else_does() {
    for p in Priority::ALL {
        assert_eq!(Priority::of_code(p.code()), Some(p), "{p:?}");
    }
    assert_eq!(Priority::of_code(0), None, "not started");
    assert_eq!(Priority::of_code(7), None);
}

#[test]
fn the_registry_keeps_each_thread_s_last_outcome_and_task_index() {
    let s = Scheduling::new();
    assert!(s.threads().is_empty(), "nothing started");
    s.record(MediaThread::Rx, Priority::Mmcss, Some(0), Some(19));
    s.record(
        MediaThread::Pump,
        Priority::MmcssPriorityRefused,
        Some(u32::MAX),
        Some(-15),
    );
    s.record(MediaThread::Tx, Priority::Granted, None, None);
    assert_eq!(
        s.threads(),
        [
            Scheduled {
                thread: MediaThread::Pump,
                outcome: Priority::MmcssPriorityRefused,
                mmcss_task: Some(u32::MAX),
                priority: Some(-15),
            },
            Scheduled {
                thread: MediaThread::Tx,
                outcome: Priority::Granted,
                mmcss_task: None,
                priority: None,
            },
            Scheduled {
                thread: MediaThread::Rx,
                outcome: Priority::Mmcss,
                mmcss_task: Some(0),
                priority: Some(19),
            },
        ]
    );
    // A thread started again (a new TX thread): its last outcome.
    s.record(MediaThread::Tx, Priority::Refused, None, Some(0));
    s.record(
        MediaThread::TxRelay,
        Priority::Highest,
        Some(3),
        Some(i32::MIN),
    );
    assert_eq!(
        s.threads()[1..3],
        [
            Scheduled {
                thread: MediaThread::Tx,
                outcome: Priority::Refused,
                mmcss_task: None,
                priority: Some(0),
            },
            Scheduled {
                thread: MediaThread::TxRelay,
                outcome: Priority::Highest,
                mmcss_task: Some(3),
                priority: Some(i32::MIN),
            },
        ]
    );
    // dstack #461: a read-back, then none (not read at the next start).
    s.record(MediaThread::Rx, Priority::Mmcss, Some(0), None);
    assert_eq!(s.threads()[3].priority, None);
}

/// dstack #461: on FOH (mmcss.sys disabled) the join returns a handle with
/// task index `u32::MAX` and raises nothing: neither that index nor a
/// read-back at or under THREAD_PRIORITY_HIGHEST is a thread in MMCSS. A
/// real join reads above it (resolume-snv, 20.11.2026: 18 after the join,
/// 19 at AVRT_PRIORITY_HIGH, in a BelowNormal process).
#[test]
fn a_join_without_mmcss_is_no_thread_in_mmcss() {
    let refused = |index| MmcssError::Priority {
        index,
        error: std::io::Error::other("access denied"),
    };
    // FOH's join: the index alone tells, read or not.
    for read in [None, Some(0), Some(19)] {
        assert_eq!(joined_task(&Ok(NO_MMCSS_TASK), read), None, "{read:?}");
        assert_eq!(mmcss_of(&Ok(NO_MMCSS_TASK), read), (Mmcss::Out, None));
        assert_eq!(
            mmcss_of(&Err(refused(NO_MMCSS_TASK)), read),
            (Mmcss::Out, None)
        );
    }
    assert_eq!(
        joined_task(&Ok(NO_MMCSS_TASK - 1), None),
        Some(u32::MAX - 1)
    );
    // A real index read back above HIGHEST: in MMCSS.
    assert_eq!(mmcss_of(&Ok(135), Some(19)), (Mmcss::High, Some(135)));
    assert_eq!(
        mmcss_of(&Ok(135), Some(WINDOWS_HIGHEST + 1)),
        (Mmcss::High, Some(135))
    );
    assert_eq!(
        mmcss_of(&Err(refused(7)), Some(18)),
        (Mmcss::PriorityRefused, Some(7))
    );
    // Read back at or under HIGHEST: MMCSS raised nothing.
    for read in [WINDOWS_HIGHEST, 0, -15] {
        assert_eq!(joined_task(&Ok(135), Some(read)), None, "{read}");
        assert_eq!(mmcss_of(&Ok(135), Some(read)), (Mmcss::Out, None));
        assert_eq!(mmcss_of(&Err(refused(7)), Some(read)), (Mmcss::Out, None));
    }
    // A refused join stays out whatever reads back.
    let join = MmcssError::Join(std::io::Error::other("the service is stopped"));
    assert_eq!(joined_task(&Err(join), Some(19)), None);
}

/// dstack #461: outside MMCSS a thread whose read-back is under the level
/// it asked sets it again; never in MMCSS (a SetThreadPriority there takes
/// the thread out of MMCSS's band: resolume-snv, 25 back to 8), never with
/// nothing asked or nothing read.
#[test]
fn outside_mmcss_a_priority_read_under_the_asked_one_is_set_again() {
    let highest = Some(WINDOWS_HIGHEST);
    // FOH: the joined threads read their class's base (relative 0).
    assert!(needs_again(highest, Mmcss::Out, Some(0)));
    assert!(needs_again(highest, Mmcss::Out, Some(WINDOWS_HIGHEST - 1)));
    assert!(!needs_again(highest, Mmcss::Out, Some(WINDOWS_HIGHEST)));
    assert!(!needs_again(highest, Mmcss::Out, Some(WINDOWS_HIGHEST + 1)));
    let critical = Some(WINDOWS_TIME_CRITICAL);
    assert!(needs_again(critical, Mmcss::Out, Some(WINDOWS_HIGHEST)));
    assert!(!needs_again(
        critical,
        Mmcss::Out,
        Some(WINDOWS_TIME_CRITICAL)
    ));
    for mmcss in [Mmcss::High, Mmcss::PriorityRefused] {
        assert!(!needs_again(highest, mmcss, Some(0)), "{mmcss:?}");
    }
    assert!(!needs_again(None, Mmcss::Out, Some(0)));
    assert!(!needs_again(highest, Mmcss::Out, None));
}

/// dstack #461: a raised priority holds only when its read-back reaches
/// the level asked; unread or nothing wanted, the ask's own answer.
#[test]
fn a_priority_holds_when_its_read_back_reaches_the_asked_one() {
    let highest = Some(WINDOWS_HIGHEST);
    assert!(held(true, highest, Some(WINDOWS_HIGHEST)));
    assert!(held(true, highest, Some(WINDOWS_HIGHEST + 1)));
    assert!(!held(true, highest, Some(WINDOWS_HIGHEST - 1)));
    assert!(!held(true, highest, Some(0)));
    assert!(!held(false, highest, Some(WINDOWS_HIGHEST)));
    assert!(held(true, highest, None));
    assert!(!held(false, highest, None));
    assert!(held(true, None, Some(0)));
    assert!(!held(false, None, Some(0)));
}

/// No priority is read back off Windows.
#[cfg(not(windows))]
#[test]
fn off_windows_no_priority_is_read_back() {
    assert_eq!(thread_priority(), None);
}

/// A thread's set level reads back as it was set (Windows CI).
#[cfg(windows)]
#[test]
fn a_set_priority_reads_back() {
    std::thread::spawn(|| {
        use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority};
        // SAFETY: the calling thread's pseudo handle.
        assert_ne!(unsafe { SetThreadPriority(GetCurrentThread(), 1) }, 0);
        assert_eq!(thread_priority(), Some(1));
        // SAFETY: as above.
        assert_ne!(
            unsafe { SetThreadPriority(GetCurrentThread(), WINDOWS_HIGHEST) },
            0
        );
        assert_eq!(thread_priority(), Some(WINDOWS_HIGHEST));
    })
    .join()
    .unwrap();
}
