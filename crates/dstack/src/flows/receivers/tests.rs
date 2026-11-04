use super::*;

use std::net::Ipv4Addr;

use mdns_sd::{ServiceEvent, ServiceInfo};

use crate::announce::ARC_SERVICE;
use crate::device::DeviceConfig;
use crate::flows::control::FlowRequest;
use crate::subscribe::directory::{ArcDirectory, Timing};

const TEST_DEVICE: &str = include_str!("../../../../../config/test-device.toml");
const S: u64 = 1_000_000_000;
const RX_IP: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 2);

fn dest(port: u16) -> SocketAddrV4 {
    SocketAddrV4::new(RX_IP, port)
}

fn job(slot: usize, port: u16) -> Job {
    Job {
        slot,
        dest: dest(port),
        device: "peer-device".to_string(),
    }
}

fn flow(slot: usize, port: u16) -> (usize, SocketAddrV4, String) {
    (slot, dest(port), "peer-device".to_string())
}

fn at(ns: u32) -> Settings {
    Settings {
        latency_active_ns: Some(ns),
        ..Settings::default()
    }
}

#[test]
fn a_new_flow_is_read_at_once_then_every_30_s_and_after_a_failure_on_the_backoff() {
    let mut reads = Reads::new();
    assert_eq!(reads.next(0), None, "no flow");
    reads.follow(&[flow(2, 4000)], 5 * S);
    assert_eq!(reads.next(5 * S - 1), None, "due from when it was seen");
    assert_eq!(reads.next(5 * S), Some(job(2, 4000)));
    reads.finished(&job(2, 4000), Outcome::Read, 6 * S);
    assert_eq!(reads.next(6 * S + REREAD_NS - 1), None);
    assert_eq!(reads.next(6 * S + REREAD_NS), Some(job(2, 4000)));
    // Failures back off as the directory's reads do; a read clears them.
    reads.finished(&job(2, 4000), Outcome::Failed, 40 * S);
    let first = 40 * S + retry_after(1);
    assert_eq!(reads.next(first - 1), None);
    assert_eq!(reads.next(first), Some(job(2, 4000)));
    reads.finished(&job(2, 4000), Outcome::Failed, first);
    assert_eq!(reads.next(first + retry_after(2) - 1), None);
    assert_eq!(reads.next(first + retry_after(2)), Some(job(2, 4000)));
    assert!(retry_after(2) > retry_after(1));
    reads.finished(&job(2, 4000), Outcome::Read, 100 * S);
    reads.finished(&job(2, 4000), Outcome::Failed, 100 * S);
    assert_eq!(reads.next(100 * S + retry_after(1)), Some(job(2, 4000)));
    // Following the same flows again changes nothing.
    reads.follow(&[flow(2, 4000)], 200 * S);
    assert_eq!(reads.next(100 * S + retry_after(1) - 1), None);
}

#[test]
fn a_device_whose_record_is_not_in_yet_is_asked_at_the_next_look() {
    let mut reads = Reads::new();
    reads.follow(&[flow(0, 4000)], 0);
    reads.finished(&job(0, 4000), Outcome::Failed, 0);
    let due = retry_after(1);
    reads.finished(&job(0, 4000), Outcome::NotYet, due);
    assert_eq!(reads.next(due), None, "not again in the same look");
    assert_eq!(reads.next(due + 1), Some(job(0, 4000)));
    // No failure counted: the next failure is still the second.
    reads.finished(&job(0, 4000), Outcome::Failed, due + 1);
    assert_eq!(reads.next(due + 1 + retry_after(2) - 1), None);
    assert_eq!(reads.next(due + 1 + retry_after(2)), Some(job(0, 4000)));
}

