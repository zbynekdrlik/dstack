// SPDX-License-Identifier: MIT OR Apache-2.0
//! dstack #352: a latency write (0x1101) on a device whose RX flows run
//! reaches them: the flow read (0x3200) then states the new latency, as
//! Dante Controller and dcontroller read it. The field case: a dasio
//! device written from 40 to 5 ms kept stating 40000 us on every flow.

use super::*;

use super::super::control::{latency_request_body, rx_flow_page};

/// Each listed RX flow's latency, ns, from one 0x3200 page.
fn stated(device: &mut Device, live: &Live<'_>) -> Vec<u32> {
    let read = request(NETAUDIO, 7, op::RX_FLOWS, 0, &DC_FIRST_PAGE);
    let ArcOutcome::Reply(bytes) = respond(device, live, &read) else {
        panic!("the flow list is answered");
    };
    let (h, body) = Header::parse(&bytes).unwrap();
    let page = rx_flow_page(h.op2, body).expect("the page reads");
    page.items.iter().map(|f| f.latency_ns).collect()
}

#[test]
fn a_latency_write_reaches_the_running_flows_the_flow_read_states() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config_in(dir.path());
    cfg.latency_ns = 40_000_000;
    let mut device = Device::load(cfg).unwrap();
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
    let tx = TxTable::new(7);
    assert_eq!(
        stated(&mut device, &Live { tx: &tx, rx: &rx }),
        [40_000_000; 2]
    );
    let write = request(
        NETAUDIO,
        8,
        op::SETTINGS_SET,
        0,
        &latency_request_body(5_000_000),
    );
    let ns = match respond(&mut device, &Live { tx: &tx, rx: &rx }, &write) {
        ArcOutcome::Changed(
            _,
            Change {
                latency: Some(ns), ..
            },
        ) => ns,
        other => panic!("{other:?}"),
    };
    // As `serve` takes it: the manager gets the new device latency.
    rx.set_latency(ns);
    assert_eq!(
        stated(&mut device, &Live { tx: &tx, rx: &rx }),
        [5_000_000; 2]
    );
}
