// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted from inferno (https://github.com/teodly/inferno at 9767558):
// inferno_aoip/src/protocol/proto_cmc.rs (the advertisement's layout),
// inferno_aoip/src/device_server/cmc_server.rs (the server) and
// inferno_aoip/src/device_server/settings.rs (the device id from the
// address), Copyright (C) Teo and the inferno contributors,
// GPL-3.0-or-later OR AGPL-3.0-or-later, taken under GPL-3.0-or-later.
// Changes Copyright (C) 2026 iemmixer contributors.

//! CMC on UDP 8800: the device's advertisement (0x1001), which names it by
//! its device id and address, and the meter subscription (0x3010, Dante
//! Controller's and [`crate::meter`]'s): the request is answered with its
//! own bytes and result OK, and while it is renewed (within
//! [`METER_LEASE`]) the destination gets a meter frame every
//! [`METER_EVERY`] with the levels the heartbeat left ([`Levels`]). A zero
//! destination stops it. Any other datagram gets no reply, as inferno.

use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::{Instant, MissedTickBehavior};
use tracing::{debug, info, warn};

use crate::ctl::{Requests, header_op};
use crate::info::Levels;
use crate::wire::{Header, STATUS_OK, reply};

/// CMC's UDP port.
pub const PORT: u16 = 8800;
/// The advertisement request.
pub const ADVERTISE: u16 = 0x1001;
/// The info request port the advertisement names (inferno's
/// `INFO_REQUEST_PORT`), served by [`crate::info`].
pub const INFO_REQUEST_PORT: u16 = crate::info::REQUEST_PORT;
/// The device's process id (inferno's default): one device on the box.
/// The process id when the config names none (inferno's default).
pub const PROCESS_ID: u16 = 0;
const RECV_BUFFER: usize = 2048;
/// How long a meter subscription lives after its last renewal (netaudio
/// renews every 3 s).
pub const METER_LEASE: Duration = Duration::from_secs(10);
/// How often a live subscription gets a frame: the heartbeat's period,
/// whose peaks the frames carry.
pub const METER_EVERY: Duration = crate::info::HEARTBEAT_EVERY;
/// The most destinations kept at once.
pub const METER_MAX: usize = 16;

/// The device's 8-byte id: two zero bytes, the IPv4 address, the process
/// id (inferno's default when none is configured). The mDNS `id` entry is
/// its hex.
pub fn device_id(ip: Ipv4Addr, process: u16) -> [u8; 8] {
    let [a, b, c, d] = ip.octets();
    let [p, q] = process.to_be_bytes();
    [0, 0, a, b, c, d, p, q]
}

/// The reply to a CMC datagram: the advertisement for 0x1001 with op2 0,
/// nothing for anything else.
pub fn handle_cmc(req: &[u8], ip: Ipv4Addr, process: u16) -> Option<Vec<u8>> {
    let (h, _) = Header::parse(req).ok()?;
    if h.op1 != ADVERTISE || h.op2 != 0 {
        debug!(
            op = format_args!("{:#06x}", h.op1),
            seq = h.seq,
            "CMC request not answered"
        );
        return None;
    }
    Some(reply(&h, STATUS_OK, &advertisement(ip, process)))
}

/// inferno's `DeviceAdvertisement`: process id, device id, 1, 0, the
/// address, the info request port, 0.
fn advertisement(ip: Ipv4Addr, process: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(22);
    out.extend_from_slice(&process.to_be_bytes());
    out.extend_from_slice(&device_id(ip, process));
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&ip.octets());
    out.extend_from_slice(&INFO_REQUEST_PORT.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out
}

/// The meter subscriptions: each destination and when its lease ends.
#[derive(Debug, Default)]
pub struct Meters {
    live: Vec<(SocketAddrV4, Instant)>,
}

impl Meters {
    /// A subscription request: a start renews (or adds, up to
    /// [`METER_MAX`]) `to`'s lease, a stop (`None`) removes the asker's
    /// destination, `from`.
    pub fn take(&mut self, to: Option<SocketAddrV4>, from: SocketAddrV4, now: Instant) {
        match to {
            Some(to) => {
                let until = now + METER_LEASE;
                if let Some(e) = self.live.iter_mut().find(|(d, _)| *d == to) {
                    e.1 = until;
                } else if self.live.len() < METER_MAX {
                    info!(port = to.port(), "meter subscription taken");
                    self.live.push((to, until));
                }
            }
            None => self.live.retain(|(d, _)| *d != from),
        }
    }

    /// The live destinations at `now`; expired ones go.
    pub fn live(&mut self, now: Instant) -> Vec<SocketAddrV4> {
        self.live.retain(|&(_, until)| until > now);
        self.live.iter().map(|&(d, _)| d).collect()
    }
}

