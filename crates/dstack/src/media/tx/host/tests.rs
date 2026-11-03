use super::*;

use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Mutex;

use crate::device::DeviceConfig;
use crate::flows::control::FlowRequest;
use crate::media::bypass::{HEADROOM, Neighbour, write_ethernet, write_headers};
use crate::media::late::HostWatch;
use crate::media::packet::{MEDIA_HEADER, read_header};
use crate::media::source::Source;
use crate::media::tx::Transmitter;

const TEST_DEVICE: &str = include_str!("../../../../../../config/test-device.toml");
const RATE: u32 = 96_000;
/// Ten years of a grandmaster's uptime, ns.
const T: u64 = 10 * 365 * 86_400 * 1_000_000_000;
const OUR_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
const PEER_MAC: [u8; 6] = [0x00, 0x1d, 0xc1, 0x01, 0x02, 0x03];
const IFINDEX: u32 = 7;
const RESOLVED: Ipv4Addr = Ipv4Addr::LOCALHOST;
const UNRESOLVED: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 2);

#[test]
fn a_frame_is_ethernet_then_the_ip_and_udp_headers_then_the_packet() {
    // The bypass test's hand-built IPv4 + UDP headers (DSCP EF, ID
    // 0x1234), behind an Ethernet II header.
    let want: [u8; FRAME_HEADROOM + 3] = [
        0x00, 0x1d, 0xc1, 0x01, 0x02, 0x03, 0x02, 0x00, 0x00, 0x00, 0x00, 0x01, 0x08, 0x00, 0x45,
        0xb8, 0x00, 0x1f, 0x12, 0x34, 0x40, 0x00, 0x40, 0x11, 0x13, 0xe0, 0x0a, 0x00, 0x00, 0x01,
        0x0a, 0x00, 0x00, 0x02, 0x0f, 0xa0, 0x13, 0x88, 0x00, 0x0b, 0xc4, 0xab, 1, 2, 3,
    ];
    let mut buf = [0xeeu8; FRAME_HEADROOM + 3];
    buf[FRAME_HEADROOM..].copy_from_slice(&[1, 2, 3]);
    let n = write_frame(
        &mut buf,
        PEER_MAC,
        OUR_MAC,
        SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 1), 4000),
        SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 2), 5000),
        0x1234,
        3,
    );
    assert_eq!(n, FRAME_HEADROOM + 3);
    assert_eq!(buf, want);
}

#[test]
fn the_start_line_names_the_path_and_its_nic() {
    assert_eq!(
        path_line(TxPath::HostSender, "eth3"),
        "TX path: host frame sender on eth3 (multicast and unresolved receivers on the UDP socket)"
    );
    assert_eq!(TxPath::AfPacket.describe(), "AF_PACKET past the qdisc");
    assert_eq!(TxPath::UdpSocket.describe(), "UDP socket");
}

/// What the test sender takes of a batch.
#[derive(Debug, Clone, Copy)]
enum Take {
    All,
    /// Every second frame (the first, the third, ...).
    Even,
    /// None: the whole batch refused with this OS error (after marking
    /// every frame, which must not count).
    Refuse(i32),
}

/// A host sender that keeps every call's frames (the host's interface:
/// a test implementation of it is the host's side).
struct Recorder {
    take: Take,
    calls: Mutex<Vec<Vec<Vec<u8>>>>,
}

impl FrameSender for Recorder {
    fn send(&self, frames: &[&[u8]], sent: &mut [bool]) -> io::Result<()> {
        assert_eq!(frames.len(), sent.len());
        assert!(sent.iter().all(|s| !s), "nothing marked on the call");
        self.calls
            .lock()
            .unwrap()
            .push(frames.iter().map(|f| f.to_vec()).collect());
        match self.take {
            Take::All => sent.fill(true),
            Take::Even => {
                for (i, s) in sent.iter_mut().enumerate() {
                    *s = i % 2 == 0;
                }
            }
            Take::Refuse(code) => {
                sent.fill(true);
                return Err(io::Error::from_raw_os_error(code));
            }
        }
        Ok(())
    }
}

impl Recorder {
    fn calls(&self) -> Vec<Vec<Vec<u8>>> {
        self.calls.lock().unwrap().clone()
    }

    fn frames(&self) -> Vec<Vec<u8>> {
        self.calls().into_iter().flatten().collect()
    }
}

