//! The pump's lost time split by cause (dstack #354), through the pump: the
//! sleep's oversleep, each drain's time and datagrams, each block's thread
//! CPU time.

use super::*;

use std::net::UdpSocket;

use crate::device::TimingProfile;

use super::drain::{flow_rig, send};

/// A sleep asked before its target and returned late is recorded, on one
/// epoch only; a sleep asked for a time already past (the work ran into
/// it), an unread clock and a new epoch record none.
#[test]
fn a_sleep_records_how_late_it_returned() {
    let s = PumpStats::new(TIMING, RATE);
    // Asked 0.1 ms before its target, woke 1 ms past it (returned for the
    // wake after it, dstack #370).
    assert_eq!(
        s.slept(10_000_000, Some(9_900_000), Some(11_000_000), true),
        Some(1_000_000)
    );
    assert_eq!(s.view().oversleep_max_us, 1_000);
    // A target already past at the ask: no sleep, nothing recorded.
    assert_eq!(
        s.slept(10_000_000, Some(10_000_000), Some(18_000_000), true),
        None
    );
    // A clock not read before or after: nothing.
    assert_eq!(s.slept(10_000_000, None, Some(18_000_000), true), None);
    assert_eq!(s.slept(10_000_000, Some(9_900_000), None, true), None);
    // A new epoch across the sleep (the clock moved): nothing.
    assert_eq!(
        s.slept(10_000_000, Some(9_900_000), Some(18_000_000), false),
        None
    );
    // On time: a measured 0.
    assert_eq!(
        s.slept(10_000_000, Some(9_900_000), Some(9_999_000), true),
        Some(0)
    );
    assert_eq!(s.view().oversleep_max_us, 1_000);
    assert_eq!(s.take_second_split().oversleep_us, 1_000);
    // A later, larger one: the peak; the second's afresh.
    s.slept(20_000_000, Some(19_000_000), Some(22_000_999), true);
    assert_eq!(s.view().oversleep_max_us, 2_000);
    assert_eq!(s.take_second_split().oversleep_us, 2_000);
    assert_eq!(s.take_second_split().oversleep_us, 0, "afresh");
}

/// The pump thread records its sleeps: a pump on a running clock has
/// slept within the bound (a wake reads the clock after its target, so
/// the peak is above 0; its size is the scheduler's, so the test proves
/// the wiring only).
#[test]
fn the_pump_thread_records_its_sleeps() {
    use crate::clock::{SkewedClock, TestClockPpm, app_monotonic_ns};
    use std::sync::atomic::AtomicBool;
    let r = rig(Map::default(), &[], &[]);
    let stats = Arc::clone(&r.stats);
    let clock: Arc<dyn PtpClock> =
        Arc::new(SkewedClock::new(app_monotonic_ns(), 0, TestClockPpm(0.0)));
    let stop = Arc::new(AtomicBool::new(false));
    let thread = spawn(clock, r.pump, Arc::clone(&stop)).unwrap();
    let began = Instant::now();
    let mut slept = false;
    while !slept && began.elapsed() < BOUND {
        std::thread::sleep(Duration::from_millis(20));
        slept = stats.oversleep_ns.max() > 0;
    }
    stop.store(true, std::sync::atomic::Ordering::Release);
    thread.join().unwrap();
    assert!(slept, "no sleep recorded: {:?}", stats.view());
}

/// dstack #370: each late wake in the recent ring carries the oversleep
/// of the sleep just before it, taken once: a wake with no measured sleep
/// before it (none, or one asked for a time already past) carries none.
#[test]
fn each_late_wake_carries_its_own_sleep_s_oversleep() {
    let mut r = rig(Map::default(), &[], &[]);
    let w = r.pump.media(T, 0);
    // Asked 0.1 ms before the wake's target, returned 40 samples (416 667
    // ns at 96 kHz) past it, and woke there: late, not past W.
    let target = ns_of(w, RATE);
    r.pump.slept(
        target,
        Some(target - 100_000),
        Some(ns_of(w + 40, RATE)),
        true,
    );
    let w = r.pump.media(w + 40, 0);
    // Late again with no sleep measured in between: none (taken once).
    let w = r.pump.media(w + 50, 0);
    // A sleep 1 ms past its target, the wake a further 0.05 ms on: past
    // W, a re-bootstrap with its own oversleep.
    let target = ns_of(w, RATE);
    r.pump.slept(
        target,
        Some(target - 100_000),
        Some(target + 1_000_000),
        true,
    );
    let w = r.pump.media(w + WORK_MARGIN + 5, 0);
    // A sleep asked for a time already past: unmeasured.
    let target = ns_of(w, RATE);
    r.pump
        .slept(target, Some(target), Some(target + 2_000_000), true);
    r.pump.media(w + 2 * B, 0);
    let late: Vec<_> = r
        .stats
        .wakes()
        .recent()
        .iter()
        .map(|l| (l.behind_us, l.rebootstrap, l.oversleep_us))
        .collect();
    assert_eq!(
        late,
        [
            (416, false, Some(416)),
            (520, false, None),
            (1_052, true, Some(1_000)),
            (666, false, None),
        ]
    );
    // The peak since the start is the largest of them.
    assert_eq!(r.stats.view().oversleep_max_us, 1_000);
}

