// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #411: the daemon's tick keeps the TX flow sockets' multicast
//! send interface across an interface re-create, as it keeps the RX flows'
//! groups (#282). The test moves a fake list's index and reads each flow
//! socket's `IP_MULTICAST_IF` back.

use super::*;
use crate::membership::fake::Fake;

/// Each TX flow socket's multicast send interface.
fn send_ifs(media: &Media) -> Vec<Ipv4Addr> {
    (media.tx.flow_sockets().iter())
        .map(|s| socket2::SockRef::from(s).multicast_if_v4().unwrap())
        .collect()
}

/// The option as a re-created interface leaves it: naming nothing.
fn spoil(media: &Media) {
    for s in media.tx.flow_sockets() {
        socket2::SockRef::from(s)
            .set_multicast_if_v4(&Ipv4Addr::UNSPECIFIED)
            .unwrap();
    }
}

/// A tick once the fake list's period (1 ms) has passed, so it reads.
fn tick_after_period(driver: &mut Driver, records: &Records) {
    std::thread::sleep(Duration::from_millis(2));
    driver.tick(records);
}

#[tokio::test(flavor = "current_thread")]
async fn after_an_interface_re_create_the_tick_names_the_tx_flow_sockets_interface_again() {
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_on(&device, Arc::new(Unready));
    let records = Records::default();
    let (mut driver, _answers, _changes) = driver_of(&media);
    // A NIC counter of our own: loopback has none on every OS.
    driver.set_nic(|_| Some(0));
    let fake = Fake::at(4);
    driver.set_lister(fake.lister(Ipv4Addr::LOCALHOST, Duration::from_millis(1)));
    let n = media.tx.flow_sockets().len();
    assert!(n > 1, "a device with several flow slots");
    driver.tick(&records);
    assert!(fake.reads() > 0, "the first tick read the list");
    spoil(&media);
    let before = fake.reads();
    tick_after_period(&mut driver, &records);
    assert!(fake.reads() > before, "the tick read the list again");
    assert_eq!(
        send_ifs(&media),
        vec![Ipv4Addr::UNSPECIFIED; n],
        "nothing while the index holds"
    );
    fake.recreate(9);
    tick_after_period(&mut driver, &records);
    assert_eq!(
        send_ifs(&media),
        vec![Ipv4Addr::LOCALHOST; n],
        "every flow socket named again"
    );
}
