// SPDX-License-Identifier: MIT OR Apache-2.0
//! dstack #493: an RX flow is listed at the latency the pump plays it at.
//! A flow whose transmitter has a `playout_override` plays at that
//! override (at least its frames a packet, at least the device latency),
//! not at the transmitter's advertised latency, and both RX flow lists
//! (0x3200, what dcontroller and Dante Controller judge each flow's
//! lateness against, and ARC 2.8.9's 0x3600) state it. The field case:
//! iem-mixer's vocal at an override of 64 samples (0.67 ms at 96 kHz),
//! advertised ~1 ms, shown at 1 ms in dcontroller.

use super::v289::{card, page_289, rx_flow};
use super::*;

use super::super::client::ReadError;
use super::super::control::{Call, latency_request_body, rx_flows};

/// The override for the peer's flow, samples (as iem-mixer's vocal).
const OVERRIDE: u32 = 64;
/// 64 samples at 96 kHz as a flow list states it (whole samples, ns).
const OVERRIDE_NS: u32 = 666_666;
/// The transmitters' advertised latency ([`adv`]).
const ADVERTISED_NS: u32 = 2_000_000;

/// The device as `serve` holds it: its config and state, and the flows.
struct Rig {
    device: Device,
    tx: TxTable,
    rx: Manager,
    seq: u16,
}

impl Rig {
    /// Our answer to `req`.
    fn send(&mut self, req: &[u8]) -> ArcOutcome {
        let live = Live {
            tx: &self.tx,
            rx: &self.rx,
        };
        respond(&mut self.device, &live, req)
    }

    /// A controller's latency write (0x1101), taken as `serve` takes it:
    /// the manager gets the new device latency.
    fn write_latency(&mut self, ns: u32) {
        self.seq = self.seq.wrapping_add(1);
        let write = request(
            NETAUDIO,
            self.seq,
            op::SETTINGS_SET,
            0,
            &latency_request_body(ns),
        );
        let ns = match self.send(&write) {
            ArcOutcome::Changed(
                _,
                Change {
                    latency: Some(ns), ..
                },
            ) => ns,
            other => panic!("{other:?}"),
        };
        self.rx.set_latency(ns);
    }
}

/// dcontroller's read: [`rx_flows`] over our answers.
impl Call for Rig {
    fn call(&mut self, op1: u16, body: &[u8]) -> Result<(u16, Vec<u8>), ReadError> {
        self.seq = self.seq.wrapping_add(1);
        let bytes = sent_bytes(self.send(&request(NETAUDIO, self.seq, op1, 0, body)));
        let (h, body) = Header::parse(&bytes).expect("a reply parses");
        Ok((h.op2, body.to_vec()))
    }
}

/// (the first RX channel fed, 1 up; the stated latency, ns) of each flow,
/// from 0x3200 through [`rx_flows`] and from 0x3600 by slot.
fn stated(rig: &mut Rig) -> (Vec<(u16, u32)>, Vec<(u16, u32)>) {
    let listed = rx_flows(rig).expect("0x3200 reads");
    let by_slot: Vec<(u16, u16)> = listed.iter().map(|f| (f.slot, f.channels[0])).collect();
    let v27 = listed
        .iter()
        .map(|f| (f.channels[0], f.latency_ns))
        .collect();
    let reply = sent_bytes(rig.send(&card("requests", "0x3600")));
    let v289 = page_289(&reply)
        .records
        .iter()
        .map(|&at| {
            let f = rx_flow(&reply, at);
            let rx = by_slot
                .iter()
                .find(|(slot, _)| *slot == f.number)
                .expect("0x3600 lists the 0x3200 flows")
                .1;
            (rx, f.latency_ns)
        })
        .collect();
    (v27, v289)
}

#[test]
fn an_overridden_flow_is_listed_at_its_played_latency_and_follows_a_write() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config_in(dir.path());
    // Our device latency (48 samples) is under the override, so the flow
    // plays at the override.
    cfg.latency_ns = 500_000;
    // Our TX keeps its own latency, so a write may go back under the TX
    // floor (`DeviceConfig::latency_range`).
    cfg.tx_latency_ns = Some(1_000_000);
    cfg.playout_override
        .insert("peer-device".to_string(), OVERRIDE);
    let device = Device::load(cfg).unwrap();
    let mut rx = manager_of(&device.cfg);
    let net = Net(vec![
        ("peer-device".into(), "TX 1".into(), adv(PEER_IP, 1)),
        (
            "other-device".into(),
            "TX 2".into(),
            adv(Ipv4Addr::new(10, 0, 0, 12), 2),
        ),
    ]);
    receiving(
        &mut rx,
        &[
            (0, sub("TX 1", "peer-device")),
            (1, sub("TX 2", "other-device")),
        ],
        &net,
    );
    let mut rig = Rig {
        device,
        tx: TxTable::new(7),
        rx,
        seq: 0,
    };
    // RX 1 is the peer's overridden flow, RX 2 the other's granted one.
    let want = vec![(1, OVERRIDE_NS), (2, ADVERTISED_NS)];
    assert_eq!(stated(&mut rig), (want.clone(), want));

    // A controller's latency write over both: each flow plays at the
    // device latency, and both lists state it.
    rig.write_latency(5_000_000);
    let want = vec![(1, 5_000_000), (2, 5_000_000)];
    assert_eq!(stated(&mut rig), (want.clone(), want));

    // Back under the override: the overridden flow plays at it again, the
    // other at its transmitter's latency.
    rig.write_latency(500_000);
    let want = vec![(1, OVERRIDE_NS), (2, ADVERTISED_NS)];
    assert_eq!(stated(&mut rig), (want.clone(), want));
}
