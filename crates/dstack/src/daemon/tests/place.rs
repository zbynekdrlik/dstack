//! The place the device serves on, waited for until it is there (dstack
//! #225: a device started at logon in the same second the interface got
//! its address failed at once).

use super::*;

use std::sync::atomic::{AtomicBool, Ordering};

use crate::test_log::capture_at;

/// A scripted address source: each call answers the next list, a
/// lookup past the script panics (a wait that asks again after the place
/// appeared, or never stops, fails at once).
fn scripted(lists: Vec<Vec<IfAddr>>) -> (Rc<Cell<usize>>, impl FnMut() -> io::Result<Vec<IfAddr>>) {
    let calls = Rc::new(Cell::new(0));
    let counted = Rc::clone(&calls);
    let mut lists = lists.into_iter();
    let source = move || {
        counted.set(counted.get() + 1);
        Ok(lists.next().expect("looked up again past the script"))
    };
    (calls, source)
}

/// `absent` lookups with only the loopback, then the box's list.
fn appears_after(absent: usize) -> Vec<Vec<IfAddr>> {
    let mut lists = vec![vec![addrs()[0].clone()]; absent];
    lists.push(addrs());
    lists
}

#[test]
fn an_address_that_appears_on_the_third_lookup_is_the_device_s() {
    let (calls, source) = scripted(appears_after(2));
    let slept = RefCell::new(Vec::new());
    let got = wait_for_interface(&Place::Ip(IP), source, |d| slept.borrow_mut().push(d));
    assert_eq!(got, Ok(if_addr("eth9", IP, MASK)));
    assert_eq!(calls.get(), 3);
    assert_eq!(*slept.borrow(), vec![PLACE_RETRY; 2]);
    assert_eq!(PLACE_RETRY, Duration::from_secs(1));
}

#[test]
fn an_interface_that_gets_its_address_later_is_waited_for_too() {
    let (calls, source) = scripted(appears_after(1));
    let slept = Cell::new(0);
    let got = wait_for_interface(&Place::Iface("eth9".into()), source, |_| {
        slept.set(slept.get() + 1)
    });
    assert_eq!(got, Ok(if_addr("eth9", IP, MASK)));
    assert_eq!((calls.get(), slept.get()), (2, 1));
}

#[test]
fn a_place_already_there_is_taken_without_a_wait_or_a_line() {
    let (calls, source) = scripted(vec![addrs()]);
    let log = capture_at(tracing::Level::INFO, || {
        let got = wait_for_interface(&Place::Ip(OTHER), source, |_| panic!("no sleep"));
        assert_eq!(got, Ok(addrs()[2].clone()));
    });
    assert_eq!(calls.get(), 1);
    assert_eq!(log, "", "no line");
}

#[test]
fn a_wait_is_logged_at_its_start_once_a_minute_and_when_the_place_is_there() {
    let (_, source) = scripted(appears_after(121));
    let log = capture_at(tracing::Level::INFO, || {
        let got = wait_for_interface(&Place::Ip(IP), source, |_| ());
        assert!(got.is_ok(), "{got:?}");
    });
    let waiting: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("waiting for the device's address to appear on an interface"))
        .collect();
    assert_eq!(waiting.len(), 3, "at 0, 1 and 2 minutes:\n{log}");
    for (line, minutes) in waiting.iter().zip(0..) {
        assert!(line.contains(" WARN "), "{line}");
        assert!(line.contains(&format!("minutes={minutes}")), "{line}");
    }
    let there: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("the device's address is there now"))
        .collect();
    assert_eq!(there.len(), 1, "{log}");
    assert!(there[0].contains("seconds=121"), "{}", there[0]);
    assert_eq!(log.lines().count(), 4, "{log}");
    assert!(!log.contains("10.0.0"), "no address in {log}");
}

#[test]
fn an_interface_s_wait_names_the_interface_never_its_name() {
    let (_, source) = scripted(appears_after(1));
    let log = capture_at(tracing::Level::INFO, || {
        let got = wait_for_interface(&Place::Iface("eth9".into()), source, |_| ());
        assert!(got.is_ok(), "{got:?}");
    });
    assert!(
        log.contains("waiting for the device's interface to appear with an IPv4 address"),
        "{log}"
    );
    assert!(log.contains("the device's interface is there now"), "{log}");
    assert!(!log.contains("eth9"), "no name in {log}");
}

