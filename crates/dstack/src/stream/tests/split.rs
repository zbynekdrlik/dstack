//! The pump's lost time split by cause (dstack #354), through the pump: the
//! sleep's oversleep, each drain's time and datagrams, each block's thread
//! CPU time.

use super::*;

use std::net::UdpSocket;

use crate::device::TimingProfile;

use super::drain::{flow_rig, send};

/// A sleep asked before its target and returned late is recorded at the
/// next wake, on the same epoch only; a sleep asked for a time already
/// past (the work ran into it), an unread ask and free pacing record none.
#[test]
fn the_wake_after_a_sleep_records_how_late_the_sleep_returned() {
    let mut r = rig(Map::default(), &[], &[]);
    let wake = r.pump.media(T, 0);
    let ns = |s: u64| ns_of(s, RATE);
    // Asked 10 samples before its target, woke 96 samples (1 ms) past it.
    r.pump.sleeping(ns(wake), Some(ns(wake - 10)), 0);
    let wake = r.pump.media(wake + 96, 0);
    assert_eq!(r.stats.view().oversleep_max_us, 1_000);
    // Taken once: the next wake records only its own sleep.
    let wake = r.pump.media(wake, 0);
    assert_eq!(r.stats.view().oversleep_max_us, 1_000);
    // A target already past at the ask: no sleep, nothing recorded.
    r.pump.sleeping(ns(wake), Some(ns(wake)), 0);
    let wake = r.pump.media(wake + 192, 0);
    assert_eq!(r.stats.view().oversleep_max_us, 1_000);
    // An ask that read no clock: nothing.
    r.pump.sleeping(ns(wake), None, 0);
    let wake = r.pump.media(wake + 192, 0);
    assert_eq!(r.stats.view().oversleep_max_us, 1_000);
    // A new epoch at the wake (the clock moved): nothing.
    r.pump.sleeping(ns(wake), Some(ns(wake - 10)), 0);
    let wake = r.pump.media(wake + 192, 1);
    assert_eq!(r.stats.view().oversleep_max_us, 1_000);
    // Free pacing between the ask and the wake: nothing.
    r.pump.sleeping(ns(wake), Some(ns(wake - 10)), 1);
    r.pump.free();
    let wake = r.pump.media(wake + 192, 1);
    assert_eq!(r.stats.view().oversleep_max_us, 1_000);
    // A later, larger one on the same epoch: the peak.
    r.pump.sleeping(ns(wake), Some(ns(wake - 10)), 1);
    r.pump.media(wake + 192, 1);
    let v = r.stats.view();
    assert_eq!(v.oversleep_max_us, 2_000);
    assert_eq!(r.stats.take_second_split().oversleep_us, 2_000);
    assert_eq!(r.stats.take_second_split().oversleep_us, 0, "afresh");
}

/// Each drain before a block records its own time and the datagrams it
/// read; a pump that writes no RX ring records none.
#[test]
fn each_drain_records_its_time_and_its_datagrams() {
    let mut r = flow_rig(TimingProfile::Normal);
    let t = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    // Three packets wait; one block is due: its drain reads all three.
    send(&r, &t, T);
    send(&r, &t, T + B);
    send(&r, &t, T + 2 * B);
    let began = Instant::now();
    r.pump.media(T + 3 * B + P, 0);
    let took_us = u64::try_from(began.elapsed().as_micros()).unwrap();
    let v = r.stats.view();
    assert_eq!(v.blocks, 1);
    assert_eq!(v.drain_max_datagrams, 3);
    assert!(v.drain_max_us <= took_us, "{} > {took_us}", v.drain_max_us);
    let second = r.stats.take_second_split();
    assert_eq!(
        (second.drain_datagrams, second.drain_us),
        (3, v.drain_max_us)
    );
    // One packet at the next block: that second's peak is 1, the start's
    // stays 3.
    send(&r, &t, T + 3 * B);
    r.pump.media(T + 4 * B + P, 0);
    assert_eq!(r.stats.take_second_split().drain_datagrams, 1);
    assert_eq!(r.stats.view().drain_max_datagrams, 3);
    // The low-latency pump drains nothing: no drain recorded.
    let mut low = flow_rig(TimingProfile::LowLatency);
    send(&low, &t, T);
    low.pump.media(T + B + P, 0);
    let v = low.stats.view();
    assert_eq!((v.blocks, v.drain_max_datagrams), (1, 0));
    assert_eq!(low.stats.take_second_split().drain_datagrams, 0);
}

/// A processor that spins or sleeps `ms` in every block.
struct Busy {
    spin: bool,
    ms: u64,
}

impl Process for Busy {
    fn process(&mut self, _block: &mut Block<'_>) {
        let d = Duration::from_millis(self.ms);
        if self.spin {
            let began = Instant::now();
            while !d.saturating_sub(began.elapsed()).is_zero() {
                std::hint::spin_loop();
            }
        } else {
            std::thread::sleep(d);
        }
    }
}

/// One block of a pump over `processor`; its view.
fn one_block(processor: Busy) -> PumpView {
    let r = rig(Map::default(), &[], &[]);
    let stats = Arc::new(PumpStats::new(TIMING, RATE));
    let mut pump = Pump::new(
        processor,
        Map::default(),
        TIMING,
        24,
        r.rx,
        r.tx,
        Arc::clone(&stats),
    );
    pump.media(T, 0);
    pump.media(T + B + P, 0);
    stats.view()
}

/// A block's thread CPU time beside its wall time (Linux): a block that
/// works has CPU time; a block whose thread is off the CPU (here asleep,
/// in the field preempted) has its wall time and almost no CPU time.
#[cfg(target_os = "linux")]
#[test]
fn a_block_records_its_thread_cpu_time_beside_its_wall_time() {
    let fresh = PumpStats::new(TIMING, RATE).view();
    assert_eq!(fresh.max_process_cpu_us, None, "no block yet");
    let asleep = one_block(Busy {
        spin: false,
        ms: 20,
    });
    let cpu = asleep.max_process_cpu_us.expect("Linux measures it");
    assert!(asleep.max_process_us >= 20_000, "{asleep:?}");
    assert!(cpu < 2_000, "asleep 20 ms: {cpu} us of CPU");
    let working = one_block(Busy { spin: true, ms: 20 });
    let cpu = working.max_process_cpu_us.unwrap();
    // On a loaded box the spin gets part of its 20 ms; never more than
    // its wall time.
    assert!(cpu >= 1_000, "spun 20 ms: {cpu} us of CPU");
    assert!(cpu <= working.max_process_us + 1, "{working:?}");
}

/// Off Linux a block's CPU time is unmeasured: absent, never a 0.
#[cfg(not(target_os = "linux"))]
#[test]
fn a_block_s_cpu_time_is_unmeasured_off_linux() {
    let v = one_block(Busy { spin: true, ms: 2 });
    assert!(v.blocks >= 1, "{v:?}");
    assert_eq!(v.max_process_cpu_us, None);
}
