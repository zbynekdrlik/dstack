//! dstack #434 (PR #432's windows-2025 run, dlink `flow_start`): a flow
//! request that came while our TX clock turned ready was never answered.
//! The server read the readiness once to answer (not ready: left
//! unanswered) and again to decide the hold (ready by then: not held), so
//! the request was neither granted nor held, and a receiver that asks once
//! timed out. Here the TX clock comes and goes while requests arrive: each
//! must be answered, at once or from the hold at a later expiry tick.

use super::*;

use std::sync::atomic::{AtomicBool, Ordering};

/// Requesters, each on its own loopback address (the peer bound counts per
/// address) and each with its own flow (a slot each).
const PEERS: u8 = 4;
/// Requests a requester sends at once: within its bound ([`PER_SPAN`]).
const BURST: u16 = 10;
/// Bursts per requester, a [`SPAN`] apart.
const BURSTS: u16 = 2;
/// A request is answered within this: a held one at the first expiry tick
/// ([`EXPIRE_CHECK`]) that reads the clock ready, well within [`HOLD`].
const ANSWERED_WITHIN: Duration = HOLD;

/// One requester: its socket and where it wants the audio.
struct Peer {
    sock: tokio::net::UdpSocket,
    rx: SocketAddrV4,
}

impl Peer {
    async fn bind(k: u8) -> Peer {
        let sock = tokio::net::UdpSocket::bind((Ipv4Addr::new(127, 0, 43, k), 0))
            .await
            .unwrap();
        let SocketAddr::V4(rx) = sock.local_addr().unwrap() else {
            panic!("an IPv4 socket");
        };
        Peer { sock, rx }
    }

    /// Sends requests `seqs` and waits for an OK to each; `Err` names the
    /// ones left unanswered.
    async fn ask(&self, server: SocketAddr, seqs: std::ops::Range<u16>) -> Result<(), String> {
        for seq in seqs.clone() {
            let r = FlowRequest {
                rx: self.rx,
                ..req()
            };
            self.sock
                .send_to(&build_request(seq, &r), server)
                .await
                .unwrap();
        }
        let mut open: Vec<u16> = seqs.collect();
        let start = Instant::now();
        let mut buf = [0u8; 2048];
        while !open.is_empty() {
            let left = ANSWERED_WITHIN.saturating_sub(start.elapsed());
            let Ok(got) = tokio::time::timeout(left, self.sock.recv_from(&mut buf)).await else {
                return Err(format!(
                    "requester {}: requests {open:?} neither granted nor held (no answer within {ANSWERED_WITHIN:?})",
                    self.rx.ip()
                ));
            };
            let (n, _) = got.unwrap();
            let (h, _) = Header::parse(&buf[..n]).unwrap();
            assert_eq!((h.op1, h.op2), (op::REQUEST, status::OK), "seq {}", h.seq);
            open.retain(|&s| s != h.seq);
        }
        Ok(())
    }
}

#[tokio::test]
async fn a_request_while_the_clock_turns_ready_is_answered() {
    let rings: Arc<[Ring]> = Arc::from(Vec::new());
    let shared = Arc::new(TxShared::new(cfg(), Ipv4Addr::LOCALHOST, rings, 7, &[]).unwrap());
    let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server = sock.local_addr().unwrap();
    let task = tokio::spawn(serve(
        sock,
        Arc::clone(&shared),
        Arc::new(Requests::default()),
    ));
    let mut peers = Vec::new();
    for k in 1..=PEERS {
        peers.push(Peer::bind(k).await);
    }
    let passes = Passes::start(&shared);
    for burst in 0..BURSTS {
        if burst > 0 {
            tokio::time::sleep(SPAN).await;
        }
        let seqs = burst * BURST..(burst + 1) * BURST;
        for peer in &peers {
            if let Err(why) = peer.ask(server, seqs.clone()).await {
                panic!("{why}");
            }
        }
    }
    drop(passes);
    task.abort();
}

/// The TX thread's passes, as fast as they can come: the clock ready, then
/// not (a clock that comes and goes), so a request's handling meets a
/// change of it at any point. Stopped and joined when dropped, by a failing
/// test's panic too: a thread left spinning would load every test after it
/// in the binary.
struct Passes {
    done: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Passes {
    fn start(shared: &Arc<TxShared>) -> Passes {
        let done = Arc::new(AtomicBool::new(false));
        let thread = std::thread::spawn({
            let (shared, done) = (Arc::clone(shared), Arc::clone(&done));
            move || {
                while !done.load(Ordering::Relaxed) {
                    shared.set_clock_ready(true);
                    shared.set_clock_ready(false);
                }
            }
        });
        Passes {
            done,
            thread: Some(thread),
        }
    }
}

impl Drop for Passes {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            // Never a panic here: a drop during a failing test's unwind
            // would abort the binary (the loop itself cannot panic).
            let _joined = thread.join();
        }
    }
}
