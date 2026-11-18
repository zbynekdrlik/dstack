//! The sweeps' multicast interface across an interface re-create (dstack
//! #409, as the directory's queries): the sweeper's `IP_MULTICAST_IF` named
//! an interface index that a re-created NIC no longer has, and no sweep
//! left again. The test moves a fake list's index and reads the option back.

use super::*;

use crate::membership::fake::{Fake, until};

/// A multicast group nobody serves stands in for mDNS's.
const GROUP: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(239, 255, 77, 92), 5353);

fn send_if(d: &Discovery) -> Ipv4Addr {
    socket2::SockRef::from(d.sweeper())
        .multicast_if_v4()
        .unwrap()
}

/// The option as a re-created interface leaves it: naming nothing.
fn spoil(d: &Discovery) {
    socket2::SockRef::from(d.sweeper())
        .set_multicast_if_v4(&Ipv4Addr::UNSPECIFIED)
        .unwrap();
}

#[test]
fn after_an_interface_re_create_the_sweeps_multicast_interface_is_set_again() {
    let fake = Fake::at(4);
    let lister = fake.lister(LOOPBACK, Duration::from_millis(1));
    let (discovery, _changes) = Discovery::start_watched(LOOPBACK, GROUP, None, lister).unwrap();
    assert_eq!(send_if(&discovery), LOOPBACK, "set at the start");
    fake.read_more(1);
    spoil(&discovery);
    fake.read_more(2);
    assert_eq!(
        send_if(&discovery),
        Ipv4Addr::UNSPECIFIED,
        "nothing while the index holds"
    );
    fake.recreate(9);
    until("the interface set again", || send_if(&discovery) == LOOPBACK);
    discovery.stop().unwrap();
}

#[test]
fn discovery_sweeping_a_unicast_responder_with_no_listener_reads_no_interface_list() {
    let (mdns, asked) = responder(Vec::new());
    let fake = Fake::at(4);
    let lister = fake.lister(LOOPBACK, Duration::from_millis(1));
    let (discovery, _changes) = Discovery::start_watched(LOOPBACK, mdns, None, lister).unwrap();
    // The first round's watch is past once its sweep is out.
    eventually("a sweep", || !asked.lock().unwrap().is_empty());
    assert_eq!(fake.reads(), 0, "nothing to keep");
    discovery.stop().unwrap();
}
