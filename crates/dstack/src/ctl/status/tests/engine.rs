// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The engine part's dstack #281 keys.

use super::*;

/// dstack #281: the re-bootstraps by cause, the pump's wake lateness and
/// the media threads' scheduling, after the totals.
#[test]
fn the_engine_status_names_the_causes_the_wakes_and_the_threads() {
    use crate::media::tx::{MediaThread, Priority};
    use crate::stream::{Cause, PumpStats};
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_with_engine(&device, Arc::new(Unready), &[0]);
    let stats = Arc::new(PumpStats::new(ENGINE_TIMING, 96_000));
    media.attach_engine(Arc::clone(&stats));
    stats.rebootstrapped(3, Cause::Late);
    stats.rebootstrapped(0, Cause::ClockBack);
    stats.rebootstrapped(0, Cause::ClockBack);
    stats.rebootstrapped(0, Cause::Reanchor);
    stats.rebootstrapped(0, Cause::Reanchor);
    stats.rebootstrapped(0, Cause::Reanchor);
    stats.rebootstrapped(0, Cause::Retime);
    let wakes = stats.wakes();
    wakes.record(100_000, false, false, || 0);
    wakes.record(12_345_678, true, true, || 1_794_000_000_123);
    wakes.take_minute();
    wakes.record(700_000, true, false, || 1_794_000_000_456);
    let scheduling = media.tx.scheduling();
    scheduling.record(MediaThread::Rx, Priority::Refused, None);
    scheduling.record(MediaThread::Pump, Priority::MmcssPriorityRefused, Some(9));
    let s = status(&media);
    for word in words_of(&s, "test-device") {
        assert!(WORDS.contains(&word.as_str()), "{word:?} in {s}");
    }
    let os_free = "\"max_process_us\":0,\"rebootstraps_late\":1,\"rebootstraps_clock_back\":2,\
         \"rebootstraps_reanchor\":3,\"rebootstraps_retime\":1,\"play_at_rejected\":0,\
         \"wake_behind_max_us\":12345,\
         \"wake_behind_minute_max_us\":12345,\"wake_behind_buckets\":[1,1,0,0,0,1,0,0],\
         \"late_wakes\":2,\"late_wakes_recent\":[\
         {\"unix_ms\":1794000000123,\"behind_us\":12345,\"rebootstrap\":1},\
         {\"unix_ms\":1794000000456,\"behind_us\":700,\"rebootstrap\":0}],\
         \"threads\":[{\"thread\":\"pump\",\"scheduling\":\"mmcss_joined_priority_refused\",\
         \"mmcss_task\":9},{\"thread\":\"rx\",\"scheduling\":\"refused\",\"mmcss_task\":null}]}}";
    assert!(s.ends_with(os_free), "{s}");
    assert!(s.contains("\"rebootstraps\":7,"), "{s}");
}
