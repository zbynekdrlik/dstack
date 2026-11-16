// SPDX-License-Identifier: MIT OR Apache-2.0

//! The to-ASIO side pulled by the driver's buffer switch (dstack #307): the
//! handover of the pull, which side writes the to-ASIO FIFO, the switch's
//! pull, the refusals of a pull that does not fit, and the app's idle look.

use std::net::{Ipv4Addr, UdpSocket};
use std::time::Duration;

use dstack::clock::{ManualClock, PtpClock};
use dstack::media::ring::Ring;
use dstack::media::rx::{RxPlan, RxRoute, RxShared};
use dstack::media::tx::ns_of;

use super::*;

const NET: u32 = 48_000;
/// Ten years of a grandmaster's uptime at 48 kHz, as a position.
const T: u64 = 10 * 365 * 86_400 * NET as u64;
/// The device's latency P in the tests, samples.
const P: u64 = 96;

/// A device's RX side of one channel, routed to flow slot 0 (a flow from
/// 127.0.0.1), with the app's claim on its rings and a clock at media now
/// `T + P`; its pull writes `fifo` (`None`: `RxPull::new` refused it).
struct Rig {
    rx: Arc<RxShared>,
    clock: Arc<ManualClock>,
    pull: Option<RxPull>,
}

fn rig(fifo: &Arc<Fifo>) -> Rig {
    let rings: Arc<[Ring]> = (0..1).map(|_| Ring::new()).collect::<Vec<_>>().into();
    let rx = Arc::new(RxShared::new(NET, Ipv4Addr::LOCALHOST, rings).unwrap());
    let plan = RxPlan {
        src: Some(Ipv4Addr::LOCALHOST),
        bps: 3,
        nslots: 1,
        latency: P as u32,
        delay: 0,
        block: 128,
        live: false,
        generation: 1,
        group: None,
    };
    rx.publish(
        |slot| if slot == 0 { plan } else { RxPlan::default() },
        |_| Some(RxRoute { flow: 0, slot: 0 }),
    );
    let intake = rx.app_intake().expect("the app's claim");
    let clock = Arc::new(ManualClock::new(ns_of(T + P, NET)));
    let pull = RxPull::new(
        intake,
        Arc::clone(&clock) as Arc<dyn PtpClock>,
        Arc::clone(&rx),
        vec![0],
        Arc::clone(fifo),
        P,
        NET,
    )
    .ok();
    Rig { rx, clock, pull }
}

/// Sends flow slot 0 a packet of `frames` frames stamped `at`, each sample
/// a quarter of full scale, and waits until it is in the socket.
fn send(rx: &RxShared, at: u64, frames: usize) {
    let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let mut out = [0u8; 1500];
    let (secs, sub) = dstack::clock::stamp(at, NET);
    let n = dstack::media::packet::write(&mut out, secs, sub, 1, 3, frames, |_, _| 1 << 29);
    sender
        .send_to(&out[..n], rx.local_addr(0).unwrap())
        .unwrap();
    std::thread::sleep(Duration::from_millis(30));
}

/// A to-ASIO only bridge at 48 kHz, block 128, with its pull handed over.
fn pulled() -> (Bridge, Rig) {
    let mut b = Bridge::new(Spec {
        dante_rate: NET,
        block_frames: 128,
        to_asio: 1,
        from_asio: 0,
        app_buffer_ns: None,
    })
    .unwrap();
    let mut r = rig(&b.to_asio_fifo().unwrap());
    b.pull_to_asio(r.pull.take().unwrap()).unwrap();
    (b, r)
}

/// A driver opened at 48 kHz with a 256-frame buffer, one output.
fn driver() -> Opened {
    Opened {
        rate: 48_000.0,
        buffer_frames: 256,
        driver_inputs: 0,
        driver_outputs: 1,
        inputs: vec![],
        outputs: vec![0],
        input_sample: None,
        output_sample: Sample::Int32,
    }
}

/// The to-ASIO player's producer is the driver's buffer when the switch
/// pulls (one push a switch), the pump's block when the pump writes.
#[test]
fn the_to_asio_producer_is_the_driver_s_buffer_when_pulled_else_the_pump_s_block() {
    let buffer = AppBuffer::new(5_333_333, 2_666_667);
    assert_eq!(to_asio_chunk_ns(buffer, true), 5_333_333);
    assert_eq!(to_asio_chunk_ns(buffer, false), 2_666_667);
}

