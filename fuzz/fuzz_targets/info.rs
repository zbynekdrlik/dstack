//! The info requests (UDP 8700): one datagram, answered for the info group.
#![no_main]

use std::net::Ipv4Addr;
use std::sync::LazyLock;

use dstack::device::DeviceConfig;
use dstack::info::{Identity, InfoBuilder, NetInfo};
use libfuzzer_sys::fuzz_target;

const DEVICE: &str = include_str!("../../config/test-device.toml");

/// Parsed once; the state each input changes is built fresh for it.
static CFG: LazyLock<DeviceConfig> =
    LazyLock::new(|| DeviceConfig::from_toml(DEVICE).expect("the test device"));

fuzz_target!(|data: &[u8]| {
    let cfg = &*CFG;
    let net = NetInfo {
        mac: [2, 0, 0, 0, 0, 1],
        ip: Ipv4Addr::new(10, 0, 0, 1),
        netmask: Ipv4Addr::new(255, 255, 255, 0),
        gateway: Ipv4Addr::UNSPECIFIED,
        link_speed: 1_000,
    };
    let mut m = InfoBuilder::new(Identity::new(cfg, net));
    let _ = m.answer(data);
});
