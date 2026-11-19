// SPDX-License-Identifier: MIT OR Apache-2.0

use std::net::{Ipv4Addr, UdpSocket};
use std::time::Duration;

use dstack::clock::ManualClock;
use dstack::media::ring::Ring;
use dstack::media::rx::{RxPlan, RxRoute};
use dstack::media::tx::ns_of;

use super::*;

const RATE: u32 = 48_000;
/// Ten years of a grandmaster's uptime at 48 kHz, as a position.
const T: u64 = 10 * 365 * 86_400 * RATE as u64;
/// The device's latency P in the tests, samples.
const P: u64 = 96;

#[test]
fn the_first_pull_starts_at_the_playout_point_and_reads_nothing() {
    assert_eq!(due(None, 1_000, 100), Due::Start(1_000));
}

#[test]
fn a_pull_reads_the_gap_up_to_the_fifo_s_capacity() {
    assert_eq!(
        due(Some(1_000), 1_480, 1_000),
        Due::Read {
            from: 1_000,
            to: 1_480
        }
    );
    // Nothing new: an empty read.
    assert_eq!(
        due(Some(1_000), 1_000, 1_000),
        Due::Read {
            from: 1_000,
            to: 1_000
        }
    );
    // The whole capacity is still read; one frame more starts again.
    assert_eq!(
        due(Some(1_000), 2_000, 1_000),
        Due::Read {
            from: 1_000,
            to: 2_000
        }
    );
    assert_eq!(due(Some(1_000), 2_001, 1_000), Due::Restart(2_001));
}

#[test]
fn a_clock_behind_the_last_read_waits_unless_it_went_too_far_back() {
    assert_eq!(due(Some(1_000), 999, 100), Due::Wait);
    assert_eq!(due(Some(1_000), 900, 100), Due::Wait);
    assert_eq!(due(Some(1_000), 899, 100), Due::Restart(899));
}

#[test]
fn a_pull_needs_a_fifo_channel_per_rx_channel_on_the_device() {
    assert_eq!(check(&[0, 1], 2, 2), Ok(()));
    assert_eq!(
        check(&[0, 1], 2, 1),
        Err("the bridge FIFO has 1 channels for 2 RX channels".to_string())
    );
    assert_eq!(
        check(&[0, 2], 2, 2),
        Err("the app's RX channel 3 is past the device's 2 RX channels".to_string())
    );
    assert_eq!(check(&[1], 2, 1), Ok(()));
}

/// A device's RX side with two channels: channel 0 routed to flow slot 0's
/// channel slot 0 (a flow from 127.0.0.1, read `delay` samples later than
/// P), channel 1 not routed; the app's intake, a FIFO and a clock at
/// media now `T + P` (the playout point at `T`).
struct Rig {
    rings: Arc<[Ring]>,
    rx: Arc<RxShared>,
    clock: Arc<ManualClock>,
    fifo: Arc<Fifo>,
    pull: RxPull,
}

fn rig(delay: u16) -> Rig {
    let rings: Arc<[Ring]> = (0..2).map(|_| Ring::new()).collect::<Vec<_>>().into();
    let rx = Arc::new(RxShared::new(RATE, Ipv4Addr::LOCALHOST, Arc::clone(&rings)).unwrap());
    let plan = RxPlan {
        src: Some(Ipv4Addr::LOCALHOST),
        bps: 3,
        nslots: 1,
        latency: P as u32,
        delay,
        block: 128,
        live: false,
        generation: 1,
        group: None,
    };
    rx.publish(
        |slot| if slot == 0 { plan } else { RxPlan::default() },
        |c| (c == 0).then_some(RxRoute { flow: 0, slot: 0 }),
    );
    let intake = rx.app_intake().expect("the app's claim");
    let clock = Arc::new(ManualClock::new(ns_of(T + P, RATE)));
    let fifo = Arc::new(super::super::fifo_for(2, RATE));
    let pull = RxPull::new(
        intake,
        Arc::clone(&clock) as Arc<dyn PtpClock>,
        Arc::clone(&rx),
        vec![0, 1],
        Arc::clone(&fifo),
        P,
        RATE,
    )
    .expect("the pull");
    Rig {
        rings,
        rx,
        clock,
        fifo,
        pull,
    }
}