/// A clock whose every sleep returns [`LateClock::LATE`] past its target,
/// 1 ms of real time a sleep.
#[derive(Debug)]
struct LateClock {
    now: std::sync::atomic::AtomicU64,
}

impl LateClock {
    /// Past W (1 ms): every wake re-bootstraps.
    const LATE: u64 = 2_000_000;
}

impl PtpClock for LateClock {
    fn now_ns(&self) -> Option<u64> {
        Some(self.now.load(std::sync::atomic::Ordering::SeqCst))
    }

    fn sleep_until(&self, ptp_ns: u64) {
        use std::sync::atomic::Ordering::SeqCst;
        let now = self.now.load(SeqCst);
        self.now.store(now.max(ptp_ns) + Self::LATE, SeqCst);
        std::thread::sleep(Duration::from_millis(1));
    }

    fn epoch(&self) -> u32 {
        0
    }
}

/// dstack #370, the pump thread's wiring: a sleep that returns late is
/// kept for the wake after it, so each late wake in the recent ring
/// carries its own oversleep, here the whole of its lateness.
#[test]
fn the_pump_thread_gives_each_late_wake_its_sleep_s_oversleep() {
    use std::sync::atomic::{AtomicBool, AtomicU64};
    let r = rig(Map::default(), &[], &[]);
    let stats = Arc::clone(&r.stats);
    let clock: Arc<dyn PtpClock> = Arc::new(LateClock {
        now: AtomicU64::new(ns_of(T, RATE)),
    });
    let stop = Arc::new(AtomicBool::new(false));
    let thread = spawn(clock, r.pump, Arc::clone(&stop)).unwrap();
    let began = Instant::now();
    while stats.wakes().late_wakes() < 3 && began.elapsed() < BOUND {
        std::thread::sleep(Duration::from_millis(5));
    }
    stop.store(true, std::sync::atomic::Ordering::Release);
    thread.join().unwrap();
    let recent = stats.wakes().recent();
    assert!(recent.len() >= 3, "{recent:?}");
    let us = LateClock::LATE / 1_000;
    for l in &recent {
        assert_eq!(
            (l.behind_us, l.rebootstrap, l.oversleep_us),
            (us, true, Some(us)),
            "{recent:?}"
        );
    }
}

/// Each drain before a block records its own time and the datagrams it
/// read; a pump that writes no RX ring records none.
#[test]
fn each_drain_records_its_time_and_its_datagrams() {
    let mut r = flow_rig(TimingProfile::Normal);
    // The rig's own first drain (`flow_rig`, no datagram) is in the peaks
    // too: its second is taken here, so the next second holds only the
    // drain below, which its own measured time bounds. Under load the
    // rig's drain can take longer than that whole step (dstack #373: 2983
    // us against 102 us), so the lifetime peak is the larger of the two.
    let rig = r.stats.take_second_split();
    assert_eq!(rig.drain_datagrams, 0);
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
    let second = r.stats.take_second_split();
    assert_eq!(second.drain_datagrams, 3);
    assert!(
        second.drain_us <= took_us,
        "{} > {took_us}",
        second.drain_us
    );
    assert_eq!(v.drain_max_us, second.drain_us.max(rig.drain_us), "{rig:?}");
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
    // The CPU window holds the block's wall window and two clock reads.
    assert!(cpu <= working.max_process_us + 50, "{working:?}");
}

/// Off Linux a block's CPU time is unmeasured: absent, never a 0.
#[cfg(not(target_os = "linux"))]
#[test]
fn a_block_s_cpu_time_is_unmeasured_off_linux() {
    let v = one_block(Busy { spin: true, ms: 2 });
    assert!(v.blocks >= 1, "{v:?}");
    assert_eq!(v.max_process_cpu_us, None);
}
