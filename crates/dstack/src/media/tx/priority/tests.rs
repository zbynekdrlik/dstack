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
    assert_eq!(mmcss_of(&Ok(4)), (Mmcss::High, Some(4)));
    let priority = MmcssError::Priority {
        index: 7,
        error: std::io::Error::other("access denied"),
    };
    assert_eq!(
        priority.to_string(),
        "joined MMCSS (task index 7), AVRT_PRIORITY_HIGH refused: access denied"
    );
    assert_eq!(mmcss_of(&Err(priority)), (Mmcss::PriorityRefused, Some(7)));
    let join = MmcssError::Join(std::io::Error::other("the service is stopped"));
    assert_eq!(join.to_string(), "the service is stopped");
    assert_eq!(mmcss_of(&Err(join)), (Mmcss::Out, None));
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
    s.record(MediaThread::Rx, Priority::Mmcss, Some(0));
    s.record(
        MediaThread::Pump,
        Priority::MmcssPriorityRefused,
        Some(u32::MAX),
    );
    s.record(MediaThread::Tx, Priority::Granted, None);
    assert_eq!(
        s.threads(),
        [
            Scheduled {
                thread: MediaThread::Pump,
                outcome: Priority::MmcssPriorityRefused,
                mmcss_task: Some(u32::MAX)
            },
            Scheduled {
                thread: MediaThread::Tx,
                outcome: Priority::Granted,
                mmcss_task: None
            },
            Scheduled {
                thread: MediaThread::Rx,
                outcome: Priority::Mmcss,
                mmcss_task: Some(0)
            },
        ]
    );
    // A thread started again (a new TX thread): its last outcome.
    s.record(MediaThread::Tx, Priority::Refused, None);
    s.record(MediaThread::TxRelay, Priority::Highest, Some(3));
    assert_eq!(
        s.threads()[1..3],
        [
            Scheduled {
                thread: MediaThread::Tx,
                outcome: Priority::Refused,
                mmcss_task: None
            },
            Scheduled {
                thread: MediaThread::TxRelay,
                outcome: Priority::Highest,
                mmcss_task: Some(3)
            },
        ]
    );
}
