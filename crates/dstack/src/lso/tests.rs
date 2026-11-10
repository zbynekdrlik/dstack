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

/// dasio#1: the two Realtek versions measured with the long receive DPC
/// warn, the measured-good one passes, any other Realtek one is only
/// unverified, and another vendor's is no Realtek verdict at all.
#[test]
fn the_driver_verdict_names_only_measured_realtek_versions() {
    let rt = Some("Realtek");
    assert_eq!(driver(rt, Some("10.79.20.1003")), Driver::KnownBad);
    assert_eq!(driver(rt, Some("10.72.524.2024")), Driver::KnownBad);
    assert_eq!(
        driver(Some("Realtek Semiconductor Corp."), Some(" 10.79.20.1003 ")),
        Driver::KnownBad
    );
    assert_eq!(driver(rt, Some("10.39.212.2020")), Driver::KnownGood);
    // Never measured: neither good nor bad (dasio: two points, no range).
    for v in [
        "10.56.119.2022",
        "10.65.421.2023",
        "10.68.815.2023",
        "10.80.1.1",
    ] {
        assert_eq!(driver(rt, Some(v)), Driver::Unverified, "{v}");
    }
    assert_eq!(driver(rt, None), Driver::Other);
    assert_eq!(driver(Some("Intel"), Some("10.79.20.1003")), Driver::Other);
    assert_eq!(driver(None, Some("10.79.20.1003")), Driver::Other);
}

/// The runner's adapter: its provider and driver version are read from
/// the class key (a Windows runner has no Realtek NIC).
#[cfg(windows)]
#[test]
fn the_routed_adapter_s_driver_is_read() {
    let probe = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    probe.connect("192.0.2.1:9").unwrap();
    let std::net::SocketAddr::V4(local) = probe.local_addr().unwrap() else {
        panic!("no routed IPv4 address");
    };
    let (_, _, guid) = crate::host::win_nic::of(*local.ip()).unwrap();
    let v = sys::adapter_values(&guid, &["ProviderName", "DriverVersion", "DriverDesc"]).unwrap();
    assert!(v[0].is_some() && v[1].is_some(), "{v:?}");
    let (verdict, version) = check_driver(&guid);
    assert_eq!(version, v[1]);
    let expected = if virtual_adapter(v[2].as_deref()) {
        Driver::Virtual
    } else {
        driver(v[0].as_deref(), v[1].as_deref())
    };
    assert_eq!(verdict, expected);
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
