//! dstack #266: an RX flow names its transmitter (the source address the
//! RX thread publishes, and the loss watch's line).

use super::*;

/// dstack #266: a lost packet on a slot whose flow has no name and no
/// packet yet reads `-` for both, never a stale or empty value.
#[test]
fn the_loss_watch_names_an_unknown_transmitter_with_a_dash() {
    use crate::media::late::{EVERY_NS, RxWatch};
    let s = shared(1);
    let mut watch = RxWatch::new();
    watch.watch(0, &s, |_| None);
    s.lanes[0].gaps.fetch_add(1, Ordering::Relaxed);
    let log = logged_warnings(|| watch.watch(EVERY_NS, &s, |_| None));
    assert!(log.contains("device=- source=- slot=0 gaps=1"), "{log}");
}

/// dstack #266: the published source follows the transmitter's port when a
/// later packet of the flow comes from another one.
#[test]
fn the_source_follows_the_transmitter_s_port() {
    let s = shared(1);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    publish(&s, &[(0, plan(LOOPBACK, 1, true, 1))], &[]);
    assert_eq!(s.source(0), None, "no packet yet");
    assert_eq!(s.source(MAX_FLOWS), None, "no such slot");
    let now = Some(ns_of(5_000, RATE));
    let t = transmitter(LOOPBACK);
    t.send_to(&packet_at(5_000, 1, 8), slot_addr(&s, 0))
        .unwrap();
    pump(&mut rx, 0, 1, now, 0);
    assert_eq!(s.source(0), Some(v4(&t)));
    let u = transmitter(LOOPBACK);
    u.send_to(&packet_at(5_008, 1, 8), slot_addr(&s, 0))
        .unwrap();
    pump(&mut rx, 0, 2, now, 0);
    assert_ne!(v4(&t), v4(&u));
    assert_eq!(s.source(0), Some(v4(&u)));
}
