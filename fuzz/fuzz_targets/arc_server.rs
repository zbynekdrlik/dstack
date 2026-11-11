//! ARC (UDP 4440): one datagram from anyone on the network, answered from
//! the synthetic test device's state, its TX table and its RX manager: by
//! [`arc::handle`], and by the server's own entry [`arc::answer`], which
//! also takes ARC 2.8.9's multicast create and delete (0x2601, 0x2602,
//! dstack #193) onto the TX table.
#![no_main]

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex, PoisonError};

use dstack::arc::{self, Flows, Live};
use dstack::device::{Device, DeviceConfig, DeviceState, Subscription};
use dstack::media::tx::TxTable;
use dstack::subscribe::{MAX_FLOWS, Manager};
use libfuzzer_sys::fuzz_target;

const DEVICE: &str = include_str!("../../config/test-device.toml");
const OUR_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 1);

/// Parsed once; the state each input changes is built fresh for it. Its
/// state file can never be written (a path under a file), so a change
/// `answer` saves is refused and undone, with no file left behind.
static CFG: LazyLock<DeviceConfig> = LazyLock::new(|| DeviceConfig {
    state_file: PathBuf::from("/dev/null/state.toml"),
    ..DeviceConfig::from_toml(DEVICE).expect("the test device")
});

/// The running flows `answer` reads and changes: a TX table and a manager.
struct Running {
    tx: Mutex<TxTable>,
    rx: Mutex<Manager>,
}

impl Flows for Running {
    fn view<R>(&self, f: impl FnOnce(&Live<'_>) -> R) -> R {
        let tx = self.tx.lock().unwrap_or_else(PoisonError::into_inner);
        let rx = self.rx.lock().unwrap_or_else(PoisonError::into_inner);
        f(&Live { tx: &tx, rx: &rx })
    }

    fn subscribe(&self, subs: &[Option<Subscription>]) -> Vec<usize> {
        let mut rx = self.rx.lock().unwrap_or_else(PoisonError::into_inner);
        rx.load(subs);
        rx.changed()
    }

    fn renamed(&self, _name: &str) {}

    fn set_latency(&self, _ns: u64) {}

    fn change_tx<R>(&self, f: impl FnOnce(&mut TxTable, Ipv4Addr) -> R) -> R {
        let mut tx = self.tx.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut tx, OUR_IP)
    }
}

fuzz_target!(|data: &[u8]| {
    let cfg = &*CFG;
    let mut state = DeviceState::from_config(cfg);
    let tx = TxTable::new(1);
    let rx = Manager::new(cfg, "fuzz", OUR_IP, [0; MAX_FLOWS]);
    let _ = arc::handle(&mut state, cfg, &Live { tx: &tx, rx: &rx }, data);
    let mut device = Device {
        cfg: cfg.clone(),
        state: DeviceState::from_config(cfg),
        status_port: None,
    };
    let running = Running {
        tx: Mutex::new(TxTable::new(1)),
        rx: Mutex::new(Manager::new(cfg, "fuzz", OUR_IP, [0; MAX_FLOWS])),
    };
    let _ = arc::answer(&mut device, &running, data);
});
