// SPDX-License-Identifier: MIT OR Apache-2.0
//! dstack #352: a device latency write (Dante Controller's, ARC 0x1101)
//! reaches the RX flows already running. A flow's latency is the larger of
//! its transmitter's advertised latency and the device's, at the write as
//! at the request; its playout follows, and nothing is asked of or sent to
//! any transmitter (a flow request carries no latency). Seen on a dasio
//! device, 16.11.2026: 40 to 5 ms, six flows kept 40000 us for 200 s.

use super::*;

const SLOW_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 13);

/// The peer's multicast flow of TX 1 and 2 ([`mics`]), another
/// transmitter's channel at 2 ms (unicast) and a slow transmitter's at
/// 10 ms (unicast).
fn network() -> Shared {
    let n = shared_network();
    n.records.add("other-device", "TX 3", adv(OTHER_IP, 3));
    let slow = Advertised {
        latency_ns: 10 * MS,
        ..adv(SLOW_IP, 5)
    };
    n.records.add("slow-device", "TX 5", slow);
    n
}

/// A device at `latency_ns` receiving the peer's multicast flow on RX 1,
/// the other device's unicast flow on RX 2 and the slow one's on RX 3,
/// every request granted at once; the time of the last tick.
fn running(latency_ns: u64, net: &Shared) -> (Manager, u64) {
    let cfg = DeviceConfig {
        latency_ns,
        ..cfg()
    };
    let mut m = Manager::new(&cfg, "test-device", OUR_IP, ports());
    m.set(0, Some(sub("TX 1", "peer-device")));
    m.set(1, Some(sub("TX 3", "other-device")));
    m.set(2, Some(sub("TX 5", "slow-device")));
    let mut now = 0;
    let mut heard = NONE;
    for k in 1..=3u64 {
        now = k * 10 * S;
        heard.iter_mut().for_each(|p| *p = k);
        for a in m.tick(now, &heard, net) {
            if let Action::Request { flow, .. } = a {
                m.answered(flow, Answer::Granted([0, 0, 0, 9, 0, 9]), now);
            }
        }
    }
    (m, now)
}

/// Each RX channel's flow: its stated latency, ns, and its plan's
/// playout and delay, samples.
fn flows(m: &Manager) -> Vec<(u64, u32, u16)> {
    (0..3)
        .map(|rx| {
            let slot = m.rx_route(rx).expect("routed").flow;
            let plan = m.rx_plan(slot);
            (m.flow(slot).unwrap().latency_ns, plan.latency, plan.delay)
        })
        .collect()
}

/// `ns` in whole samples at the test device's 96 kHz.
fn samples(ns: u64) -> u32 {
    u32::try_from(crate::clock::latency_samples(ns, 96_000)).unwrap()
}

/// The slow flow's delay past the device latency: its 10 ms over ours,
/// rounded up to whole blocks ([`Manager::read_at`]).
fn slow_delay(device_ns: u64) -> u16 {
    let block = u32::try_from(cfg().block.max(1)).unwrap();
    let delay = (samples(10 * MS) - samples(device_ns)).div_ceil(block) * block;
    u16::try_from(delay).unwrap()
}

#[test]
fn a_lower_latency_reaches_every_running_flow() {
    let net = network();
    let (mut m, now) = running(40 * MS, &net);
    assert_eq!(m.rx_plan(group_slot(&m)).group, Some(GROUP));
    let at_40 = (40 * MS, samples(40 * MS), 0);
    assert_eq!(flows(&m), [at_40; 3]);
    m.set_latency(5 * MS);
    let at_5 = (5 * MS, samples(5 * MS), 0);
    assert_eq!(
        flows(&m),
        [
            at_5,
            at_5,
            // Never below its transmitter's advertised latency.
            (10 * MS, samples(10 * MS), slow_delay(5 * MS)),
        ]
    );
    // Taken where it runs: nothing asked, nothing stopped, the same flows.
    let mut heard = NONE;
    heard.iter_mut().for_each(|p| *p = 9);
    assert_eq!(m.tick(now + S, &heard, &net), []);
    assert_eq!(flows(&m)[..2], [at_5; 2]);
}

#[test]
fn a_higher_latency_reaches_every_running_flow_too() {
    let net = network();
    let (mut m, _) = running(5 * MS, &net);
    let at_20 = (20 * MS, samples(20 * MS), 0);
    m.set_latency(20 * MS);
    assert_eq!(flows(&m), [at_20; 3]);
    // And back below the slow transmitter's: it keeps its own again.
    m.set_latency(5 * MS);
    assert_eq!(
        flows(&m)[2],
        (10 * MS, samples(10 * MS), slow_delay(5 * MS))
    );
}

/// `group_slot` of the #297 tests, for the default device.
fn group_slot(m: &Manager) -> usize {
    (0..MAX_FLOWS)
        .find(|&s| m.rx_plan(s).group.is_some())
        .expect("a slot joins the group")
}