fn shared() -> Arc<TxShared> {
    let cfg = DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-device.toml");
    let shared = TxShared::new(cfg, Ipv4Addr::LOCALHOST, Vec::new().into(), 1, &[]).unwrap();
    assert!(shared.set_source(0, Source::Counter));
    assert!(shared.set_source(1, Source::Counter));
    Arc::new(shared)
}

fn nic() -> HostNic {
    HostNic {
        ifindex: IFINDEX,
        mac: OUR_MAC,
        name: "test0".to_string(),
    }
}

/// `shared` sending through a new [`Recorder`] that takes `take`, with
/// [`RESOLVED`] in the neighbour table on our NIC.
fn host(shared: &TxShared, take: Take) -> Arc<Recorder> {
    let rec = Arc::new(Recorder {
        take,
        calls: Mutex::new(Vec::new()),
    });
    assert!(shared.use_frame_sender(rec.clone(), nic()));
    shared.set_neighbours(&[Neighbour {
        ip: RESOLVED,
        mac: PEER_MAC,
        ifindex: IFINDEX,
    }]);
    rec
}

/// A receiver's socket at `ip`.
fn receiver(ip: Ipv4Addr) -> (UdpSocket, SocketAddrV4) {
    let s = UdpSocket::bind((ip, 0)).unwrap();
    s.set_nonblocking(true).unwrap();
    let SocketAddr::V4(at) = s.local_addr().unwrap() else {
        panic!("an IPv4 socket");
    };
    (s, at)
}

/// A flow of TX 1 and 2 to `rx` in `fpp`-frame packets; its slot.
fn flow(shared: &TxShared, rx: SocketAddrV4, fpp: u16) -> usize {
    let mut c = shared.lock();
    let req = FlowRequest {
        rx_host: "peer-device".to_string(),
        rx_flow_name: "flow-1".to_string(),
        sample_rate: RATE,
        bits: 24,
        channels: vec![Some(1), Some(2)],
        fpp,
        rx,
    };
    let (slot, _) = c.table.add(&req, shared.now_ns()).unwrap();
    shared.publish(&c);
    slot
}

/// Every datagram waiting on `s`.
fn drain(s: &UdpSocket) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0u8; 2048];
    while let Ok(n) = s.recv(&mut buf) {
        out.push(buf[..n].to_vec());
    }
    out
}

/// Steps `tx` every 500 µs from `T` for `ms` ms; checks that each pass made
/// at most one call to `rec`, and one when slot `slot` sent a packet.
fn run(tx: &mut Transmitter, shared: &TxShared, rec: &Recorder, slot: usize, ms: u64) {
    for k in 0..ms * 2 {
        let (calls, packets) = (rec.calls().len(), shared.lane(slot).packets);
        tx.step(Some(T + k * 500_000), 0);
        let made = rec.calls().len() - calls;
        let sent = shared.lane(slot).packets - packets;
        assert_eq!(made, usize::from(sent > 0), "pass {k}: one call a pass");
    }
}

/// The 24-bit, 2-channel, 32-frame media packet's bytes.
const PACKET: usize = MEDIA_HEADER + 2 * 3 * 32;