/// Sends flow slot 0 a packet of `frames` frames stamped `at`, frame `f`
/// being `(f + 1) << 8`, and waits until it is in the socket.
fn send(rx: &RxShared, at: u64, frames: usize) {
    let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let mut out = [0u8; 1500];
    let (secs, sub) = dstack::clock::stamp(at, RATE);
    let n = dstack::media::packet::write(&mut out, secs, sub, 1, 3, frames, |f, _| {
        (f as i32 + 1) << 8
    });
    sender
        .send_to(&out[..n], rx.local_addr(0).unwrap())
        .unwrap();
    std::thread::sleep(Duration::from_millis(30));
}

/// The FIFO's frames, channel by channel.
fn take(fifo: &Fifo, frames: usize) -> Vec<f64> {
    let mut out = vec![9.0; frames * fifo.channels()];
    assert!(fifo.pop_planar(&mut out, frames), "{} held", fifo.len());
    assert_eq!(fifo.len(), 0, "more than {frames} frames");
    out
}

#[test]
fn a_pull_drains_the_flow_and_reads_the_rings_up_to_the_playout_point() {
    let mut r = rig(0);
    assert_eq!(r.pull.pull(), 0, "the first pull starts there");
    // A packet of 64 frames at T, still in its socket: the pull's own
    // drain writes it, then reads it at T + 64's playout point.
    send(&r.rx, T, 64);
    r.clock.sleep_until(ns_of(T + P + 64, RATE));
    assert_eq!(r.pull.pull(), 64);
    let got = take(&r.fifo, 64);
    for f in 0..64 {
        assert_eq!(got[f], to_f64((f as i32 + 1) << 8), "frame {f}");
        assert_eq!(got[64 + f], 0.0, "the unrouted channel, frame {f}");
    }
    assert_eq!(r.rings[0].counters().written, 64);
    assert_eq!(r.rings[0].counters().holes, 0);
    assert_eq!(r.pull.stats().frames(), 64);
    // A clock at the same point reads nothing more.
    assert_eq!(r.pull.pull(), 0);
    // No packet for the next 32: a counted hole, read as silence.
    r.clock.sleep_until(ns_of(T + P + 96, RATE));
    assert_eq!(r.pull.pull(), 32);
    assert_eq!(take(&r.fifo, 32), vec![0.0; 64]);
    assert_eq!(r.rings[0].counters().holes, 32);
    assert_eq!(r.rings[1].counters(), Default::default(), "never read");
    assert_eq!(r.pull.stats().restarts(), 0);
}

#[test]
fn a_flow_read_later_than_p_is_read_at_its_own_delay() {
    let mut r = rig(16);
    assert_eq!(r.pull.pull(), 0);
    send(&r.rx, T, 64);
    r.clock.sleep_until(ns_of(T + P + 64, RATE));
    assert_eq!(r.pull.pull(), 64);
    let got = take(&r.fifo, 64);
    // Positions T - 16 to T + 48: 16 before the flow's first sample, then
    // its first 48.
    assert_eq!(got[..16], [0.0; 16]);
    for (f, &x) in got.iter().enumerate().take(64).skip(16) {
        assert_eq!(x, to_f64((f as i32 - 15) << 8), "frame {f}");
    }
    assert_eq!(r.rings[0].counters().start_holes, 16);
}