#[test]
fn any_other_refusal_fails_at_once() {
    // A loopback address: before any lookup.
    let (calls, source) = scripted(vec![]);
    let e = wait_for_interface(&Place::Ip(Ipv4Addr::LOCALHOST), source, |_| {
        panic!("no sleep")
    })
    .unwrap_err();
    assert!(e.contains("loopback"), "{e}");
    assert_eq!(calls.get(), 0);
    // A loopback address no interface holds: refused, not waited for.
    let (_, source) = scripted(vec![vec![]]);
    let e = wait_for_interface(&Place::Ip(Ipv4Addr::new(127, 0, 0, 2)), source, |_| {
        panic!("no sleep")
    })
    .unwrap_err();
    assert!(e.contains("loopback"), "{e}");
    // An interface with two addresses, and one whose address is the
    // loopback's.
    for (name, why) in [("br9", "2 IPv4 addresses"), ("lo", "loopback")] {
        let (calls, source) = scripted(vec![addrs()]);
        let e = wait_for_interface(&Place::Iface(name.into()), source, |_| panic!("no sleep"))
            .unwrap_err();
        assert!(e.contains(why), "{e}");
        assert_eq!(calls.get(), 1);
    }
    // The box's interfaces that cannot be read.
    let e = wait_for_interface(
        &Place::Ip(IP),
        || Err(io::Error::other("no list")),
        |_| panic!("no sleep"),
    )
    .unwrap_err();
    assert_eq!(e, "the box's interfaces: no list");
}

// ---- dstack #230: a start that fails at once, a wait the app stops ----

#[test]
fn a_start_now_fails_on_a_place_not_there_after_one_look() {
    let (calls, source) = scripted(appears_after(1));
    let e = resolve_place(&Place::Ip(IP), PlaceWait::Now, source, |_| {
        panic!("no sleep")
    })
    .unwrap_err();
    assert_eq!(e, "no interface of this box holds the address");
    assert_eq!(calls.get(), 1);
    let (_, source) = scripted(vec![vec![]]);
    let e = resolve_place(&Place::Iface("eth9".into()), PlaceWait::Now, source, |_| {
        panic!("no sleep")
    })
    .unwrap_err();
    assert_eq!(e, "no such interface, or it has no IPv4 address");
    // A place there is taken; a list that cannot be read is an error.
    let (_, source) = scripted(vec![addrs()]);
    let got = resolve_place(&Place::Ip(IP), PlaceWait::Now, source, |_| {
        panic!("no sleep")
    });
    assert_eq!(got, Ok(if_addr("eth9", IP, MASK)));
    let e = resolve_place(
        &Place::Ip(IP),
        PlaceWait::Now,
        || Err(io::Error::other("no list")),
        |_| panic!("no sleep"),
    )
    .unwrap_err();
    assert_eq!(e, "the box's interfaces: no list");
}

#[test]
fn a_start_forever_waits_as_wait_for_interface() {
    let (calls, source) = scripted(appears_after(2));
    let slept = Cell::new(0);
    let got = resolve_place(&Place::Ip(IP), PlaceWait::Forever, source, |_| {
        slept.set(slept.get() + 1)
    });
    assert_eq!(got, Ok(if_addr("eth9", IP, MASK)));
    assert_eq!((calls.get(), slept.get()), (3, 2));
}

#[test]
fn a_stop_set_while_waiting_ends_the_wait_after_the_retry_it_was_set_in() {
    let stop = AtomicBool::new(false);
    let (calls, source) = scripted(vec![vec![addrs()[0].clone()]; 3]);
    let slept = Cell::new(0);
    let log = capture_at(tracing::Level::INFO, || {
        let e = resolve_place(&Place::Ip(IP), PlaceWait::Until(&stop), source, |d| {
            assert_eq!(d, PLACE_RETRY);
            slept.set(slept.get() + 1);
            // The app stops the start during the second retry interval.
            if slept.get() == 2 {
                stop.store(true, Ordering::Relaxed);
            }
        })
        .unwrap_err();
        assert_eq!(e, "stopped while waiting for the device's address");
    });
    // One look after the interval the stop came in, then no more.
    assert_eq!((calls.get(), slept.get()), (3, 2));
    assert!(
        log.contains("(looked up every second, until the app stops the start)"),
        "{log}"
    );
    let stopped: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("stopped while waiting for the device's address"))
        .collect();
    assert_eq!(stopped.len(), 1, "{log}");
    assert!(stopped[0].contains(" INFO "), "{}", stopped[0]);
    assert!(stopped[0].contains("seconds=2"), "{}", stopped[0]);
    assert!(!log.contains("10.0.0"), "no address in {log}");
}