/// The CMC server for the device at `ip` (the address it is reached at,
/// which the advertisement names); every datagram counted in `requests`;
/// meter frames from `levels`. Ends only with an error receiving.
pub async fn serve_cmc(
    sock: UdpSocket,
    ip: Ipv4Addr,
    process: u16,
    requests: Arc<Requests>,
    levels: Arc<Levels>,
) -> io::Result<()> {
    let mut buf = vec![0u8; RECV_BUFFER];
    let mut meters = Meters::default();
    let mut every = tokio::time::interval(METER_EVERY);
    every.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut seq: u16 = 0;
    let id = device_id(ip, process);
    loop {
        tokio::select! {
            got = sock.recv_from(&mut buf) => {
                let (n, peer) = got?;
                let req = &buf[..n];
                requests.received(PORT, peer.ip(), header_op(req));
                if let (Some(to), SocketAddr::V4(from)) = (crate::meter::destination(req), peer) {
                    meters.take(to, from, Instant::now());
                    // Answered with its own bytes, result OK (netaudio's
                    // reply keeps the request's layout).
                    let mut answer = req.to_vec();
                    answer[8..10].copy_from_slice(&STATUS_OK.to_be_bytes());
                    if let Err(e) = sock.send_to(&answer, peer).await {
                        warn!(error = %e, "a meter subscription reply could not be sent");
                    }
                } else if let Some(bytes) = handle_cmc(req, ip, process)
                    && let Err(e) = sock.send_to(&bytes, peer).await
                {
                    warn!(error = %e, "a CMC reply could not be sent");
                }
            }
            _ = every.tick() => {
                let live = meters.live(Instant::now());
                if live.is_empty() {
                    continue;
                }
                let (tx, rx) = levels.get();
                seq = seq.wrapping_add(1);
                if let Some(frame) = crate::meter::frame(seq, id, &tx, &rx) {
                    for to in live {
                        if let Err(e) = sock.send_to(&frame, to).await {
                            debug!(error = %e, port = to.port(), "a meter frame not sent");
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use crate::wire::{Header, STATUS_OK, request};

    /// A synthetic address (public repository, P6).
    const IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 10);

    #[test]
    fn the_device_id_is_the_address_between_zeros() {
        assert_eq!(device_id(IP, 0), [0, 0, 10, 0, 0, 10, 0, 0]);
        assert_eq!(
            device_id(Ipv4Addr::new(192, 0, 2, 254), 0),
            [0, 0, 192, 0, 2, 254, 0, 0]
        );
    }

    #[test]
    fn the_advertisement_names_the_device_by_its_address() {
        let reply = handle_cmc(&request(0x27ff, 0x4321, ADVERTISE, 0, &[]), IP, 0).unwrap();
        let (h, body) = Header::parse(&reply).unwrap();
        assert_eq!(
            (h.start, h.seq, h.op1, h.op2),
            (0x27ff, 0x4321, ADVERTISE, STATUS_OK)
        );
        #[rustfmt::skip]
        let want = [
            0x00, 0x00,                                     // process id 0
            0x00, 0x00, 10, 0, 0, 10, 0x00, 0x00,           // the device id
            0x00, 0x01, 0x00, 0x00,                         // 1, 0 (inferno)
            10, 0, 0, 10,                                   // the address
            0x21, 0xfc,                                     // the info request port, 8700
            0x00, 0x00,
        ];
        assert_eq!(body, want);
    }

    #[test]
    fn anything_but_the_advertisement_request_gets_no_answer() {
        // inferno answers only 0x1001 with op2 0; Dante Controller asks again.
        for op1 in [0x1000, 0x1002, 0x0000, 0xffff] {
            assert_eq!(
                handle_cmc(&request(0x27ff, 1, op1, 0, &[]), IP, 0),
                None,
                "{op1:#06x}"
            );
        }
        assert_eq!(
            handle_cmc(&request(0x27ff, 1, ADVERTISE, 1, &[]), IP, 0),
            None
        );
        let mut cut = request(0x27ff, 1, ADVERTISE, 0, &[]);
        cut.pop();
        assert_eq!(handle_cmc(&cut, IP, 0), None);
        assert_eq!(handle_cmc(&[], IP, 0), None);
        // A body after the request is no reason to refuse it.
        assert!(handle_cmc(&request(0x27ff, 1, ADVERTISE, 0, &[1, 2, 3]), IP, 0).is_some());
    }

    #[tokio::test]
    async fn the_cmc_server_answers_the_advertisement_request() {
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = sock.local_addr().unwrap();
        let requests = Arc::new(Requests::default());
        let task = tokio::spawn(serve_cmc(
            sock,
            IP,
            0,
            Arc::clone(&requests),
            Arc::default(),
        ));
        let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        // An unknown request gets no answer; the next one does.
        peer.send_to(&request(0x27ff, 1, 0x1000, 0, &[]), addr)
            .await
            .unwrap();
        peer.send_to(&request(0x27ff, 2, ADVERTISE, 0, &[]), addr)
            .await
            .unwrap();
        let mut buf = [0u8; 256];
        let (n, _) = tokio::time::timeout(Duration::from_secs(5), peer.recv_from(&mut buf))
            .await
            .expect("an answer within 5 s")
            .unwrap();
        let (h, body) = Header::parse(&buf[..n]).unwrap();
        assert_eq!(
            (h.seq, h.op1, h.op2, body.len()),
            (2, ADVERTISE, STATUS_OK, 22)
        );
        assert_eq!(body[14..18], [10, 0, 0, 10]);
        // Both datagrams counted for the status, by port, peer and opcode.
        let counts = requests.counts();
        let seen: Vec<(u16, usize, u16, u64)> = counts
            .received
            .iter()
            .map(|r| (r.port, r.peer, r.op, r.n))
            .collect();
        assert_eq!(seen, [(PORT, 1, 0x1000, 1), (PORT, 1, ADVERTISE, 1)]);
        task.abort();
    }

    #[test]
    fn the_advertisement_and_the_id_carry_the_process_id() {
        assert_eq!(device_id(IP, 0x0102), [0, 0, 10, 0, 0, 10, 1, 2]);
        let reply = handle_cmc(&request(0x27ff, 1, ADVERTISE, 0, &[]), IP, 0x0102).unwrap();
        let (_, body) = Header::parse(&reply).unwrap();
        assert_eq!(&body[..2], &[1, 2], "the process id first");
        assert_eq!(&body[2..10], &device_id(IP, 0x0102));
    }

    #[test]
    fn a_meter_lease_is_renewed_kept_to_its_end_and_stopped() {
        let now = Instant::now();
        let a = SocketAddrV4::new(IP, 8751);
        let b = SocketAddrV4::new(IP, 8752);
        let mut m = Meters::default();
        m.take(Some(a), a, now);
        m.take(Some(b), b, now + Duration::from_secs(5));
        assert_eq!(m.live(now + Duration::from_secs(9)), [a, b]);
        // a's lease ends at 10 s; b's renewal at 5 s holds it to 15 s.
        assert_eq!(m.live(now + Duration::from_secs(10)), [b]);
        m.take(Some(b), b, now + Duration::from_secs(14));
        assert_eq!(m.live(now + Duration::from_secs(20)), [b]);
        // A stop names no destination: the asker's goes.
        m.take(None, b, now + Duration::from_secs(20));
        assert!(m.live(now + Duration::from_secs(20)).is_empty());
        // At most METER_MAX at once.
        for p in 0..(METER_MAX as u16 + 3) {
            let d = SocketAddrV4::new(IP, 9000 + p);
            m.take(Some(d), d, now);
        }
        assert_eq!(m.live(now).len(), METER_MAX);
    }

    #[test]
    fn our_server_sends_meter_frames_with_the_beat_s_levels_until_the_stop() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let sock = rt
            .block_on(UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)))
            .unwrap();
        let SocketAddr::V4(at) = sock.local_addr().unwrap() else {
            unreachable!()
        };
        let levels = Arc::new(Levels::default());
        // The heartbeat's peak bytes: full scale (0), -20 dBFS (40), and a
        // silent channel (255, the floor).
        levels.set(&[0, 40], &[255]);
        rt.spawn(serve_cmc(
            sock,
            Ipv4Addr::LOCALHOST,
            0,
            Arc::new(Requests::default()),
            Arc::clone(&levels),
        ));
        let _keep = std::thread::spawn(move || rt.block_on(std::future::pending::<()>()));
        let (meter, frames) =
            crate::meter::Meter::start(Ipv4Addr::LOCALHOST, at, "ctl", [0, 0, 0, 0, 0, 1]).unwrap();
        let f = frames
            .recv_timeout(Duration::from_secs(3))
            .expect("a frame");
        assert_eq!(f.device_id, device_id(Ipv4Addr::LOCALHOST, 0));
        assert_eq!((f.tx, f.rx), (vec![1, 41], vec![253]));
        meter.stop().unwrap();
        // A plain socket as the destination: frames come until its stop,
        // then none within two periods.
        let raw = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let std::net::SocketAddr::V4(me) = raw.local_addr().unwrap() else {
            unreachable!()
        };
        let mac = [0, 0, 0, 0, 0, 2];
        let start = crate::meter::start_request(9, "raw", mac, me).unwrap();
        raw.send_to(&start, at).unwrap();
        raw.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut buf = [0u8; 256];
        // The reply (its own bytes, result OK), then a frame.
        let (n, _) = raw.recv_from(&mut buf).unwrap();
        assert_eq!(
            (n, &buf[8..10]),
            (start.len(), &STATUS_OK.to_be_bytes()[..])
        );
        let (n, _) = raw.recv_from(&mut buf).unwrap();
        assert!(crate::meter::parse_frame(&buf[..n]).is_some());
        let stop = crate::meter::stop_request(10, "raw", mac, me).unwrap();
        raw.send_to(&stop, at).unwrap();
        // The stop's reply, maybe a frame already on its way, then quiet.
        raw.set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        while raw.recv_from(&mut buf).is_ok() {}
        raw.set_read_timeout(Some(Duration::from_millis(2500)))
            .unwrap();
        assert!(raw.recv_from(&mut buf).is_err(), "a frame after the stop");
    }
}