#[test]
fn a_pull_reads_at_most_the_fifo_or_half_a_ring() {
    // 0.5 s FIFOs: 24 000 frames at 48 kHz, 96 000 at 192 kHz (past the
    // ring's 65 536: half a ring).
    assert_eq!(reach(24_000), 24_000);
    assert_eq!(reach(SLOTS / 2), SLOTS / 2);
    assert_eq!(reach(SLOTS / 2 + 1), SLOTS / 2);
    assert_eq!(reach(96_000), 32_768);
    assert_eq!(reach(super::super::fifo_for(2, 192_000).capacity()), 32_768);
}

#[test]
fn an_idle_drain_keeps_the_flow_heard_and_the_next_pull_starts_afresh() {
    let mut r = rig(0);
    let stats = r.pull.stats();
    assert_eq!(r.pull.pull(), 0);
    r.clock.sleep_until(ns_of(T + P + 10, RATE));
    assert_eq!(r.pull.pull(), 10);
    assert_eq!(stats.pulls(), 2);
    // The consumer stops; another thread drains: the packet is read (the
    // flow is heard) but placed nowhere, and nothing goes into the FIFO.
    send(&r.rx, T + 10, 64);
    r.clock.sleep_until(ns_of(T + P + 74, RATE));
    r.pull.drain_idle();
    let c = r.rx.lane(0);
    assert_eq!((c.packets, c.unplaced, c.late), (1, 1, 0), "{c:?}");
    assert_eq!(r.rings[0].counters().written, 0);
    assert_eq!(r.fifo.len(), 10);
    assert_eq!((stats.pulls(), stats.idle_drains()), (2, 1));
    // A packet 200 behind (past P + a block of 128, within P + the time
    // since the idle drain) at the next pull: not late, placed.
    send(&r.rx, T + P + 300 - 200 - P, 4);
    r.clock.sleep_until(ns_of(T + P + 300, RATE));
    // The next pull starts at now: nothing read, no restart.
    assert_eq!(r.pull.pull(), 0);
    let c = r.rx.lane(0);
    assert_eq!((c.packets, c.unplaced, c.late), (2, 1, 0), "{c:?}");
    r.clock.sleep_until(ns_of(T + P + 304, RATE));
    assert_eq!(r.pull.pull(), 4);
    assert_eq!(stats.restarts(), 0);
}

#[test]
fn an_idle_drain_empties_a_backlog_past_one_drain() {
    let mut r = rig(0);
    assert_eq!(r.pull.pull(), 0);
    let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let mut out = [0u8; 1500];
    let backlog = dstack::media::rx::PUMP_DRAIN as u64 + 40;
    for k in 0..backlog {
        let (secs, sub) = dstack::clock::stamp(T + k, RATE);
        let n = dstack::media::packet::write(&mut out, secs, sub, 1, 3, 1, |_, _| 1 << 8);
        sender
            .send_to(&out[..n], r.rx.local_addr(0).unwrap())
            .unwrap();
    }
    std::thread::sleep(Duration::from_millis(50));
    r.clock.sleep_until(ns_of(T + P + backlog, RATE));
    r.pull.drain_idle();
    let c = r.rx.lane(0);
    assert_eq!((c.packets, c.unplaced), (backlog, backlog), "{c:?}");
    // A second look, nothing waiting: counted, the read position forgotten.
    r.pull.drain_idle();
    assert_eq!(r.pull.stats().idle_drains(), 2);
    assert_eq!(
        format!("{:?}", r.pull),
        format!("RxPull {{ channels: [0, 1], next: None, playout: {P}, rate: {RATE}, .. }}")
    );
}

#[test]
fn the_idle_watch_sees_a_consumer_stop_and_come_back_and_the_restarts() {
    let mut w = IdleWatch::default();
    let look = |idle, changed, restarts_rose| Look {
        idle,
        changed,
        restarts_rose,
    };
    // The first look: idle until the consumer pulled once.
    assert_eq!(w.look(0, 0), look(true, true, 0));
    assert_eq!(w.look(5, 0), look(false, true, 0));
    assert_eq!(w.look(9, 2), look(false, false, 2));
    assert_eq!(w.look(9, 2), look(true, true, 0), "stopped");
    assert_eq!(w.look(9, 3), look(true, false, 1));
    assert_eq!(w.look(10, 3), look(false, true, 0), "back");
}

