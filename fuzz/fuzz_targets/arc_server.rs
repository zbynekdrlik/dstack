//! ARC (UDP 4440): one datagram from anyone on the network, answered from
//! the synthetic test device's state, its TX table and its RX manager.
#![no_main]

use std::net::Ipv4Addr;
use std::sync::LazyLock;

use dstack::arc::{self, Live};
use dstack::device::{DeviceConfig, DeviceState};
use dstack::media::tx::TxTable;
use dstack::subscribe::{MAX_FLOWS, Manager};
use libfuzzer_sys::fuzz_target;

const DEVICE: &str = include_str!("../../config/test-device.toml");

/// Parsed once; the state each input changes is built fresh for it.
static CFG: LazyLock<DeviceConfig> =
    LazyLock::new(|| DeviceConfig::from_toml(DEVICE).expect("the test device"));

fuzz_target!(|data: &[u8]| {
    let cfg = &*CFG;
    let mut state = DeviceState::from_config(cfg);
    let tx = TxTable::new(1);
    let rx = Manager::new(cfg, "fuzz", Ipv4Addr::new(10, 0, 0, 1), [0; MAX_FLOWS]);
    let _ = arc::handle(&mut state, cfg, &Live { tx: &tx, rx: &rx }, data);
});