#[test]
fn a_slot_with_another_receiver_is_read_again_and_a_gone_one_forgotten() {
    let mut reads = Reads::new();
    reads.follow(&[flow(0, 4000), flow(1, 4001)], 0);
    reads.finished(&job(0, 4000), Outcome::Failed, 0);
    reads.finished(&job(1, 4001), Outcome::Read, 0);
    assert_eq!(reads.next(1), None);
    // Slot 0 sends elsewhere now, slot 1's flow ended.
    reads.follow(&[flow(0, 4002)], 2 * S);
    assert_eq!(reads.next(2 * S), Some(job(0, 4002)));
    // A failure on the new receiver is its first.
    reads.finished(&job(0, 4002), Outcome::Failed, 2 * S);
    assert_eq!(reads.next(2 * S + retry_after(1)), Some(job(0, 4002)));
    reads.finished(&job(0, 4002), Outcome::Read, 3 * S);
    assert_eq!(reads.next(3 * S + REREAD_NS), Some(job(0, 4002)));
    reads.follow(&[], 4 * S);
    assert_eq!(reads.next(u64::MAX), None, "every flow ended");
    // The end of a read for a receiver no longer there, or a slot past the
    // table, changes nothing.
    reads.follow(&[flow(0, 4003)], 5 * S);
    reads.finished(&job(0, 4002), Outcome::Read, 5 * S);
    assert_eq!(reads.next(5 * S), Some(job(0, 4003)));
    reads.finished(&job(MAX_FLOWS, 4003), Outcome::Read, 5 * S);
    // Another device name at the same address is another receiver.
    let other = (0, dest(4003), "other".to_string());
    reads.finished(&job(0, 4003), Outcome::Read, 5 * S);
    reads.follow(&[other], 6 * S);
    assert_eq!(
        reads.next(6 * S).map(|j| j.device),
        Some("other".to_string())
    );
}

#[test]
fn a_device_s_latency_is_the_one_in_effect_else_the_configured_one() {
    assert_eq!(latency_of(&at(10_000_000)), 10_000_000);
    let configured = Settings {
        latency_configured_ns: Some(2_000_000),
        ..Settings::default()
    };
    assert_eq!(latency_of(&configured), 2_000_000);
    let both = Settings {
        latency_configured_ns: Some(2_000_000),
        ..at(1_000_000)
    };
    assert_eq!(latency_of(&both), 1_000_000);
    assert_eq!(latency_of(&Settings::default()), 0);
}

#[test]
fn a_receiver_s_latency_is_read_only_at_its_own_arc_on_the_flow_s_address() {
    let arc = SocketAddrV4::new(RX_IP, 4441);
    let mut asked = Vec::new();
    let mut read = |a: SocketAddrV4| {
        asked.push(a);
        Ok(at(10_000_000))
    };
    assert_eq!(read_latency(Ok(arc), &mut read), Ok(10_000_000));
    assert_eq!(
        read_latency(Err(ArcMiss::NoRecord), &mut read),
        Err(Unread::NoRecord)
    );
    assert_eq!(
        read_latency(Err(ArcMiss::OtherAddress), &mut read),
        Err(Unread::OtherAddress)
    );
    assert_eq!(asked, [arc], "asked only at the receiver's own ARC");
    assert_eq!(
        read_latency(Ok(arc), |_| Ok(Settings::default())),
        Err(Unread::NoLatency)
    );
    let refused = ReadError::Refused {
        op: 0x1100,
        status: 0x0022,
    };
    assert_eq!(
        read_latency(Ok(arc), |_| Err(refused)),
        Err(Unread::Read(refused))
    );
}

#[test]
fn only_a_missing_record_is_tried_again_at_the_next_look() {
    assert_eq!(Unread::NoRecord.outcome(), Outcome::NotYet);
    assert_eq!(Unread::OtherAddress.outcome(), Outcome::Failed);
    assert_eq!(Unread::NoLatency.outcome(), Outcome::Failed);
    let timeout = Unread::Read(ReadError::Timeout { op: 0x1100 });
    assert_eq!(timeout.outcome(), Outcome::Failed);
}

/// A TX table with one flow to `dest(4000)` from peer-device, and the
/// device's ARC record at `arc_ips` (comma-separated), port 4441.
fn rig(arc_ips: &str) -> (Arc<TxShared>, ArcRecords, ArcDirectory, usize) {
    let cfg = DeviceConfig::from_toml(TEST_DEVICE).unwrap();
    let tx = Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, Vec::new().into(), 1, &[]).unwrap());
    let slot = add_flow(&tx, "peer-device", 4000);
    let dir = ArcDirectory::new(Timing::default());
    let records = dir.records();
    let info = ServiceInfo::new(
        ARC_SERVICE,
        "peer-device",
        "peer.local.",
        arc_ips,
        4441,
        None,
    )
    .unwrap();
    records.keep(&ServiceEvent::ServiceResolved(Box::new(
        info.as_resolved_service(),
    )));
    (tx, records, dir, slot)
}

