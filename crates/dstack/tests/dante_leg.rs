// SPDX-License-Identifier: MIT OR Apache-2.0
//! The generic Dante-leg filter (dstack #264) against the ports the code
//! binds: every port any device slot can take (ARC, CMC, flow control, each
//! RX and TX flow port of its block) and Dante's standard ports must be
//! accepted by both rulesets, `tools/linux/dante-leg.nft` and
//! `tools/windows/dante-leg.ps1`, and their flow range must be the code's
//! exactly. The sets are read from the files, the expectations from the
//! crate's constants, so the two never drift apart.

use std::ops::RangeInclusive;

use dstack::device::ports::{FLOW_PORT_FIRST, FLOW_PORT_LAST, PORT_SLOTS, TX_OFFSET, slot_bases};
use dstack::media::rx::MAX_FLOWS;

const NFT: &str = include_str!("../../../tools/linux/dante-leg.nft");
const PS1: &str = include_str!("../../../tools/windows/dante-leg.ps1");

/// `a-b` or `a` as a range of `T`.
fn span<T: Copy>(text: &str, parse: impl Fn(&str) -> T) -> RangeInclusive<T> {
    let text = text.trim();
    match text.split_once('-') {
        Some((a, b)) => parse(a)..=parse(b),
        None => parse(text)..=parse(text),
    }
}

fn port(text: &str) -> u16 {
    text.trim()
        .parse()
        .unwrap_or_else(|e| panic!("{text:?}: {e}"))
}

fn ipv4(text: &str) -> u32 {
    let text = text.trim();
    u32::from(
        text.parse::<std::net::Ipv4Addr>()
            .unwrap_or_else(|e| panic!("{text:?}: {e}")),
    )
}

/// The elements of the nft set `name`, as written.
fn nft_set(name: &str) -> Vec<String> {
    let start = NFT
        .find(&format!("set {name} {{"))
        .unwrap_or_else(|| panic!("no set {name} in dante-leg.nft"));
    let body = &NFT[start..];
    let body = &body[..body.find('}').expect("the set's end")];
    let elements = &body[body.find("elements = {").expect("the set's elements") + 12..];
    elements
        .split(',')
        .map(|e| e.trim().to_string())
        .filter(|e| !e.is_empty())
        .collect()
}

/// The PowerShell list `$script:<name> = @('a', 'b', ...)`, as written.
fn ps1_list(name: &str) -> Vec<String> {
    let head = format!("$script:{name} = @(");
    let start = PS1
        .find(&head)
        .unwrap_or_else(|| panic!("no {head} in dante-leg.ps1"));
    let body = &PS1[start + head.len()..];
    let body = &body[..body.find(')').expect("the list's end")];
    body.split(',')
        .map(|e| e.trim().trim_matches('\'').to_string())
        .filter(|e| !e.is_empty())
        .collect()
}

fn ports(list: &[String]) -> Vec<RangeInclusive<u16>> {
    list.iter().map(|e| span(e, port)).collect()
}

/// An IPv4 range from nft (`a.b.c.d/n`, `a-b`) or PowerShell (`a-b`).
fn addresses(list: &[String]) -> Vec<RangeInclusive<u32>> {
    list.iter()
        .map(|e| match e.split_once('/') {
            Some((addr, bits)) => {
                let first = ipv4(addr);
                let host = u32::MAX >> bits.parse::<u32>().unwrap();
                first..=first | host
            }
            None => span(e, ipv4),
        })
        .collect()
}

fn accepts<T: PartialOrd>(set: &[RangeInclusive<T>], v: &T) -> bool {
    set.iter().any(|r| r.contains(v))
}

/// Every UDP port a device of ours or Dante itself listens on.
fn every_bound_port() -> Vec<(String, u16)> {
    let mut all = vec![
        ("PTP event".to_string(), 319),
        ("PTP general".to_string(), 320),
        ("Audinate's multicast audio".to_string(), 4321),
        ("Audinate's unicast audio, first".to_string(), 14336),
        ("Audinate's unicast audio, last".to_string(), 14591),
        ("mDNS".to_string(), dstack::announce::responder::PORT),
    ];
    for p in dstack::info::REQUEST_PORT..=8708 {
        all.push((format!("info {p}"), p));
    }
    all.push(("info".to_string(), dstack::info::INFO_PORT));
    all.push(("heartbeat".to_string(), dstack::info::HEARTBEAT_PORT));
    for k in 0..PORT_SLOTS {
        all.push((format!("slot {k} ARC"), dstack::arc::PORT + k));
        all.push((
            format!("slot {k} flow control"),
            dstack::flows::control::PORT + k,
        ));
        all.push((format!("slot {k} CMC"), dstack::cmc::PORT + k));
        let (rx, tx) = slot_bases(k);
        for n in 0..MAX_FLOWS as u16 {
            all.push((format!("slot {k} RX flow {n}"), rx + n));
            all.push((format!("slot {k} TX flow {n}"), tx + n));
        }
    }
    all
}

