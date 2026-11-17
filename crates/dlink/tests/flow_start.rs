// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #371: a receiver's flow, granted on loopback, carries the app's
//! audio within a bound, start after start. SongPlayer's whole-device test
//! (songplayer CI run 44030478973, windows-2025) once got a granted flow and
//! 6 s of no tone: SongPlayer's shape here (a 48 kHz app on a 96 kHz
//! device, two TX channels, a receiver of ours asking for 32-frame packets
//! of 24 bits), started again and again. A run that misses its bound names
//! what the receiver saw (packets, the first one's time, the levels) and the
//! device's whole status line.
//!
//! That failure was SongPlayer's own test: its processor pops single samples
//! from an interleaved stereo ring, its test pushes one sample at a time, and
//! a pop between a pair's two pushes swapped the channels for the rest of the
//! run (the stack sent every packet, 0 holes). The processor here writes both
//! levels in each block, so only a stack fault fails this test.

use std::net::{SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dlink::{Block, Clock, Config, Link, Map, Place, Process};
use dstack::flows::client::Client;
use dstack::flows::control::FlowRequest;
use dstack::media::packet;

/// Loopback (dstack #366): nothing this test sends leaves the box.
fn test_ipv4() -> std::net::Ipv4Addr {
    std::net::Ipv4Addr::LOCALHOST
}

/// Each test here runs a device on the same ports.
static ONE_DEVICE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The receiver must hear both levels within this of the grant: well under
/// the 4 s a flow lives without the receiver's keepalives (this receiver
/// sends none, as SongPlayer's).
const BOUND: Duration = Duration::from_secs(3);

/// Left 0.5, right -0.25 once `on` (silence before: SongPlayer's ring is
/// empty until its test writes).
struct Levels {
    on: Arc<AtomicBool>,
}

impl Process for Levels {
    fn process(&mut self, block: &mut Block<'_>) {
        let on = self.on.load(Ordering::Relaxed);
        block.output(0).fill(if on { 0.5 } else { 0.0 });
        block.output(1).fill(if on { -0.25 } else { 0.0 });
    }
}

/// What the receiver saw of one flow.
#[derive(Debug, Default)]
struct Heard {
    packets: u64,
    first_packet: Option<Duration>,
    left_at: Option<Duration>,
    right_at: Option<Duration>,
    /// The last packet's first sample of each slot (24-bit).
    last: [i32; 2],
}

/// One start: the device, the flow asked, the levels until both are heard
/// or [`BOUND`] passes. `Err` names what was heard and the status.
fn one_start(run: usize) -> Result<Duration, String> {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::new("dlink-flow-start", dir.path().join("dlink.toml"))
        .tx(["01", "02"])
        .sample_rate(48_000)
        .network_rate(96_000);
    let on = Arc::new(AtomicBool::new(false));
    let link = Link::start_on(
        &config,
        &Place::Ip(test_ipv4()),
        Map {
            rx: Vec::new(),
            tx: vec![0, 1],
        },
        Levels {
            on: Arc::clone(&on),
        },
        Clock::System,
    )
    .map_err(|e| format!("run {run}: the start: {e:?}"))?;
    let ip = test_ipv4();
    let audio = UdpSocket::bind((ip, 0)).unwrap();
    audio
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let SocketAddr::V4(at) = audio.local_addr().unwrap() else {
        panic!("IPv4")
    };
    let req = FlowRequest {
        rx_host: "test-receiver".to_string(),
        rx_flow_name: "program".to_string(),
        sample_rate: 96_000,
        bits: 24,
        channels: vec![Some(1), Some(2)],
        fpp: 32,
        rx: at,
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let flows = SocketAddrV4::new(ip, dstack::flows::control::PORT);
    let granted = rt.block_on(Client::new(ip).request_flow(flows, &req));
    if let Err(e) = granted {
        let status = link.status();
        link.stop();
        return Err(format!("run {run}: the flow: {e:?}; status {status}"));
    }
    on.store(true, Ordering::Relaxed);
    let start = Instant::now();
    let mut heard = Heard::default();
    let mut buf = [0u8; 2048];
    let mut errors = Vec::new();
    while (heard.left_at.is_none() || heard.right_at.is_none())
        && !BOUND.saturating_sub(start.elapsed()).is_zero()
    {
        let n = match audio.recv(&mut buf) {
            Ok(n) => n,
            Err(e) => {
                if errors.len() < 8 {
                    errors.push(format!("{:?}: {e}", start.elapsed()));
                }
                continue;
            }
        };
        heard.packets += 1;
        heard.first_packet.get_or_insert(start.elapsed());
        let mut first = [None::<i32>; 2];
        packet::read_frames(&buf[..n], 2, 3, |_, slot, x| {
            let x = x >> 8;
            if first[slot].is_none() {
                first[slot] = Some(x);
            }
            if slot == 0 && (x - (1 << 22)).abs() < 8 {
                heard.left_at.get_or_insert(start.elapsed());
            }
            if slot == 1 && (x + (1 << 21)).abs() < 8 {
                heard.right_at.get_or_insert(start.elapsed());
            }
        })
        .expect("two 24-bit slots");
        heard.last = [first[0].unwrap_or(0), first[1].unwrap_or(0)];
    }
    let took = start.elapsed();
    let status = link.status();
    link.stop();
    if heard.left_at.is_some() && heard.right_at.is_some() {
        Ok(took)
    } else {
        Err(format!(
            "run {run}: no tone within {BOUND:?} of the grant: {heard:?}; receive errors {errors:?}; status {status}"
        ))
    }
}

/// `runs` starts in a row; every one must carry the tone within [`BOUND`].
fn starts(runs: usize) {
    let _one = ONE_DEVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut slowest = Duration::ZERO;
    for run in 0..runs {
        match one_start(run) {
            Ok(took) => slowest = slowest.max(took),
            Err(why) => panic!("{why}"),
        }
    }
    eprintln!("{runs} flow starts, the slowest tone {slowest:?} after the grant");
}

#[test]
fn a_granted_flow_carries_the_app_s_audio_within_its_bound() {
    starts(3);
}

/// dstack #371's loop on the platform it failed on: Windows CI only.
#[cfg(windows)]
#[test]
fn a_granted_flow_carries_the_app_s_audio_start_after_start_on_windows() {
    starts(40);
}