/// Adds a flow to `dest(port)` asked by `device`; its slot.
fn add_flow(tx: &TxShared, device: &str, port: u16) -> usize {
    let r = FlowRequest {
        rx_host: device.to_string(),
        rx_flow_name: "flow-1".to_string(),
        sample_rate: tx.cfg.sample_rate,
        bits: 24,
        channels: vec![Some(1)],
        fpp: 16,
        rx: dest(port),
    };
    let mut c = tx.lock();
    let (slot, _) = c.table.add(&r, tx.now_ns()).unwrap();
    tx.publish(&c);
    slot
}

fn go() -> AtomicBool {
    AtomicBool::new(false)
}

#[test]
fn a_look_hands_each_flow_s_receiver_latency_to_the_tx_side() {
    let (tx, records, _dir, slot) = rig("127.0.0.2");
    let mut reads = Reads::new();
    let mut asked = 0;
    look(&mut reads, &tx, &records, 0, &go(), |a| {
        assert_eq!(a, SocketAddrV4::new(RX_IP, 4441));
        asked += 1;
        Ok(at(10_000_000))
    });
    assert_eq!(asked, 1);
    assert_eq!(tx.receiver_latency_ns(slot, dest(4000)), 10_000_000);
    assert_eq!(tx.lane(slot).receiver_latency_ns, 10_000_000);
    // Nothing due before the re-read.
    look(&mut reads, &tx, &records, REREAD_NS - 1, &go(), |_| {
        asked += 1;
        Ok(at(1))
    });
    assert_eq!(asked, 1);
    // A failed re-read keeps the last latency; a changed one replaces it.
    look(&mut reads, &tx, &records, REREAD_NS, &go(), |_| {
        Err(ReadError::Timeout { op: 0x1100 })
    });
    assert_eq!(tx.receiver_latency_ns(slot, dest(4000)), 10_000_000);
    let later = REREAD_NS + retry_after(1);
    look(&mut reads, &tx, &records, later, &go(), |_| {
        Ok(at(4_000_000))
    });
    assert_eq!(tx.receiver_latency_ns(slot, dest(4000)), 4_000_000);
}

#[test]
fn a_device_several_flows_go_to_is_read_once_a_look() {
    let (tx, records, _dir, first) = rig("127.0.0.2");
    let second = add_flow(&tx, "PEER-device", 4001);
    let mut reads = Reads::new();
    let mut asked = 0;
    look(&mut reads, &tx, &records, 0, &go(), |_| {
        asked += 1;
        Ok(at(10_000_000))
    });
    assert_eq!(asked, 1, "one read for both flows");
    assert_eq!(tx.receiver_latency_ns(first, dest(4000)), 10_000_000);
    assert_eq!(tx.receiver_latency_ns(second, dest(4001)), 10_000_000);
    // Both are due again together: still one read. A failure counts for
    // both.
    look(&mut reads, &tx, &records, REREAD_NS, &go(), |_| {
        asked += 1;
        Err(ReadError::Timeout { op: 0x1100 })
    });
    assert_eq!(asked, 2);
    assert_eq!(reads.next(REREAD_NS + retry_after(1) - 1), None);
    // Another device at the same address is read on its own.
    let other = add_flow(&tx, "other", 4002);
    let info = ServiceInfo::new(ARC_SERVICE, "other", "o.local.", "127.0.0.2", 4442, None);
    records.keep(&ServiceEvent::ServiceResolved(Box::new(
        info.unwrap().as_resolved_service(),
    )));
    let mut ports = Vec::new();
    look(&mut reads, &tx, &records, REREAD_NS + 1, &go(), |a| {
        ports.push(a.port());
        Ok(at(2_000_000))
    });
    assert_eq!(ports, [4442]);
    assert_eq!(tx.receiver_latency_ns(other, dest(4002)), 2_000_000);
}

#[test]
fn two_devices_at_one_address_are_each_read_in_the_same_look() {
    let (tx, records, _dir, first) = rig("127.0.0.2");
    let other = add_flow(&tx, "other", 4002);
    let info = ServiceInfo::new(ARC_SERVICE, "other", "o.local.", "127.0.0.2", 4442, None);
    records.keep(&ServiceEvent::ServiceResolved(Box::new(
        info.unwrap().as_resolved_service(),
    )));
    let mut reads = Reads::new();
    look(&mut reads, &tx, &records, 0, &go(), |a| {
        Ok(at(if a.port() == 4441 {
            10_000_000
        } else {
            2_000_000
        }))
    });
    assert_eq!(tx.receiver_latency_ns(first, dest(4000)), 10_000_000);
    assert_eq!(tx.receiver_latency_ns(other, dest(4002)), 2_000_000);
}