/// The pull must be the bridge's own: a to-ASIO FIFO, that one, once.
#[test]
fn a_pull_of_another_fifo_a_second_one_or_none_to_asio_is_refused() {
    let mut b = Bridge::new(spec(0, 1)).unwrap();
    let other = Arc::new(fifo_for(1, NET));
    assert_eq!(
        b.pull_to_asio(rig(&other).pull.take().unwrap()),
        Err("the bridge has no channel to ASIO to pull".to_string())
    );
    assert!(b.to_asio_pull_stats().is_none());
    assert_eq!(b.keep_pulled(), None, "nothing to look at");
    let mut b = Bridge::new(spec(1, 0)).unwrap();
    assert_eq!(
        b.pull_to_asio(rig(&other).pull.take().unwrap()),
        Err("the pull writes another FIFO than the bridge's to-ASIO one".to_string())
    );
    let own = b.to_asio_fifo().unwrap();
    assert_eq!(b.pull_to_asio(rig(&own).pull.take().unwrap()), Ok(()));
    assert!(b.to_asio_pull_stats().is_some());
    assert_eq!(
        b.pull_to_asio(rig(&own).pull.take().unwrap()),
        Err("the bridge pulls already".to_string())
    );
}

/// The pump's side writes the to-ASIO FIFO only while nothing pulls it,
/// and only a block that has RX inputs (a pulling app's pump has none).
#[test]
fn the_pump_feeds_the_to_asio_fifo_only_unpulled_and_with_rx_inputs() {
    let mut b = Bridge::new(spec(1, 0)).unwrap();
    let fifo = b.to_asio_fifo().unwrap();
    let mut dante = b.dante_side().unwrap();
    let mut out = vec![0.0; 128];
    dante.process(&mut Block::new(128, &[], &mut out));
    assert_eq!(fifo.len(), 0, "no RX inputs: nothing");
    dante.process(&mut Block::new(128, &[0.5; 128], &mut out));
    assert_eq!(fifo.len(), 128, "the block's RX");
    let mut r = rig(&fifo);
    b.pull_to_asio(r.pull.take().unwrap()).unwrap();
    dante.process(&mut Block::new(128, &[0.5; 128], &mut out));
    assert_eq!(fifo.len(), 128, "pulled: the pump writes nothing");
}

/// Each buffer switch pulls before it plays: it drains the flow's socket
/// and reads the rings up to media now less P into the FIFO.
#[test]
fn the_driver_s_switch_pulls_the_rings_into_the_fifo_before_it_plays() {
    let (mut b, r) = pulled();
    let fifo = b.to_asio_fifo().unwrap();
    let stats = b.to_asio_pull_stats().unwrap();
    let mut asio = b.asio_side(&driver()).unwrap();
    let mut out = vec![1.0; 256];
    asio.process(&mut Block::new(256, &[], &mut out));
    assert_eq!((stats.pulls(), stats.frames()), (1, 0), "the start");
    assert!(out.iter().all(|&x| x == 0.0), "priming: silence");
    send(&r.rx, T, 64);
    r.clock.sleep_until(ns_of(T + P + 64, NET));
    asio.process(&mut Block::new(256, &[], &mut out));
    assert_eq!((stats.pulls(), stats.frames()), (2, 64));
    assert_eq!(fifo.len(), 64, "the player is still priming");
    assert_eq!(r.rx.rings()[0].counters().written, 64);
}

/// While no switch pulls (no driver open), the app's look drains the flow
/// itself; once a switch pulls again it leaves the pull alone.
#[test]
fn the_app_s_look_drains_while_no_switch_pulls() {
    let (mut b, _r) = pulled();
    let stats = b.to_asio_pull_stats().unwrap();
    let (look, poisoned) = b.keep_pulled().unwrap();
    assert!(look.idle && look.changed && !poisoned, "{look:?}");
    assert_eq!(stats.idle_drains(), 1);
    let mut asio = b.asio_side(&driver()).unwrap();
    let mut out = vec![0.0; 256];
    asio.process(&mut Block::new(256, &[], &mut out));
    let (look, _) = b.keep_pulled().unwrap();
    assert!(!look.idle && look.changed, "{look:?}");
    assert_eq!(stats.idle_drains(), 1, "a switch pulled: no drain");
    let (look, _) = b.keep_pulled().unwrap();
    assert!(look.idle && look.changed, "{look:?}");
    assert_eq!(stats.idle_drains(), 2);
}

/// A pull that reads another channel count than the to-ASIO FIFO holds
/// would write past its scratch in the driver's callback: `RxPull::new`
/// refuses it, so no such pull reaches the bridge. A pull handed over after
/// the driver's first open would leave that run's player on the pump's
/// block (dstack #317): refused.
#[test]
fn a_pull_of_another_width_or_after_the_first_open_is_refused() {
    let b = Bridge::new(spec(2, 0)).unwrap();
    let fifo = b.to_asio_fifo().unwrap();
    assert!(rig(&fifo).pull.is_none(), "one RX channel for two");
    let mut b = Bridge::new(spec(1, 0)).unwrap();
    let fifo = b.to_asio_fifo().unwrap();
    let _asio = b.asio_side(&driver()).unwrap();
    assert_eq!(
        b.pull_to_asio(rig(&fifo).pull.take().unwrap()),
        Err("the driver opened already: hand the pull over before its first open".to_string())
    );
    assert!(b.to_asio_pull_stats().is_none());
}