#[test]
fn a_stop_set_before_the_start_ends_it_after_one_look_and_a_place_there_still_starts() {
    let stop = AtomicBool::new(true);
    let (calls, source) = scripted(vec![vec![]]);
    let e = resolve_place(
        &Place::Iface("eth9".into()),
        PlaceWait::Until(&stop),
        source,
        |_| panic!("no sleep"),
    )
    .unwrap_err();
    assert_eq!(e, "stopped while waiting for the device's interface");
    assert_eq!(calls.get(), 1);
    // The stop ends a wait only: a place there is the device's.
    let (_, source) = scripted(vec![addrs()]);
    let got = resolve_place(&Place::Ip(IP), PlaceWait::Until(&stop), source, |_| {
        panic!("no sleep")
    });
    assert_eq!(got, Ok(if_addr("eth9", IP, MASK)));
    // A refusal fails at once, the stop not set.
    let idle = AtomicBool::new(false);
    let (_, source) = scripted(vec![]);
    let e = resolve_place(
        &Place::Ip(Ipv4Addr::LOCALHOST),
        PlaceWait::Until(&idle),
        source,
        |_| panic!("no sleep"),
    )
    .unwrap_err();
    assert!(e.contains("loopback"), "{e}");
}

// ---- dstack #230: the default route's address, waited for ----

#[test]
fn the_default_route_s_address_is_waited_for_with_the_same_lines() {
    let mut answers = vec![Ok(None), Ok(None), Ok(Some(IP))].into_iter();
    let looked = Cell::new(0);
    let slept = RefCell::new(Vec::new());
    let log = capture_at(tracing::Level::INFO, || {
        let got = wait_for_route(
            || {
                looked.set(looked.get() + 1);
                answers.next().expect("looked up again after it came")
            },
            |d| slept.borrow_mut().push(d),
        );
        assert_eq!(got, Ok(IP));
    });
    assert_eq!(looked.get(), 3);
    assert_eq!(*slept.borrow(), vec![PLACE_RETRY; 2]);
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2, "{log}");
    assert!(lines[0].contains(" WARN "), "{log}");
    assert!(
        lines[0].contains(
            "waiting for the default route's address to appear \
             (looked up every second, no time limit)"
        ),
        "{log}"
    );
    assert!(lines[0].contains("minutes=0"), "{log}");
    assert!(lines[1].contains(" INFO "), "{log}");
    assert!(
        lines[1].contains("the default route's address is there now"),
        "{log}"
    );
    assert!(lines[1].contains("seconds=2"), "{log}");
    assert!(!log.contains("10.0.0"), "no address in {log}");
}

#[test]
fn a_default_route_there_is_taken_and_an_error_ends_the_wait() {
    let got = wait_for_route(|| Ok(Some(OTHER)), |_| panic!("no sleep"));
    assert_eq!(got, Ok(OTHER));
    let e = wait_for_route(|| Err("refused".to_string()), |_| panic!("no sleep"));
    assert_eq!(e, Err("refused".to_string()));
}

#[test]
fn a_route_answer_is_the_address_not_yet_or_an_error() {
    let v4 = |ip: Ipv4Addr| Ok(SocketAddr::V4(SocketAddrV4::new(ip, 40_000)));
    assert_eq!(route_answer(v4(IP)), Ok(Some(IP)));
    // An address not given yet, and no route yet: not there.
    assert_eq!(route_answer(v4(Ipv4Addr::UNSPECIFIED)), Ok(None));
    for kind in [
        io::ErrorKind::NetworkUnreachable,
        io::ErrorKind::HostUnreachable,
        io::ErrorKind::AddrNotAvailable,
        // An adapter still coming up at logon (WSAENETDOWN, ENETDOWN).
        io::ErrorKind::NetworkDown,
    ] {
        assert_eq!(
            route_answer(Err(io::Error::from(kind))),
            Ok(None),
            "{kind:?}"
        );
    }
    // Any other error, and an IPv6 address, fail at once.
    let e = route_answer(Err(io::Error::other("no socket"))).unwrap_err();
    assert_eq!(e, "the default route: no socket");
    let v6 = SocketAddr::new(std::net::Ipv6Addr::LOCALHOST.into(), 9);
    let e = route_answer(Ok(v6)).unwrap_err();
    assert!(e.starts_with("no IPv4 address on the default route"), "{e}");
}