#[test]
fn a_stopped_look_reads_nothing_more() {
    let (tx, records, _dir, slot) = rig("127.0.0.2");
    let mut reads = Reads::new();
    let stop = AtomicBool::new(true);
    look(&mut reads, &tx, &records, 0, &stop, |_| {
        panic!("never asked")
    });
    assert_eq!(tx.receiver_latency_ns(slot, dest(4000)), 0);
    assert_eq!(reads.next(0), Some(job(slot, 4000)), "still due");
}

#[test]
fn a_receiver_s_latency_is_logged_when_read_first_and_when_it_changes() {
    let (tx, records, _dir, slot) = rig("127.0.0.2");
    let mut reads = Reads::new();
    let info = |now: u64, reads: &mut Reads, ns: u32| {
        crate::test_log::capture_at(tracing::Level::INFO, || {
            look(reads, &tx, &records, now, &go(), |_| Ok(at(ns)));
        })
    };
    let first = info(0, &mut reads, 10_000_000);
    assert!(
        first.contains(&format!("slot={slot} latency_us=10000")),
        "{first}"
    );
    assert!(first.contains("a TX flow's receiver states its latency"));
    assert_eq!(info(REREAD_NS, &mut reads, 10_000_000), "", "unchanged");
    let changed = info(2 * REREAD_NS, &mut reads, 2_000_000);
    assert!(changed.contains("latency_us=2000"), "{changed}");
}

#[test]
fn a_receiver_whose_record_is_at_another_address_stays_unknown() {
    let (tx, records, _dir, slot) = rig("127.0.0.3");
    let mut reads = Reads::new();
    look(&mut reads, &tx, &records, 0, &go(), |_| {
        panic!("never asked")
    });
    assert_eq!(tx.receiver_latency_ns(slot, dest(4000)), 0);
    assert_eq!(tx.lane(slot).receiver_latency_ns, 0);
    // A failure: not asked again at the next look.
    assert_eq!(reads.next(1), None);
}

/// A PC with several network cards: its record's lowest address is not
/// the flow's, and its latency is still read, at the flow's address.
#[test]
fn a_receiver_with_several_addresses_is_read_at_the_flow_s() {
    let (tx, records, _dir, slot) = rig("127.0.0.1,127.0.0.2");
    let mut reads = Reads::new();
    look(&mut reads, &tx, &records, 0, &go(), |a| {
        assert_eq!(a, SocketAddrV4::new(RX_IP, 4441));
        Ok(at(10_000_000))
    });
    assert_eq!(tx.receiver_latency_ns(slot, dest(4000)), 10_000_000);
}

#[test]
fn a_receiver_without_a_record_is_looked_up_again_at_the_next_look() {
    let (tx, _records, _dir, slot) = rig("127.0.0.2");
    let empty = ArcDirectory::new(Timing::default());
    let mut reads = Reads::new();
    look(&mut reads, &tx, &empty.records(), 0, &go(), |_| {
        panic!("never asked")
    });
    assert_eq!(reads.next(1), Some(job(slot, 4000)));
}

#[test]
fn the_reader_thread_reads_with_its_stop_and_ends_when_dropped() {
    let (tx, records, _dir, slot) = rig("127.0.0.2");
    let reader = spawn(
        Arc::clone(&tx),
        records,
        Duration::from_millis(1),
        |_, stop| {
            assert!(!stop.load(Ordering::Relaxed));
            Ok(at(10_000_000))
        },
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while tx.receiver_latency_ns(slot, dest(4000)) == 0 {
        assert!(Instant::now() < deadline, "never read");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(tx.receiver_latency_ns(slot, dest(4000)), 10_000_000);
    let stop = Arc::clone(&reader.stop);
    drop(reader);
    assert!(stop.load(Ordering::Relaxed), "asked to stop");
    // The thread holds the stop until it ends.
    while Arc::strong_count(&stop) > 1 {
        assert!(Instant::now() < deadline, "the thread never ended");
        std::thread::sleep(Duration::from_millis(1));
    }
}
