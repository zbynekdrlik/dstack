use super::*;

use std::cell::RefCell;
use std::collections::HashMap;

use mdns_sd::ServiceInfo;

/// Every wait in these tests ends.
const BOUND: Duration = Duration::from_secs(5);

/// A resolved record of `service` for `name` at `ip`:`port` with `txt`.
fn record(service: &str, name: &str, ip: &str, port: u16, txt: &[(&str, &str)]) -> ServiceEvent {
    let props: HashMap<String, String> = txt
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let info = ServiceInfo::new(service, name, "peer.local.", ip, port, Some(props)).unwrap();
    ServiceEvent::ServiceResolved(Box::new(info.as_resolved_service()))
}

fn arc(name: &str, ip: &str, txt: &[(&str, &str)]) -> ServiceEvent {
    record(ARC_SERVICE, name, ip, 4440, txt)
}

fn dbc(name: &str, ip: &str) -> ServiceEvent {
    record(DBC_SERVICE, name, ip, 4455, &[])
}

fn cmc(name: &str, ip: &str, id: &str) -> ServiceEvent {
    record(CMC_SERVICE, name, ip, 8800, &[("id", id), ("process", "0")])
}

fn gone(service: &str, name: &str) -> ServiceEvent {
    ServiceEvent::ServiceRemoved(service.to_string(), format!("{name}.{service}"))
}

fn at(ip: [u8; 4], port: u16) -> SocketAddrV4 {
    SocketAddrV4::new(Ipv4Addr::from(ip), port)
}

