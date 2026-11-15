// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The status's `network` part (dstack #321): the floor, the binaries
//! below it (the meter's peers, the local clients) and the clients.

use super::*;
use crate::discover::senders::Meter;
use crate::network_floor::{Guard, Kind, Version, now_ns};

/// Absent with no meter and no client; with a meter, the floor and empty
/// lists; then the meter's old peer and an old local client below the
/// floor, every client listed with its version.
#[test]
fn the_status_shows_the_floor_the_binaries_below_it_and_the_clients() {
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_on(&device, Arc::new(Unready));
    assert!(!status(&media).contains("network"));
    media.set_mdns_meter(Arc::new(Meter::holding(None)));
    assert!(
        status(&media)
            .contains("\"network\":{\"floor\":\"0.6.58\",\"below_floor\":[],\"clients\":[]}"),
        "{}",
        status(&media)
    );
    let mut floor = Guard::default();
    floor.see(
        Kind::Peer,
        "status-old-peer",
        Some(Ipv4Addr::new(192, 0, 2, 5)),
        Version::new(0, 6, 20),
        now_ns(),
    );
    media.set_mdns_meter(Arc::new(Meter::holding_floor(None, floor)));
    media.client_asked("dtray", Version::new(0, 6, 144));
    media.client_asked("status-old-client", Version::new(0, 6, 57));
    let s = status(&media);
    for word in words_of(&s, "test-device") {
        assert!(
            WORDS.contains(&word.as_str())
                || word.parse::<Ipv4Addr>().is_ok()
                || Version::parse(&word).is_some()
                || ["name", "status-old-peer", "status-old-client", "dtray"]
                    .contains(&word.as_str()),
            "{word:?} in {s}"
        );
    }
    assert!(
        s.contains(
            "\"network\":{\"floor\":\"0.6.58\",\"below_floor\":[\
             {\"kind\":\"peer\",\"name\":\"status-old-peer\",\"address\":\"192.0.2.5\",\
             \"version\":\"0.6.20\"},\
             {\"kind\":\"client\",\"name\":\"status-old-client\",\"version\":\"0.6.57\"}],\
             \"clients\":[{\"name\":\"dtray\",\"version\":\"0.6.144\"},\
             {\"name\":\"status-old-client\",\"version\":\"0.6.57\"}]}"
        ),
        "{s}"
    );
}

/// A client alone, with no meter, brings the part: its peers' list is
/// empty.
#[test]
fn a_client_alone_brings_the_network_part() {
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_on(&device, Arc::new(Unready));
    media.client_asked("dtray", Version::new(0, 6, 58));
    assert!(
        status(&media).contains(
            "\"network\":{\"floor\":\"0.6.58\",\"below_floor\":[],\
             \"clients\":[{\"name\":\"dtray\",\"version\":\"0.6.58\"}]}"
        ),
        "{}",
        status(&media)
    );
    assert_eq!(media.peers_below_floor(), None);
}
