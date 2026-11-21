// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #403: the "RX flow granted" line carries the flow's read as the
//! status names it (`latency`, `playout`, `delay`, `advertised`, samples at
//! the device's rate), so an app's log shows a transmitter's latency moving
//! its audio (#402: stream-obs read mbc-snv's 40 ms flow 28 ms later, and
//! no log said so).

use super::*;
use crate::test_log::Captured;

/// The fields of the one line of `log` with `msg`, after the message.
fn fields_of(log: &str, msg: &str) -> Vec<String> {
    let lines: Vec<&str> = log.lines().filter(|l| l.contains(msg)).collect();
    assert_eq!(lines.len(), 1, "{msg:?} in {log}");
    let (_, after) = lines[0].split_once(msg).unwrap();
    after.split_whitespace().map(str::to_string).collect()
}

/// The grant line of a flow from a transmitter advertising `advertised_ns`
/// to the test device (96 kHz, 1 ms = 96 samples, block 32).
async fn granted_line(advertised_ns: u64) -> (Vec<String>, usize) {
    let (log, slot) = granted_log(advertised_ns, false).await;
    (fields_of(&log, "RX flow granted"), slot)
}

/// The INFO log of that grant, the device leading or not (dstack #476:
/// `rx_handoff_lead`, H 28 of its 96), and the flow's slot.
async fn granted_log(advertised_ns: u64, leads: bool) -> (String, usize) {
    let dir = tempfile::tempdir().unwrap();
    let device = if leads {
        let cfg = DeviceConfig {
            state_file: dir.path().join("state.toml"),
            rx_handoff_lead: true,
            ..DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-device.toml")
        };
        Device::load(cfg).unwrap()
    } else {
        device_at(dir.path())
    };
    let media = media_on(&device, Arc::new(Unready));
    let t = stand_in(Ipv4Addr::LOCALHOST, Grant::Handle([0, 0, 0, 7, 1, 2])).await;
    let records = Records::default().with(
        "FOH-Card",
        "TX 2",
        Advertised {
            latency_ns: advertised_ns,
            ..advert(t.addr, 2)
        },
    );
    lock(&media.manager).set(0, Some(sub("TX 2", "FOH-Card")));
    let (mut driver, mut answers, _changes) = driver_of(&media);
    let log = Captured::start(tracing::Level::INFO);
    driver.tick(&records);
    assert!(matches!(
        answer_next(&mut driver, &mut answers).await,
        Answer::Granted(_)
    ));
    let slot = flow_slots(&media)[0];
    t.task.abort();
    (log.text(), slot)
}

/// A transmitter over our latency: the flow's latency is its 1.5 ms (144),
/// read 48 samples later than our 96, rounded up to whole blocks of 32: a
/// delay of 64, played out at 160.
#[tokio::test(flavor = "current_thread")]
async fn the_grant_line_carries_a_flow_read_later_than_ours() {
    let (fields, slot) = granted_line(1_500_000).await;
    assert_eq!(
        fields,
        [
            "device=FOH-Card".to_string(),
            format!("flow={}", slot + 1),
            "latency=144".to_string(),
            "playout=160".to_string(),
            "delay=64".to_string(),
            "advertised=144".to_string(),
        ]
    );
}

/// A transmitter under our latency: the flow is read at ours (96), no
/// delay; its own 0.5 ms (48) still shows.
#[tokio::test(flavor = "current_thread")]
async fn the_grant_line_carries_a_flow_read_at_our_latency() {
    let (fields, slot) = granted_line(500_000).await;
    assert_eq!(
        fields,
        [
            "device=FOH-Card".to_string(),
            format!("flow={}", slot + 1),
            "latency=96".to_string(),
            "playout=96".to_string(),
            "delay=0".to_string(),
            "advertised=48".to_string(),
        ]
    );
}

/// A grant of a flow the manager no longer holds names no device and no
/// read: never another flow's figures.
#[tokio::test(flavor = "current_thread")]
async fn a_grant_of_a_flow_gone_from_its_slot_carries_no_read() {
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_on(&device, Arc::new(Unready));
    let t = stand_in(Ipv4Addr::LOCALHOST, Grant::Handle([0, 0, 0, 9, 1, 2])).await;
    let records = Records::default().with("FOH-Card", "TX 1", advert(t.addr, 1));
    lock(&media.manager).set(0, Some(sub("TX 1", "FOH-Card")));
    let (mut driver, mut answers, _changes) = driver_of(&media);
    driver.tick(&records);
    let (flow, answer) = tokio::time::timeout(BOUND, answers.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(answer, Answer::Granted(_)));
    let stale = FlowId {
        slot: flow.slot,
        generation: flow.generation.wrapping_sub(1),
    };
    let log = Captured::start(tracing::Level::INFO);
    driver.answered(stale, answer);
    assert_eq!(
        fields_of(&log.text(), "RX flow granted"),
        ["device=-".to_string(), format!("flow={}", flow.slot + 1)]
    );
    t.task.abort();
}

/// dstack #476: a leading device reads at P - H (96 - 28 = 68), so a
/// transmitter at 1.5 ms (144) is read 76 later, 96 in whole blocks of 32,
/// and its grant names that once, with the numbers.
#[tokio::test(flavor = "current_thread")]
async fn a_flow_read_later_because_of_the_lead_is_logged_once_with_its_numbers() {
    let (log, slot) = granted_log(1_500_000, true).await;
    assert_eq!(
        fields_of(&log, "RX flow granted"),
        [
            "device=FOH-Card".to_string(),
            format!("flow={}", slot + 1),
            "latency=144".to_string(),
            "playout=164".to_string(),
            "delay=96".to_string(),
            "advertised=144".to_string(),
        ]
    );
    assert_eq!(
        fields_of(&log, LEAD_READ_LATER),
        [
            "device=FOH-Card".to_string(),
            format!("flow={}", slot + 1),
            "advertised=144".to_string(),
            "latency=96".to_string(),
            "lead=28".to_string(),
            "read_point=68".to_string(),
            "delay=96".to_string(),
        ]
    );
    // Under the read point: read at 68, nothing more to say.
    let (log, _) = granted_log(500_000, true).await;
    assert!(log.contains("playout=68 delay=0"), "{log}");
    assert!(!log.contains(LEAD_READ_LATER), "{log}");
    // Read later without a lead (past P itself): no lead line either.
    let (log, _) = granted_log(1_500_000, false).await;
    assert!(log.contains("delay=64"), "{log}");
    assert!(!log.contains(LEAD_READ_LATER), "{log}");
}
