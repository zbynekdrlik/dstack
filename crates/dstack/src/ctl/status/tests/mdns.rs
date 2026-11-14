// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The status's `mdns` part (dstack #289).

use super::*;
use crate::discover::senders::{Meter, Minute, Rate};

/// The last minute of the process's mDNS meter: the total a second, the
/// loudest sources and those over the cap; absent with no meter or before
/// its first minute.
#[test]
fn the_status_shows_the_mdns_meter_s_last_minute() {
    let dir = tempfile::tempdir().unwrap();
    let device = device_at(dir.path());
    let media = media_on(&device, Arc::new(Unready));
    assert!(!status(&media).contains("mdns"));
    media.set_mdns_meter(Arc::new(Meter::holding(None)));
    assert!(!status(&media).contains("mdns"));
    let flood = Rate {
        source: Ipv4Addr::new(192, 0, 2, 66),
        packets: 7200,
    };
    let quiet = Rate {
        source: Ipv4Addr::new(192, 0, 2, 1),
        packets: 6,
    };
    media.set_mdns_meter(Arc::new(Meter::holding(Some(Minute {
        ns: 60_000_000_000,
        total: 7206,
        top: vec![flood, quiet],
        over_cap: vec![flood],
    }))));
    let s = status(&media);
    for word in words_of(&s, "test-device") {
        assert!(
            WORDS.contains(&word.as_str()) || word.parse::<Ipv4Addr>().is_ok(),
            "{word:?} in {s}"
        );
    }
    assert!(
        s.contains(
            "\"mdns\":{\"per_s\":120.1,\"top\":[{\"source\":\"192.0.2.66\",\"per_s\":120.0},\
             {\"source\":\"192.0.2.1\",\"per_s\":0.1}],\"over_cap\":[{\"source\":\"192.0.2.66\",\
             \"per_s\":120.0}]}"
        ),
        "{s}"
    );
    assert_eq!(media.mdns_minute().map(|m| m.total), Some(7206));
}
