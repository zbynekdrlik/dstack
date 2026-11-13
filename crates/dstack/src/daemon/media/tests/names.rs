// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #266: the driver's lines name an RX flow's transmitter (its Dante
//! name and address): the request, the answer, the stop, and the tick's loss
//! warning.

use super::*;
use crate::test_log::Captured;

/// The one line of `log` with `msg`.
fn line_with<'a>(log: &'a str, msg: &str) -> &'a str {
    let lines: Vec<&str> = log.lines().filter(|l| l.contains(msg)).collect();
    assert_eq!(lines.len(), 1, "{msg:?} in {log}");
    lines[0]
}

#[tokio::test(flavor = "current_thread")]
async fn the_driver_s_lines_name_the_flow_s_transmitter() {
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_on(&device, Arc::new(ManualClock::new(T)));
    let handle: Handle = [0, 0, 0, 3, 0xab, 0xcd];
    let mut t = stand_in(Ipv4Addr::LOCALHOST, Grant::Handle(handle)).await;
    let records = Records::default().with("FOH-Card", "TX 2", advert(t.addr, 2));
    lock(&media.manager).set(4, Some(sub("TX 2", "FOH-Card")));
    let (mut driver, mut answers, _changes) = driver_of(&media);
    let log = Captured::start(tracing::Level::INFO);
    driver.tick(&records);
    answer_next(&mut driver, &mut answers).await;
    let slot = flow_slots(&media)[0];
    // The transmitter's audio: two packets 32 frames apart, so a gap.
    let mut receiver = Receiver::new(Arc::clone(&media.rx)).unwrap();
    let audio = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut buf = [0u8; 1472];
    for sub in [0, 64] {
        let n = packet::write(&mut buf, 1000, sub, 2, 3, 32, |_, _| 0);
        audio
            .send_to(&buf[..n], ("127.0.0.1", media.rx.ports().unwrap()[slot]))
            .unwrap();
    }
    let deadline = Instant::now() + BOUND;
    while media.rx.lane(slot).packets < 2 {
        assert!(Instant::now() < deadline, "{:?}", media.rx.lane(slot));
        receiver.wait(Duration::from_millis(10)).unwrap();
        receiver.pass(Some(T), media.rx.now_ns());
    }
    assert_eq!(media.rx.lane(slot).gaps, 1);
    std::thread::sleep(Duration::from_millis(1_010));
    driver.tick(&records);
    tokio::time::timeout(BOUND, driver.stop())
        .await
        .expect("the stop ends within the bound");
    let got = tokio::time::timeout(BOUND, t.stops.recv()).await.unwrap();
    assert_eq!(got, Some(handle));
    let text = log.text();
    let to = t.addr;
    let source = audio.local_addr().unwrap();
    let requested = line_with(&text, "RX flow requested");
    assert!(
        requested.contains(&format!("device=FOH-Card to={to} peer=1 flow={}", slot + 1)),
        "{requested}"
    );
    let granted = line_with(&text, "RX flow granted");
    assert!(
        granted.contains(&format!("device=FOH-Card flow={}", slot + 1)),
        "{granted}"
    );
    let missing = line_with(&text, "never reached our socket");
    assert!(missing.contains(" WARN "), "{missing}");
    assert!(
        missing.contains(&format!(
            "device=FOH-Card source={source} slot={slot} gaps=1"
        )),
        "{missing}"
    );
    let stop = line_with(&text, "RX flow stop sent");
    assert!(
        stop.contains(&format!("device=FOH-Card to={to} peer=1")),
        "{stop}"
    );
    t.task.abort();
}

/// An answer to a flow the manager no longer holds (another generation in
/// its slot, or none) names no device: `-`, never another flow's.
#[tokio::test(flavor = "current_thread")]
async fn an_answer_to_a_flow_gone_from_its_slot_names_no_device() {
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_on(&device, Arc::new(Unready));
    let t = stand_in(
        Ipv4Addr::LOCALHOST,
        Grant::Refuse(status::TOO_MANY_TX_FLOWS),
    )
    .await;
    let records = Records::default().with("FOH-Card", "TX 1", advert(t.addr, 1));
    lock(&media.manager).set(0, Some(sub("TX 1", "FOH-Card")));
    let (mut driver, mut answers, _changes) = driver_of(&media);
    driver.tick(&records);
    let (flow, answer) = tokio::time::timeout(BOUND, answers.recv())
        .await
        .unwrap()
        .unwrap();
    let stale = FlowId {
        slot: flow.slot,
        generation: flow.generation.wrapping_add(1),
    };
    let log = Captured::start(tracing::Level::INFO);
    driver.answered(stale, Answer::Failed);
    driver.answered(flow, answer);
    let text = log.text();
    let failed = line_with(&text, "RX flow request had no answer");
    assert!(
        failed.contains(&format!("device=- flow={}", flow.slot + 1)),
        "{failed}"
    );
    let refused = line_with(&text, "RX flow refused");
    assert!(
        refused.contains(&format!("device=FOH-Card flow={}", flow.slot + 1)),
        "{refused}"
    );
    t.task.abort();
}