#[test]
fn a_pass_sends_its_frames_to_the_host_sender_in_one_call_byte_exact() {
    let shared = shared();
    let (a, at_a) = receiver(RESOLVED);
    let (b, at_b) = receiver(UNRESOLVED);
    let slot_a = flow(&shared, at_a, 32);
    let slot_b = flow(&shared, at_b, 32);
    let rec = host(&shared, Take::All);
    assert_eq!(shared.tx_path(), TxPath::HostSender);
    let mut tx = Transmitter::new(Arc::clone(&shared));
    run(&mut tx, &shared, &rec, slot_a, 20);
    let frames = rec.frames();
    let lane = shared.lane(slot_a);
    assert!(frames.len() > 50, "{}", frames.len());
    assert_eq!(lane.packets, frames.len() as u64, "{lane:?}");
    assert_eq!(lane.direct, lane.packets, "every frame taken counts direct");
    assert_eq!(lane.bytes, lane.packets * (PACKET as u64 + 42));
    assert_eq!(lane.late_bins.iter().sum::<u64>(), lane.packets);
    assert_eq!((lane.send_errors, lane.send_retries), (0, 0));
    assert!(
        drain(&a).is_empty(),
        "nothing on the resolved flow's socket"
    );
    // Byte-exact: Ethernet II from our MAC to the neighbour's, IPv4 with
    // DSCP EF and consecutive IDs, UDP from the slot's port, the packet.
    let src = SocketAddrV4::new(
        Ipv4Addr::LOCALHOST,
        shared.local_addr(slot_a).unwrap().port(),
    );
    let first_id = u16::from_be_bytes([frames[0][18], frames[0][19]]);
    let mut stamp = None;
    for (i, frame) in frames.iter().enumerate() {
        assert_eq!(frame.len(), FRAME_HEADROOM + PACKET, "frame {i}");
        let mut want = frame.clone();
        want[..FRAME_HEADROOM].fill(0);
        write_ethernet(&mut want, PEER_MAC, OUR_MAC);
        let id = first_id.wrapping_add(i as u16);
        write_headers(&mut want[ETHERNET_HEADER..], src, at_a, 0xb8, id, PACKET);
        assert_eq!(*frame, want, "frame {i}");
        let (secs, sub) = read_header(&frame[FRAME_HEADROOM..]).unwrap();
        let t = u64::from(secs) * u64::from(RATE) + u64::from(sub);
        if let Some(before) = stamp {
            assert_eq!(t, before + 32, "frame {i}: the next packet");
        }
        stamp = Some(t);
    }
    // The unresolved receiver: through its UDP socket, never the sender.
    let lb = shared.lane(slot_b);
    let got = drain(&b);
    assert!(lb.packets > 50, "{lb:?}");
    assert_eq!(got.len() as u64, lb.packets);
    assert_eq!(lb.direct, 0);
    assert!(got.iter().all(|p| p.len() == PACKET));
    let to_b = (HEADROOM + 2..HEADROOM + 4, at_b.port().to_be_bytes());
    assert!(
        frames
            .iter()
            .all(|f| f[ETHERNET_HEADER + to_b.0.start..ETHERNET_HEADER + to_b.0.end] != to_b.1),
        "no frame to the unresolved receiver"
    );
    assert_eq!(shared.host_refusals(), (0, 0));
}

#[test]
fn a_frame_the_sender_did_not_take_goes_through_the_udp_socket() {
    let shared = shared();
    let (a, at_a) = receiver(RESOLVED);
    let slot = flow(&shared, at_a, 32);
    let rec = host(&shared, Take::Even);
    let mut tx = Transmitter::new(Arc::clone(&shared));
    run(&mut tx, &shared, &rec, slot, 20);
    let calls = rec.calls();
    let left: Vec<Vec<u8>> = calls
        .iter()
        .flat_map(|c| c.iter().skip(1).step_by(2))
        .map(|f| f[FRAME_HEADROOM..].to_vec())
        .collect();
    let taken: usize = calls.iter().map(|c| c.len().div_ceil(2)).sum();
    let lane = shared.lane(slot);
    let total: usize = calls.iter().map(Vec::len).sum();
    assert_eq!(lane.packets, total as u64, "{lane:?}");
    assert_eq!(lane.direct, taken as u64);
    assert_eq!(lane.send_errors, 0);
    assert_eq!(
        drain(&a),
        left,
        "the frames not taken, as datagrams, in order"
    );
    assert_eq!(
        shared.host_refusals(),
        (0, 0),
        "a partial take is no refusal"
    );
}

#[test]
fn a_batch_refused_whole_goes_through_the_udp_socket_and_is_counted() {
    let shared = shared();
    let (a, at_a) = receiver(RESOLVED);
    let slot = flow(&shared, at_a, 32);
    let rec = host(&shared, Take::Refuse(10055));
    let mut tx = Transmitter::new(Arc::clone(&shared));
    run(&mut tx, &shared, &rec, slot, 20);
    let calls = rec.calls().len() as u64;
    let lane = shared.lane(slot);
    // A packet every 333 µs, a pass every 500 µs: most passes send one.
    assert!(calls > 30, "{calls}");
    assert_eq!(lane.direct, 0, "the sender's marks do not count: {lane:?}");
    assert_eq!(drain(&a).len() as u64, lane.packets);
    assert_eq!(lane.packets, rec.frames().len() as u64);
    assert_eq!(shared.host_refusals(), (calls, 10055));
}

#[test]
fn a_pass_with_nothing_due_calls_no_sender() {
    let shared = shared();
    let (_a, at_a) = receiver(RESOLVED);
    flow(&shared, at_a, 32);
    let rec = host(&shared, Take::All);
    let mut tx = Transmitter::new(Arc::clone(&shared));
    tx.step(None, 0);
    // The first pass starts the schedule: nothing is due yet.
    tx.step(Some(T), 0);
    assert!(rec.calls().is_empty());
}

