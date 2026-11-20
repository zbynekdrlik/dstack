use super::*;

#[test]
fn lso_is_on_when_either_ipv4_version_is_on_and_unknown_without_either() {
    assert_eq!(verdict(Some("1"), Some("0")), Verdict::On);
    assert_eq!(verdict(Some("0"), Some("1")), Verdict::On);
    assert_eq!(verdict(None, Some("1")), Verdict::On);
    assert_eq!(verdict(Some("1"), None), Verdict::On);
    assert_eq!(verdict(Some("0"), Some("0")), Verdict::Off);
    assert_eq!(verdict(Some("0"), None), Verdict::Off);
    assert_eq!(verdict(None, Some(" 0 ")), Verdict::Off);
    assert_eq!(verdict(None, None), Verdict::Unknown);
}

/// The runner's routed adapter is found under the network class key by its
/// GUID: the registry path and the name match work on a real Windows.
#[cfg(windows)]
#[test]
fn the_routed_adapter_s_settings_are_found_by_its_guid() {
    let probe = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    probe.connect("192.0.2.1:9").unwrap();
    let std::net::SocketAddr::V4(local) = probe.local_addr().unwrap() else {
        panic!("no routed IPv4 address");
    };
    let (_, _, guid) = crate::host::win_nic::of(*local.ip()).unwrap();
    assert!(guid.starts_with('{') && guid.ends_with('}'), "{guid}");
    assert!(
        sys::lso_values(&guid).is_some(),
        "no class subkey names {guid}"
    );
}

/// The runner's adapter: its provider, hardware id, version and driver
/// file are read from the class key and its service (a Windows runner has
/// no Realtek NIC: a Hyper-V adapter, `netvsc.sys`).
#[cfg(windows)]
#[test]
fn the_routed_adapter_s_driver_is_read() {
    let probe = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    probe.connect("192.0.2.1:9").unwrap();
    let std::net::SocketAddr::V4(local) = probe.local_addr().unwrap() else {
        panic!("no routed IPv4 address");
    };
    let (_, _, guid) = crate::host::win_nic::of(*local.ip()).unwrap();
    let facts = driver_facts(&guid).expect("the adapter's class subkey");
    assert!(
        facts.provider.is_some()
            && facts.version.is_some()
            && facts.hardware_id.as_deref().is_some_and(|h| !h.is_empty()),
        "{facts:?}"
    );
    let file = facts.file.clone().unwrap_or_default();
    assert!(
        file.to_ascii_lowercase().ends_with(".sys") && !file.contains('\\'),
        "{facts:?}"
    );
    let v = sys::adapter_values(&guid, &["DriverVersion"]).unwrap();
    assert_eq!(facts.version, v[0]);
}

/// dstack #155: the Dante leg on resolume-snv is Realtek's VLAN "Realtek
/// Virtual Adapter" (driver 10.4.1117.2020), which the measured-driver
/// check read as an unverified NIC driver.
#[test]
fn a_vlan_virtual_adapter_is_told_apart_from_a_nic() {
    assert!(virtual_adapter(Some("Realtek Virtual Adapter")));
    assert!(virtual_adapter(Some("Realtek Virtual Adapter #2")));
    assert!(!virtual_adapter(Some(
        "Realtek Gaming USB 2.5GbE Family Controller"
    )));
    assert!(!virtual_adapter(Some(
        "Intel(R) Ethernet Controller (2) I225-V"
    )));
    assert!(!virtual_adapter(None));
}

mod verdict;
