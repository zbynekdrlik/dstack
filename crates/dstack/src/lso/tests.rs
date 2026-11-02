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
