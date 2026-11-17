// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::signal::unix::{SignalKind, signal};

use super::report;

/// `report` sets the host's flag at a signal on either of its two streams
/// (dstack #369: the move's mutation gate found no test of it). SIGUSR1 and
/// SIGUSR2 stand in for SIGTERM and SIGINT, which would reach every other
/// test's handler in this process; no test here registers either.
#[test]
fn a_signal_on_either_stream_sets_the_host_s_flag() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        for raised in [libc::SIGUSR1, libc::SIGUSR2] {
            let term = signal(SignalKind::user_defined1()).expect("SIGUSR1");
            let int = signal(SignalKind::user_defined2()).expect("SIGUSR2");
            let flag = Arc::new(AtomicBool::new(false));
            let task = tokio::spawn(report(term, int, Arc::clone(&flag)));
            // SAFETY: raise only sends a signal to this process; both have a
            // handler (the streams above), so nothing is terminated.
            assert_eq!(unsafe { libc::raise(raised) }, 0);
            let set = tokio::time::timeout(Duration::from_secs(5), async {
                while !flag.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await;
            task.abort();
            assert!(set.is_ok(), "signal {raised} did not set the flag");
        }
    });
}
