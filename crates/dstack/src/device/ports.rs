// SPDX-License-Identifier: MIT OR Apache-2.0
//! The fixed flow-port ranges a config may ask for (`tx_port_base`, dstack
//! #262): which ports they must stay clear of, as pure functions.

use std::ops::RangeInclusive;

use crate::media::rx::{MAX_FLOWS, last_port};

/// Device slots on one address: slot `k` takes ARC 4440+k, flow control
/// 4455+k and CMC 8800+k (`docs/two-networks.md`'s slot ranges).
const PORT_SLOTS: u16 = 32;

/// The [`MAX_FLOWS`] flow ports from `base`, `None` when they would pass
/// 65535.
fn flow_ports(base: u16) -> Option<RangeInclusive<u16>> {
    last_port(base).map(|last| base..=last)
}

/// What `tx_port_base`'s ports from `base` clash with, `None` when they lie
/// in 1024..=65535 clear of the RX flow ports (`rx_base`), the device's own
/// control ports (`own`: ARC, CMC, flow control), every slot's control
/// range (ARC 4440-4471, flow control 4455-4486, CMC 8800-8831), mDNS and
/// the info ports (8700, 8702, 8708).
pub(super) fn tx_clash(base: u16, rx_base: Option<u16>, own: [u16; 3]) -> Option<String> {
    let Some(tx) = flow_ports(base).filter(|_| base >= 1024) else {
        return Some("out of range".to_string());
    };
    let slot = |first: u16| first..=first + (PORT_SLOTS - 1);
    let one = |p: u16| p..=p;
    let mut taken = vec![
        ("every slot's ARC ports", slot(crate::arc::PORT)),
        (
            "every slot's flow-control ports",
            slot(crate::flows::control::PORT),
        ),
        ("every slot's CMC ports", slot(crate::cmc::PORT)),
        ("the device's ARC port", one(own[0])),
        ("the device's CMC port", one(own[1])),
        ("the device's flow-control port", one(own[2])),
        ("mDNS", one(crate::announce::responder::PORT)),
        ("the info request port", one(crate::info::REQUEST_PORT)),
        ("the info port", one(crate::info::INFO_PORT)),
        ("the heartbeat port", one(crate::info::HEARTBEAT_PORT)),
    ];
    // An RX base past 65535 is refused on its own line; its ports up to
    // 65535 still count.
    if let Some(rx) = rx_base {
        let last = last_port(rx).unwrap_or(u16::MAX);
        taken.push(("rx_port_base's ports", rx..=last));
    }
    taken
        .into_iter()
        .find(|(_, r)| r.start() <= tx.end() && tx.start() <= r.end())
        .map(|(name, r)| {
            format!(
                "{}-{} overlaps {name} ({}-{})",
                tx.start(),
                tx.end(),
                r.start(),
                r.end()
            )
        })
}

// The TX side bounds its range with the RX side's `last_port`: one count.
const _: () = assert!(MAX_FLOWS == crate::media::tx::MAX_FLOWS);