#[test]
fn more_frames_than_a_batch_go_in_full_batches() {
    let shared = shared();
    let slots: Vec<usize> = (0..MAX_FLOWS as u16)
        .map(|i| flow(&shared, SocketAddrV4::new(RESOLVED, 40_000 + i), 16))
        .collect();
    let rec = host(&shared, Take::All);
    let mut tx = Transmitter::new(Arc::clone(&shared));
    tx.step(Some(T), 0);
    assert!(rec.calls().is_empty());
    // 1 ms later: five or six 16-frame packets due on each of the 32 flows.
    tx.step(Some(T + 1_000_000), 0);
    let calls = rec.calls();
    let sizes: Vec<usize> = calls.iter().map(Vec::len).collect();
    let total: usize = sizes.iter().sum();
    assert!(total > 2 * BATCH_FRAMES, "{sizes:?}");
    assert_eq!(sizes.len(), total.div_ceil(BATCH_FRAMES), "{sizes:?}");
    assert!(sizes[..sizes.len() - 1].iter().all(|&n| n == BATCH_FRAMES));
    let packets: u64 = slots.iter().map(|&s| shared.lane(s).packets).sum();
    let direct: u64 = slots.iter().map(|&s| shared.lane(s).direct).sum();
    assert_eq!((packets, direct), (total as u64, total as u64));
}

#[test]
fn the_host_path_is_set_once_and_follows_the_neighbour_table() {
    let shared = shared();
    let slot = flow(&shared, SocketAddrV4::new(RESOLVED, 4321), 32);
    let other = flow(&shared, SocketAddrV4::new(UNRESOLVED, 4321), 32);
    let rows = [Neighbour {
        ip: RESOLVED,
        mac: PEER_MAC,
        ifindex: IFINDEX,
    }];
    // Without a sender: the table moves nothing, no slot goes to a host.
    shared.set_neighbours(&rows);
    assert_eq!(shared.slot_mac(slot), None);
    assert_eq!(shared.host_mac(slot), None);
    assert_ne!(shared.tx_path(), TxPath::HostSender);
    let rec = host(&shared, Take::All);
    assert_eq!(shared.tx_path(), TxPath::HostSender);
    assert_eq!(shared.tx_nic(), "test0");
    assert_eq!(shared.host_mac(slot), Some(PEER_MAC));
    assert_eq!(shared.host_mac(other), None, "not in the table");
    // A second sender is refused; the first stays.
    assert!(!shared.use_frame_sender(rec.clone(), nic()));
    // Another interface's entry, or none: back to the UDP socket.
    let version = shared.macs[slot].version();
    shared.set_neighbours(&[Neighbour {
        ifindex: IFINDEX + 1,
        ..rows[0]
    }]);
    assert_eq!(shared.host_mac(slot), None);
    shared.set_neighbours(&[]);
    assert_eq!(shared.host_mac(slot), None);
    assert_eq!(
        shared.macs[slot].version(),
        version.wrapping_add(2),
        "an unchanged MAC is not written again"
    );
}

#[test]
fn the_start_line_s_nic_is_the_address_without_a_sender() {
    let shared = shared();
    if shared.tx_path() == TxPath::UdpSocket {
        assert_eq!(shared.tx_nic(), "127.0.0.1");
    }
}

#[test]
fn the_host_watch_warns_of_refused_batches_with_the_os_error() {
    let shared = shared();
    let mut watch = HostWatch::new();
    watch.watch(0, &shared);
    shared.host_refused.store(3, Ordering::Relaxed);
    shared.host_error.store(10055, Ordering::Relaxed);
    let text = warned(|| watch.watch(crate::media::late::EVERY_NS, &shared));
    assert!(text.contains("refused whole batches"), "{text}");
    assert!(text.contains("batches=3"), "{text}");
    assert!(text.contains("os_error=10055"), "{text}");
    let text = warned(|| watch.watch(2 * crate::media::late::EVERY_NS, &shared));
    assert!(text.is_empty(), "nothing new: {text}");
}

/// What `f` logs at WARN or above on this thread, as text.
fn warned(f: impl FnOnce()) -> String {
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = Arc::clone(&buf);
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || Writer(Arc::clone(&sink)))
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .finish();
    crate::test_log::undecided();
    tracing::subscriber::with_default(subscriber, f);
    String::from_utf8(buf.lock().unwrap().clone()).unwrap()
}

struct Writer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Writer {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