#[test]
fn a_device_is_listed_with_its_arc_record_name_address_and_txt() {
    let mut d = Devices::default();
    let change = d.apply(&arc(
        "Stage-Card",
        "10.0.0.11",
        &[("mf", "Acme"), ("model", "X1")],
    ));
    let want = Device {
        name: "Stage-Card".to_string(),
        arc: at([10, 0, 0, 11], 4440),
        dbc: None,
        device_id: None,
        txt: [("mf", "Acme"), ("model", "X1")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    };
    assert_eq!(change, Some(Change::Added(want.clone())));
    assert_eq!(d.list(), vec![want]);
}

#[test]
fn a_dbc_record_alone_lists_nothing_and_joins_the_device_later() {
    let mut d = Devices::default();
    assert_eq!(d.apply(&dbc("card", "10.0.0.11")), None);
    assert!(d.list().is_empty());
    let Some(Change::Added(dev)) = d.apply(&arc("card", "10.0.0.11", &[])) else {
        panic!("the ARC record lists the device");
    };
    assert_eq!(dev.dbc, Some(at([10, 0, 0, 11], 4455)));
}

#[test]
fn a_changed_address_txt_or_flow_control_is_a_change_and_the_same_record_is_none() {
    let mut d = Devices::default();
    d.apply(&arc("card", "10.0.0.11", &[("model", "X1")]));
    assert_eq!(d.apply(&arc("card", "10.0.0.11", &[("model", "X1")])), None);
    let Some(Change::Changed(dev)) = d.apply(&arc("card", "10.0.0.12", &[("model", "X1")])) else {
        panic!("a new address is a change");
    };
    assert_eq!(dev.arc, at([10, 0, 0, 12], 4440));
    let Some(Change::Changed(dev)) = d.apply(&arc("card", "10.0.0.12", &[("model", "X2")])) else {
        panic!("a new TXT is a change");
    };
    assert_eq!(dev.txt.get("model").map(String::as_str), Some("X2"));
    let Some(Change::Changed(dev)) = d.apply(&dbc("CARD", "10.0.0.12")) else {
        panic!("flow control (any case of the name) is a change");
    };
    assert_eq!(dev.dbc, Some(at([10, 0, 0, 12], 4455)));
    let Some(Change::Changed(dev)) = d.apply(&gone(DBC_SERVICE, "card")) else {
        panic!("flow control withdrawn is a change");
    };
    assert_eq!(dev.dbc, None);
    assert_eq!(d.list().len(), 1);
}

#[test]
fn a_withdrawn_arc_record_removes_the_device_and_an_unknown_one_is_none() {
    let mut d = Devices::default();
    d.apply(&arc("Card", "10.0.0.11", &[]));
    d.apply(&dbc("card", "10.0.0.11"));
    assert_eq!(d.apply(&gone(ARC_SERVICE, "other")), None);
    assert_eq!(
        d.apply(&gone(ARC_SERVICE, "card")),
        Some(Change::Removed("Card".to_string()))
    );
    assert!(d.list().is_empty());
    // Its flow control record goes after: nothing listed changes, and the
    // entry is gone (a new ARC record is an addition again).
    assert_eq!(d.apply(&gone(DBC_SERVICE, "card")), None);
    assert!(d.entries.is_empty());
    assert!(matches!(
        d.apply(&arc("Card", "10.0.0.11", &[])),
        Some(Change::Added(_))
    ));
}

#[test]
fn other_services_and_records_without_ipv4_are_ignored() {
    let mut d = Devices::default();
    assert_eq!(
        d.apply(&record(
            "_netaudio-cmc._udp.local.",
            "card",
            "10.0.0.11",
            8800,
            &[]
        )),
        None
    );
    let no_ip = ServiceInfo::new(ARC_SERVICE, "card", "x.local.", "", 4440, None).unwrap();
    assert_eq!(
        d.apply(&ServiceEvent::ServiceResolved(Box::new(
            no_ip.as_resolved_service()
        ))),
        None
    );
    assert_eq!(
        d.apply(&ServiceEvent::ServiceRemoved(
            ARC_SERVICE.to_string(),
            ARC_SERVICE.to_string()
        )),
        None
    );
    assert!(d.list().is_empty());
}

#[test]
fn a_record_name_keeps_its_case_and_needs_its_service_suffix() {
    assert_eq!(
        record_of(ARC_SERVICE, "My-Card._netaudio-ARC._udp.local."),
        Some((Service::Arc, "My-Card".to_string()))
    );
    assert_eq!(
        record_of(DBC_SERVICE, "x._netaudio-dbc._udp.local."),
        Some((Service::Dbc, "x".to_string()))
    );
    assert_eq!(record_of(ARC_SERVICE, "._netaudio-arc._udp.local."), None);
    assert_eq!(
        record_of(ARC_SERVICE, "card-netaudio-arc._udp.local."),
        None
    );
    assert_eq!(
        record_of(ARC_SERVICE, "card._netaudio-dbc._udp.local."),
        None
    );
    assert_eq!(record_of(ARC_SERVICE, "short"), None);
    assert_eq!(record_of("_http._tcp.local.", "a._http._tcp.local."), None);
}

#[test]
fn the_list_is_bounded_and_txt_too() {
    let mut d = Devices::default();
    for i in 0..MAX_DEVICES {
        assert!(d.apply(&arc(&format!("d{i}"), "10.0.0.11", &[])).is_some());
    }
    assert_eq!(d.apply(&arc("one-more", "10.0.0.11", &[])), None);
    // A known device still updates at the bound.
    assert!(matches!(
        d.apply(&arc("d0", "10.0.0.12", &[])),
        Some(Change::Changed(_))
    ));
    assert_eq!(d.list().len(), MAX_DEVICES);
    let many: Vec<(String, String)> = (0..MAX_TXT + 5)
        .map(|i| (format!("k{i:02}"), "v".to_string()))
        .collect();
    let many: Vec<(&str, &str)> = many.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut d = Devices::default();
    let Some(Change::Added(dev)) = d.apply(&arc("card", "10.0.0.11", &many)) else {
        panic!("listed");
    };
    assert_eq!(dev.txt.len(), MAX_TXT);
}

// ---- live, through a fake daemon ----

struct FakeDaemon(RefCell<HashMap<String, mpsc::Receiver<ServiceEvent>>>);

impl FakeDaemon {
    fn new() -> (Self, mpsc::Sender<ServiceEvent>, mpsc::Sender<ServiceEvent>) {
        let (daemon, arc_tx, dbc_tx, _cmc_tx) = Self::with_cmc();
        (daemon, arc_tx, dbc_tx)
    }

    /// The three browses' senders (a dropped one ends its browse).
    fn with_cmc() -> (
        Self,
        mpsc::Sender<ServiceEvent>,
        mpsc::Sender<ServiceEvent>,
        mpsc::Sender<ServiceEvent>,
    ) {
        let (arc_tx, arc_rx) = mpsc::channel();
        let (dbc_tx, dbc_rx) = mpsc::channel();
        let (cmc_tx, cmc_rx) = mpsc::channel();
        let map = HashMap::from([
            (ARC_SERVICE.to_string(), arc_rx),
            (DBC_SERVICE.to_string(), dbc_rx),
            (CMC_SERVICE.to_string(), cmc_rx),
        ]);
        (FakeDaemon(RefCell::new(map)), arc_tx, dbc_tx, cmc_tx)
    }
}

impl Browse for FakeDaemon {
    type Events = mpsc::IntoIter<ServiceEvent>;
    fn browse(&self, service: &str) -> Result<Self::Events, mdns_sd::Error> {
        self.0
            .borrow_mut()
            .remove(service)
            .map(mpsc::Receiver::into_iter)
            .ok_or_else(|| mdns_sd::Error::Msg(format!("no browse of {service}")))
    }
}

#[test]
fn live_discovery_lists_devices_and_sends_each_change_until_stopped() {
    let (daemon, arc_tx, dbc_tx) = FakeDaemon::new();
    let (discovery, changes) = Discovery::start_with(&daemon).unwrap();
    arc_tx
        .send(arc("card", "10.0.0.11", &[("model", "X1")]))
        .unwrap();
    let Change::Added(dev) = changes.recv_timeout(BOUND).unwrap() else {
        panic!("an addition first");
    };
    assert_eq!(dev.name, "card");
    dbc_tx.send(dbc("card", "10.0.0.11")).unwrap();
    assert!(matches!(
        changes.recv_timeout(BOUND).unwrap(),
        Change::Changed(_)
    ));
    assert_eq!(discovery.devices().len(), 1);
    assert_eq!(discovery.devices()[0].dbc, Some(at([10, 0, 0, 11], 4455)));
    arc_tx.send(gone(ARC_SERVICE, "card")).unwrap();
    assert_eq!(
        changes.recv_timeout(BOUND).unwrap(),
        Change::Removed("card".to_string())
    );
    assert!(discovery.devices().is_empty());
    // The events end (the daemon's shutdown does that): the threads end.
    drop((arc_tx, dbc_tx));
    discovery.stop().unwrap();
}

#[test]
fn the_list_follows_with_nobody_listening_for_changes() {
    let (daemon, arc_tx, dbc_tx) = FakeDaemon::new();
    let (discovery, changes) = Discovery::start_with(&daemon).unwrap();
    drop(changes);
    arc_tx.send(arc("card", "10.0.0.11", &[])).unwrap();
    let deadline = Instant::now() + BOUND;
    while discovery.devices().is_empty() {
        assert!(Instant::now() < deadline, "the device was never listed");
        std::thread::sleep(Duration::from_millis(5));
    }
    drop((arc_tx, dbc_tx));
    discovery.stop().unwrap();
}

#[test]
fn a_browse_that_does_not_start_is_an_error() {
    let daemon = FakeDaemon(RefCell::new(HashMap::new()));
    assert!(matches!(
        Discovery::start_with(&daemon),
        Err(DiscoverError::Browse(_))
    ));
}

#[test]
fn a_stop_with_events_still_open_is_a_timeout() {
    let (daemon, arc_tx, dbc_tx) = FakeDaemon::new();
    let (discovery, _changes) = Discovery::start_with(&daemon).unwrap();
    let started = Instant::now();
    assert!(matches!(discovery.stop(), Err(DiscoverError::StopTimeout)));
    assert!(started.elapsed() >= STOP_WAIT);
    drop((arc_tx, dbc_tx));
}

#[test]
fn discovery_on_a_real_daemon_starts_and_stops() {
    let (discovery, _changes) = Discovery::start(Ipv4Addr::LOCALHOST).unwrap();
    discovery.stop().unwrap();
}

#[test]
fn flow_control_withdrawn_from_a_device_without_it_changes_nothing() {
    let mut d = Devices::default();
    d.apply(&arc("card", "10.0.0.11", &[]));
    assert_eq!(d.apply(&gone(DBC_SERVICE, "card")), None);
    assert_eq!(d.list().len(), 1);
}

#[test]
fn dropping_discovery_shuts_its_daemon_down() {
    let (discovery, _changes) = Discovery::start(Ipv4Addr::LOCALHOST).unwrap();
    let daemon = discovery.daemon().expect("its own daemon");
    assert!(daemon.get_metrics().is_ok());
    drop(discovery);
    let deadline = Instant::now() + BOUND;
    while !matches!(daemon.get_metrics(), Err(mdns_sd::Error::DaemonShutdown)) {
        assert!(Instant::now() < deadline, "the daemon still runs");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// dcontroller (1.11): a controller maps a device to its heartbeats by the
/// device id, which the CMC record's `id` gives (two of our devices share
/// an address on the site).
#[test]
fn the_cmc_record_gives_the_device_id_its_heartbeat_carries() {
    let mut d = Devices::default();
    assert_eq!(d.apply(&cmc("card", "10.0.0.11", "00000a4d07720001")), None);
    let Some(Change::Added(dev)) = d.apply(&arc("card", "10.0.0.11", &[])) else {
        panic!("listed");
    };
    assert_eq!(dev.device_id, Some([0, 0, 10, 77, 7, 114, 0, 1]));
    // A hardware id, upper case: read the same.
    let Some(Change::Changed(dev)) = d.apply(&cmc("CARD", "10.0.0.11", "001DC10821C40000")) else {
        panic!("changed");
    };
    assert_eq!(dev.device_id, Some([0, 0x1d, 0xc1, 0x08, 0x21, 0xc4, 0, 0]));
    // An id that does not read: none, the device still listed.
    let Some(Change::Changed(dev)) = d.apply(&cmc("card", "10.0.0.11", "xyz")) else {
        panic!("changed");
    };
    assert_eq!(dev.device_id, None);
    d.apply(&cmc("card", "10.0.0.11", "c8a362fffe2e958a"));
    // The CMC record withdrawn: the id goes, the device stays.
    let Some(Change::Changed(dev)) = d.apply(&gone(CMC_SERVICE, "card")) else {
        panic!("changed");
    };
    assert_eq!(dev.device_id, None);
    // Withdrawn last, a CMC record alone keeps no entry.
    d.apply(&cmc("solo", "10.0.0.12", "0000000000000001"));
    assert_eq!(d.apply(&gone(CMC_SERVICE, "solo")), None);
    assert_eq!(d.entries.len(), 1, "only card");
    // The ARC record goes while the CMC record stays: removed from the list,
    // kept until the CMC record goes too.
    d.apply(&cmc("card", "10.0.0.11", "0000000000000002"));
    assert_eq!(
        d.apply(&gone(ARC_SERVICE, "card")),
        Some(Change::Removed("card".to_string()))
    );
    assert_eq!(d.entries.len(), 1);
    d.apply(&gone(CMC_SERVICE, "card"));
    assert!(d.entries.is_empty());
}

#[test]
fn a_device_id_is_16_hex_digits() {
    assert_eq!(
        parse_device_id("0123456789abcdef"),
        Some([0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef])
    );
    for bad in [
        "",
        "0123456789abcde",
        "0123456789abcdef0",
        "0123456789abcdeg",
        "01234567é9abcde",
    ] {
        assert_eq!(parse_device_id(bad), None, "{bad:?}");
    }
}

#[test]
fn live_discovery_reads_the_cmc_record_too() {
    let (daemon, arc_tx, dbc_tx, cmc_tx) = FakeDaemon::with_cmc();
    let (discovery, changes) = Discovery::start_with(&daemon).unwrap();
    arc_tx.send(arc("card", "10.0.0.11", &[])).unwrap();
    assert!(matches!(
        changes.recv_timeout(BOUND).unwrap(),
        Change::Added(_)
    ));
    cmc_tx
        .send(cmc("card", "10.0.0.11", "00000a4d070b0000"))
        .unwrap();
    let Change::Changed(dev) = changes.recv_timeout(BOUND).unwrap() else {
        panic!("a change");
    };
    assert_eq!(dev.device_id, Some([0, 0, 10, 77, 7, 11, 0, 0]));
    drop((arc_tx, dbc_tx, cmc_tx));
    discovery.stop().unwrap();
}