#[test]
fn a_read_longer_than_a_callback_is_read_whole() {
    let mut r = rig(0);
    assert_eq!(r.pull.pull(), 0);
    let frames = 2_000;
    r.clock.sleep_until(ns_of(T + P + frames, RATE));
    assert_eq!(r.pull.pull(), frames as usize);
    assert_eq!(r.fifo.len(), frames as usize);
    assert_eq!(r.pull.stats().frames(), frames);
    assert_eq!(r.rings[0].counters().start_holes, frames);
}

#[test]
fn a_gap_past_the_fifo_starts_again_and_an_unready_clock_starts_afresh() {
    let mut r = rig(0);
    assert_eq!(r.pull.pull(), 0);
    let room = r.fifo.capacity() as u64;
    r.clock.sleep_until(ns_of(T + P + room + 1, RATE));
    assert_eq!(r.pull.pull(), 0, "too far: nothing read");
    assert_eq!(r.pull.stats().restarts(), 1);
    r.clock.sleep_until(ns_of(T + P + room + 11, RATE));
    assert_eq!(r.pull.pull(), 10, "on from the restart");
    // The clock not ready: the sockets drained, nothing read, and the
    // next ready pull starts again without counting a restart.
    send(&r.rx, T + room + 11, 8);
    r.clock.set_ready(false);
    assert_eq!(r.pull.pull(), 0);
    assert_eq!(r.rx.lane(0).unplaced, 1, "drained while not ready");
    r.clock.set_ready(true);
    r.clock.sleep_until(ns_of(T + P + room + 50, RATE));
    assert_eq!(r.pull.pull(), 0, "starts afresh");
    r.clock.sleep_until(ns_of(T + P + room + 55, RATE));
    assert_eq!(r.pull.pull(), 5);
    assert_eq!(r.pull.stats().restarts(), 1);
}

/// dstack #440 on the pull: the media clock a step back (100 ms, 400 ms,
/// both within the FIFO's reach) left the pull waiting the whole step out
/// for the clock to come back to where the last read ended, the consumer's
/// FIFO unfed for that long. A wait that far ahead is the clock gone back
/// (the clock-back rule of `Blocks::next`, ROZHODNUTÉ 19.11): the pull
/// starts again at once at the new playout point, counted, and reads on
/// from there.
#[test]
fn a_clock_a_step_back_starts_the_pull_again_at_once() {
    for back in [4_800u64, 19_200] {
        let mut r = rig(0);
        assert!(
            back < reach(r.fifo.capacity()) as u64,
            "{back} within the reach"
        );
        assert_eq!(r.pull.pull(), 0);
        r.clock.advance(ns_of(480, RATE));
        assert_eq!(r.pull.pull(), 480, "{back} back");
        assert!(r.fifo.pop_planar(&mut [0.0; 960], 480));
        r.clock
            .re_anchor(-i64::try_from(ns_of(back, RATE)).unwrap());
        assert_eq!(r.pull.pull(), 0, "{back} back: nothing read at the step");
        assert_eq!(r.pull.stats().restarts(), 1, "{back} back");
        r.clock.advance(ns_of(480, RATE));
        assert_eq!(r.pull.pull(), 480, "{back} back: on from the new time");
    }
}

