//! The place the device serves on, waited for until it is there (dstack
//! #225: a device started at logon in the same second the interface got
//! its address failed at once).

use super::*;

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
