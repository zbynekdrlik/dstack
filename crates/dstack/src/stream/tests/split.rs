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
    // Asked 0.1 ms before its target, woke 1 ms past it.
    s.slept(10_000_000, Some(9_900_000), Some(11_000_000), true);
    assert_eq!(s.view().oversleep_max_us, 1_000);
    // A target already past at the ask: no sleep, nothing recorded.
    s.slept(10_000_000, Some(10_000_000), Some(18_000_000), true);
    // A clock not read before or after: nothing.
    s.slept(10_000_000, None, Some(18_000_000), true);
    s.slept(10_000_000, Some(9_900_000), None, true);
    // A new epoch across the sleep (the clock moved): nothing.
    s.slept(10_000_000, Some(9_900_000), Some(18_000_000), false);
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