/// The pull's room behind its position is a clock handover's step
/// (`dstack::clock::linked::HANDOVER_NS`, 30 µs; ROZHODNUTÉ 19.11 on
/// #440): the clock's only backward move with no new epoch. At 48 kHz that
/// is 1.44 samples, so 2 samples back (the most two reads 30 µs apart can
/// differ) wait, and 3 start the pull again, counted, which then reads on
/// from the new playout point (the 3 positions again).
#[test]
fn a_step_back_within_a_handover_waits_and_one_sample_more_restarts() {
    for (back, restarts, reads) in [(2u64, 0u64, 10usize), (3, 1, 13)] {
        let mut r = rig(0);
        assert_eq!(r.pull.pull(), 0);
        r.clock.sleep_until(ns_of(T + P + 480, RATE));
        assert_eq!(r.pull.pull(), 480);
        let step = i64::try_from(ns_of(T + P + 480 - back, RATE)).unwrap()
            - i64::try_from(ns_of(T + P + 480, RATE)).unwrap();
        r.clock.re_anchor(step);
        assert_eq!(r.pull.pull(), 0, "{back} back");
        assert_eq!(r.pull.stats().restarts(), restarts, "{back} back");
        r.clock.sleep_until(ns_of(T + P + 490, RATE));
        assert_eq!(r.pull.pull(), reads, "{back} back");
    }
}

/// A P that Dante Controller raises moves the playout point back: within
/// the room the pull waits it out, past it the pull starts again once,
/// counted (as the pump re-bootstraps at a new P), and reads on from the
/// new playout point.
#[test]
fn a_playout_raised_past_the_room_restarts_the_pull_once() {
    for (more, restarts, reads) in [(2u64, 0u64, 8usize), (4, 1, 10)] {
        let mut r = rig(0);
        assert_eq!(r.pull.pull(), 0);
        r.clock.sleep_until(ns_of(T + P + 480, RATE));
        assert_eq!(r.pull.pull(), 480);
        r.rx.set_device_latency(ns_of(P + more, RATE));
        assert_eq!(r.pull.pull(), 0, "{more} more");
        assert_eq!(r.pull.stats().restarts(), restarts, "{more} more");
        r.clock.sleep_until(ns_of(T + P + 490, RATE));
        assert_eq!(r.pull.pull(), reads, "{more} more");
        assert_eq!(r.pull.stats().restarts(), restarts, "{more} more");
    }
}

#[test]
fn the_device_s_published_latency_moves_the_playout_point() {
    let mut r = rig(0);
    assert_eq!(r.pull.pull(), 0);
    // Dante Controller set 4 samples more: the playout point is 4 back.
    r.rx.set_device_latency(ns_of(P + 4, RATE));
    r.clock.sleep_until(ns_of(T + P + 20, RATE));
    assert_eq!(r.pull.pull(), 16);
}

/// A pull of `channels` into a FIFO of `fifo` channels, on a device of two
/// RX channels: its channels, or why `RxPull::new` refused it.
fn fit(channels: Vec<usize>, fifo: usize) -> Result<Vec<usize>, String> {
    let rings: Arc<[Ring]> = (0..2).map(|_| Ring::new()).collect::<Vec<_>>().into();
    let rx = Arc::new(RxShared::new(RATE, Ipv4Addr::LOCALHOST, rings).unwrap());
    let pull = RxPull::new(
        rx.app_intake().expect("the app's claim"),
        Arc::new(ManualClock::new(0)) as Arc<dyn PtpClock>,
        rx,
        channels,
        Arc::new(super::super::fifo_for(fifo, RATE)),
        P,
        RATE,
    )?;
    Ok(pull.channels().to_vec())
}

/// A pull whose FIFO has not one channel per RX channel read would index
/// past its scratch at its first read (on the consumer's thread with the
/// deadline): `RxPull::new` refuses it, and an RX channel past the
/// device's.
#[test]
fn a_pull_that_does_not_fit_its_fifo_or_device_is_refused() {
    assert_eq!(
        fit(vec![0], 2),
        Err("the bridge FIFO has 2 channels for 1 RX channels".to_string())
    );
    assert_eq!(
        fit(vec![0, 2], 2),
        Err("the app's RX channel 3 is past the device's 2 RX channels".to_string())
    );
    assert_eq!(fit(vec![1, 0], 2), Ok(vec![1, 0]));
}

mod shared;
