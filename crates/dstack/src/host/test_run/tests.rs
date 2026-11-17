// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

const SITE: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 10);

fn run(loopback: bool, live: bool) -> TestRun {
    TestRun { loopback, live }
}

fn found(name: &str, ip: Ipv4Addr) -> IfAddr {
    IfAddr {
        name: name.into(),
        ip,
        netmask: Ipv4Addr::new(255, 255, 255, 0),
    }
}

#[test]
fn each_flag_is_set_by_its_own_variable_at_exactly_1() {
    let with = |pairs: &'static [(&'static str, &'static str)]| {
        TestRun::from_vars(move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| OsString::from(v))
        })
    };
    assert_eq!(with(&[]), run(false, false));
    assert_eq!(with(&[(LOOPBACK_ENV, "1")]), run(true, false));
    assert_eq!(with(&[(LIVE_ENV, "1")]), run(false, true));
    assert_eq!(
        with(&[(LOOPBACK_ENV, "1"), (LIVE_ENV, "1")]),
        run(true, true)
    );
    // Only "1" sets a flag.
    for other in ["0", "", "true", "11", " 1"] {
        let got = TestRun::from_vars(|_| Some(OsString::from(other)));
        assert_eq!(got, run(false, false), "{other:?}");
    }
}

#[test]
fn a_test_run_serves_a_loopback_address_as_it_is() {
    for live in [false, true] {
        for ip in [Ipv4Addr::LOCALHOST, Ipv4Addr::new(127, 36, 6, 1)] {
            assert_eq!(
                run(true, live).loopback(&Place::Ip(ip)),
                Some(IfAddr {
                    name: "lo".into(),
                    ip,
                    netmask: Ipv4Addr::new(255, 0, 0, 0),
                })
            );
        }
    }
}

#[test]
fn every_other_place_and_every_place_outside_a_test_run_takes_the_lookup() {
    for live in [false, true] {
        for place in [Place::Ip(SITE), Place::Iface("lo".into())] {
            assert_eq!(run(true, live).loopback(&place), None, "{place:?}");
        }
        for place in [
            Place::Ip(Ipv4Addr::LOCALHOST),
            Place::Ip(SITE),
            Place::Iface("eth0".into()),
        ] {
            assert_eq!(run(false, live).loopback(&place), None, "{place:?}");
        }
    }
}

#[test]
fn a_test_run_refuses_a_found_site_address_unless_live() {
    let site = found("eth0", SITE);
    assert_eq!(
        run(true, false).admit(site.clone()),
        Err(SITE_REFUSED.to_string())
    );
    assert_eq!(run(true, true).admit(site.clone()), Ok(site.clone()));
    // Outside a test run, live or not, the lookup's answer stands.
    assert_eq!(run(false, false).admit(site.clone()), Ok(site.clone()));
    assert_eq!(run(false, true).admit(site.clone()), Ok(site));
    // A loopback answer (never from the production lookup) is admitted.
    let lo = found("lo", Ipv4Addr::LOCALHOST);
    assert_eq!(run(true, false).admit(lo.clone()), Ok(lo));
    assert!(SITE_REFUSED.contains("DSTACK_LIVE_TEST=1"));
    assert!(SITE_REFUSED.contains("127.0.0.1"));
}

#[test]
fn a_loopback_device_has_a_locally_administered_mac_holding_its_address() {
    let ifa = IfAddr {
        name: "lo".into(),
        ip: Ipv4Addr::new(127, 36, 6, 1),
        netmask: Ipv4Addr::new(255, 0, 0, 0),
    };
    let net = loopback_net(&ifa);
    assert_eq!(net.mac, [0x02, 0x00, 127, 36, 6, 1]);
    // Locally administered (bit 1), unicast (bit 0 clear): a valid MAC.
    assert_eq!(net.mac[0] & 0x03, 0x02);
    assert!(crate::daemon::parse_mac("02:00:7f:24:06:01").is_some());
    assert_eq!(net.ip, ifa.ip);
    assert_eq!(net.netmask, ifa.netmask);
    assert_eq!(net.gateway, Ipv4Addr::UNSPECIFIED);
    assert_eq!(net.link_speed, 0);
}
