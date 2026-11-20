// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #464: the media side gives the (address, port) pairs the
//! manager's published plan receives on, through the driver's tick.

use super::*;

#[tokio::test]
async fn the_media_side_gives_a_requested_flow_s_pair_and_keeps_it_through_the_grant() {
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_on(&device, Arc::new(ManualClock::new(T)));
    assert_eq!(media.rx_destinations().generation, 0);
    let handle: Handle = [0, 0, 0, 3, 0xab, 0xcd];
    let t = stand_in(Ipv4Addr::LOCALHOST, Grant::Handle(handle)).await;
    let records = Records::default().with("peer-device", "TX 2", advert(t.addr, 2));
    lock(&media.manager).set(4, Some(sub("TX 2", "peer-device")));
    let (mut driver, mut answers, _changes) = driver_of(&media);
    driver.tick(&records);
    // Asked for, not granted yet: its pair is there already.
    let slot = flow_slots(&media)[0];
    let port = media.rx.ports().unwrap()[slot];
    let asked = media.rx_destinations();
    assert_eq!(asked.generation, 1);
    assert_eq!(asked.pairs, [SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)]);
    // The grant changes the plan, not the pairs.
    assert_eq!(
        answer_next(&mut driver, &mut answers).await,
        Answer::Granted(handle)
    );
    driver.tick(&records);
    assert_eq!(media.rx_destinations(), asked);
    t.task.abort();
}
