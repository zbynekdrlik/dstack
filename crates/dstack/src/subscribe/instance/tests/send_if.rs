//! The queries' multicast interface across an interface re-create (dstack
//! #409): Linux keeps the interface INDEX `IP_MULTICAST_IF` named, so after
//! the NIC is re-created under a new index every query failed (ENODEV) and
//! the directory never found its transmitter again. A root-free test cannot
//! re-create an interface; it moves a fake list's index and reads the
//! option back from the thread's socket, set to name nothing in between.

use super::*;

use crate::membership::fake::{Fake, until};

/// A multicast group nobody serves stands in for mDNS's: with no lookup
/// the thread sends no query.
const GROUP: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(239, 255, 77, 91), 5353);

/// The thread's socket's multicast send interface.
fn send_if(i: &Instances) -> Ipv4Addr {
    socket2::SockRef::from(i.socket())
        .multicast_if_v4()
        .unwrap()
}

/// The option as a re-created interface leaves it: naming nothing.
fn spoil(i: &Instances) {
    socket2::SockRef::from(i.socket())
        .set_multicast_if_v4(&Ipv4Addr::UNSPECIFIED)
        .unwrap();
}

#[test]
fn after_an_interface_re_create_the_queries_multicast_interface_is_set_again() {
    let fake = Fake::at(4);
    let mut i = Instances::new();
    let lister = fake.lister(Ipv4Addr::LOCALHOST, Duration::from_millis(1));
    assert!(i.start_watched(Ipv4Addr::LOCALHOST, GROUP, lister).unwrap());
    assert_eq!(send_if(&i), Ipv4Addr::LOCALHOST, "set at the start");
    // The first look learns the index.
    fake.read_more(1);
    spoil(&i);
    fake.read_more(2);
    assert_eq!(
        send_if(&i),
        Ipv4Addr::UNSPECIFIED,
        "nothing while the index holds"
    );
    fake.recreate(9);
    until("the interface set again", || {
        send_if(&i) == Ipv4Addr::LOCALHOST
    });
    // Once per change.
    spoil(&i);
    fake.read_more(2);
    assert_eq!(send_if(&i), Ipv4Addr::UNSPECIFIED, "set once per change");
}

#[test]
fn a_thread_that_asks_a_unicast_responder_reads_no_interface_list() {
    let responder = UdpSocket::bind("127.0.0.1:0").unwrap();
    responder
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let std::net::SocketAddr::V4(to) = responder.local_addr().unwrap() else {
        panic!("IPv4")
    };
    let fake = Fake::at(4);
    let mut i = Instances::new();
    i.lookup(DEVICE, CHANNEL);
    let lister = fake.lister(Ipv4Addr::LOCALHOST, Duration::from_millis(1));
    assert!(i.start_watched(Ipv4Addr::LOCALHOST, to, lister).unwrap());
    // The thread's first round is past its watch once its query is out.
    let mut buf = [0u8; 1500];
    responder.recv_from(&mut buf).expect("a query");
    assert_eq!(fake.reads(), 0, "no multicast interface to keep");
    assert_eq!(i.send_errors(), 0);
}

#[test]
fn failed_query_sends_are_counted_and_name_the_interface_again() {
    let mut i = Instances::new();
    // Several names, so several queries are due at once.
    for ch in ["a", "b", "c", "d"] {
        i.lookup(DEVICE, ch);
    }
    // A send to the broadcast address without SO_BROADCAST is refused
    // every time (EACCES; WSAEACCES on Windows), and it is no group: no
    // interface watch, and the option is not set at the start.
    let to = SocketAddrV4::new(Ipv4Addr::BROADCAST, 9);
    let fake = Fake::at(4);
    let lister = fake.lister(Ipv4Addr::LOCALHOST, Duration::from_millis(1));
    assert!(i.start_watched(Ipv4Addr::LOCALHOST, to, lister).unwrap());
    assert_eq!(send_if(&i), Ipv4Addr::UNSPECIFIED, "not named at the start");
    let at = u64::from(crate::info::sendwatch::REAPPLY_AT);
    until("failed sends counted", || i.send_errors() >= at);
    until("the interface named after failed sends", || {
        send_if(&i) == Ipv4Addr::LOCALHOST
    });
    assert_eq!(fake.reads(), 0, "named by the send watch, not the list's");
}

#[test]
fn failed_query_sends_are_reported_with_their_count_and_error() {
    let r = Report {
        failures: 7,
        error: "No such device (os error 19)".to_string(),
    };
    let text = crate::test_log::capture_warnings(|| report(&r));
    assert!(
        text.contains("mDNS queries could not be sent (counted; one line a minute at most)"),
        "{text}"
    );
    assert!(text.contains("failures=7"), "{text}");
    assert!(text.contains("No such device (os error 19)"), "{text}");
}
