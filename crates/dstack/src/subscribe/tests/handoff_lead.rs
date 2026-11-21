// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #476: with the hand-off lead (the device config's
//! `rx_handoff_lead`) our flows are read at P - H, H 30 % of P; a flow whose
//! transmitter advertises more than P - H is read later, in whole blocks.

use super::*;

/// The test device at 1 ms (96 samples at 96 kHz), block 16, leading:
/// H 28, read point 68.
fn leading() -> DeviceConfig {
    let mut cfg = cfg();
    cfg.block = 16;
    cfg.rx_handoff_lead = true;
    cfg
}

#[test]
fn a_leading_device_reads_its_flows_at_p_less_h() {
    let net = Fake::default();
    net.add("Mics-01", "Vox", advertised_mic());
    let mut rack = adv(PEER_IP, 1);
    rack.latency_ns = 0;
    net.add("rack", "TX 1", rack);
    let mut m = Manager::new(&leading(), "test-device", OUR_IP, ports());
    assert_eq!(m.handoff_lead(), 28);
    m.set(0, Some(sub("Vox", "Mics-01")));
    m.set(1, Some(sub("TX 1", "rack")));
    run_with_mic(&mut m, &net, 1, |_| Answer::Granted([0, 0, 0, 9, 0, 9]));
    // The mic's 1 ms (96) is past 68 by 28: 32 in whole blocks of 16, so
    // it plays at 100; without the lead it played at ours, 96.
    let mic = m.rx_route(0).expect("the mic routed").flow;
    assert_eq!((m.rx_plan(mic).latency, m.rx_plan(mic).delay), (100, 32));
    assert!(!m.flow(mic).unwrap().below_advertised);
    // The rack advertises nothing: read at the read point.
    let rack = m.rx_route(1).expect("the rack routed").flow;
    assert_eq!((m.rx_plan(rack).latency, m.rx_plan(rack).delay), (68, 0));
    // A new P keeps the share: 2 ms is 192, H 57, read at 135.
    m.set_latency(2_000_000);
    assert_eq!(m.handoff_lead(), 57);
    assert_eq!((m.rx_plan(mic).latency, m.rx_plan(mic).delay), (135, 0));
}

#[test]
fn a_device_without_the_lead_reads_at_p() {
    let mut cfg = leading();
    cfg.rx_handoff_lead = false;
    let net = Fake::default();
    net.add("Mics-01", "Vox", advertised_mic());
    let mut m = Manager::new(&cfg, "test-device", OUR_IP, ports());
    assert_eq!(m.handoff_lead(), 0);
    m.set(0, Some(sub("Vox", "Mics-01")));
    run_with_mic(&mut m, &net, 1, |_| Answer::Granted([0, 0, 0, 9, 0, 9]));
    let mic = m.rx_route(0).expect("the mic routed").flow;
    assert_eq!((m.rx_plan(mic).latency, m.rx_plan(mic).delay), (96, 0));
}