#[test]
fn both_rulesets_accept_every_port_any_device_slot_can_bind() {
    let linux = ports(&nft_set("dante_udp"));
    let windows = ports(&ps1_list("DanteUdpPorts"));
    let all = every_bound_port();
    // 32 slots: 3 control ports and 64 flow ports each, and Dante's own ones.
    assert_eq!(all.len(), 6 + 9 + 2 + 32 * (3 + 64));
    for (what, p) in &all {
        assert!(accepts(&linux, p), "dante-leg.nft drops {what} (UDP {p})");
        assert!(
            accepts(&windows, p),
            "dante-leg.ps1 blocks {what} (UDP {p})"
        );
    }
}

#[test]
fn both_rulesets_name_the_code_s_flow_range_exactly() {
    assert_eq!(slot_bases(PORT_SLOTS - 1).1 + TX_OFFSET - 1, FLOW_PORT_LAST);
    let range = format!("{FLOW_PORT_FIRST}-{FLOW_PORT_LAST}");
    for (file, set) in [
        ("nft", nft_set("dante_udp")),
        ("ps1", ps1_list("DanteUdpPorts")),
    ] {
        assert!(set.contains(&range), "{file}: {set:?} lacks {range}");
        let accepted = ports(&set);
        for outside in [FLOW_PORT_FIRST - 1, FLOW_PORT_LAST + 1] {
            assert!(
                !accepted.contains(&(outside..=outside)) && !accepts(&accepted, &outside),
                "{file} accepts {outside}, outside the dstack flow range"
            );
        }
    }
}

#[test]
fn both_rulesets_carry_the_same_sets() {
    assert_eq!(
        nft_set("dante_udp"),
        ps1_list("DanteUdpPorts"),
        "the UDP ports"
    );
    assert_eq!(
        nft_set("remote_tcp"),
        ps1_list("RemoteTcpPorts"),
        "remote access"
    );
    assert_eq!(nft_set("remote_tcp"), ["22", "8092"]);
    assert_eq!(
        addresses(&nft_set("dante_groups")),
        addresses(&ps1_list("DanteGroups")),
        "the Dante groups"
    );
    // mDNS answers (from 5353) reach only the OS's ephemeral ports; a DHCP
    // server's answer reaches 68 (Linux; the Windows Dante leg is static).
    assert!(NFT.contains("udp sport 5353 udp dport 32768-60999 accept"));
    assert!(NFT.contains("udp sport 67 udp dport 68 accept"));
    assert_eq!(ps1_list("MdnsSourcePorts"), ["5353"]);
    assert_eq!(ps1_list("EphemeralPorts"), ["49152-65535"]);
}

#[test]
fn both_rulesets_accept_dante_s_groups() {
    let linux = addresses(&nft_set("dante_groups"));
    let windows = addresses(&ps1_list("DanteGroups"));
    for (what, addr) in [
        ("the info group", "224.0.0.231"),
        ("the heartbeat group", "224.0.0.233"),
        ("mDNS", "224.0.0.251"),
        ("PTP", "224.0.1.129"),
        ("PTP domains 1-3", "224.0.1.132"),
        ("a multicast audio flow", "239.255.12.34"),
    ] {
        let a = ipv4(addr);
        assert!(accepts(&linux, &a), "dante-leg.nft drops {what} ({addr})");
        assert!(
            accepts(&windows, &a),
            "dante-leg.ps1 blocks {what} ({addr})"
        );
    }
    for other in ["224.0.1.1", "230.1.2.3", "240.0.0.1"] {
        let a = ipv4(other);
        assert!(
            !accepts(&linux, &a) && !accepts(&windows, &a),
            "{other} is no Dante group"
        );
    }
}
