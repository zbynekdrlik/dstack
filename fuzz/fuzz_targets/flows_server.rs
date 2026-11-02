//! Flow control (UDP 4455): one datagram from a receiver, answered from
//! the test device's TX table.
#![no_main]

use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::LazyLock;

use dstack::device::DeviceConfig;
use dstack::flows::server;
use dstack::media::tx::TxTable;
use libfuzzer_sys::fuzz_target;

const DEVICE: &str = include_str!("../../config/test-device.toml");

/// Parsed once; the state each input changes is built fresh for it.
static CFG: LazyLock<DeviceConfig> =
    LazyLock::new(|| DeviceConfig::from_toml(DEVICE).expect("the test device"));

fuzz_target!(|data: &[u8]| {
    let cfg = &*CFG;
    let mut table = TxTable::new(1);
    let from = SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 11), 4455);
    let _ = server::handle(&mut table, cfg, from, data, 1_000_000_000);
});
