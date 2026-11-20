use super::*;

const IP: Ipv4Addr = Ipv4Addr::new(10, 70, 0, 21);
const TX: Ipv4Addr = Ipv4Addr::new(10, 70, 0, 9);

fn flow(group: Option<SocketAddrV4>) -> RxPlan {
    RxPlan {
        src: Some(TX),
        bps: 3,
        nslots: 2,
        latency: 96,
        delay: 0,
        block: 16,
        live: true,
        generation: 1,
        group,
    }
}

fn group(last: u8, port: u16) -> SocketAddrV4 {
    SocketAddrV4::new(Ipv4Addr::new(239, 255, 9, last), port)
}

#[test]
fn a_unicast_flow_arrives_on_our_address_and_its_slot_s_port() {
    let slots = [
        (RxPlan::default(), 29800),
        (flow(None), 29801),
        (RxPlan::default(), 29802),
    ];
    assert_eq!(destinations(IP, slots), [SocketAddrV4::new(IP, 29801)]);
}

#[test]
fn a_multicast_flow_arrives_on_its_group_and_port_never_the_slot_s() {
    let slots = [(flow(Some(group(3, 4321))), 29800)];
    assert_eq!(destinations(IP, slots), [group(3, 4321)]);
}

#[test]
fn mixed_flows_give_every_pair_sorted_by_address_then_port() {
    let slots = [
        (flow(Some(group(7, 4321))), 29800),
        (flow(None), 29805),
        (flow(Some(group(2, 5000))), 29801),
        (flow(None), 29802),
        (flow(Some(group(2, 4000))), 29803),
    ];
    assert_eq!(
        destinations(IP, slots),
        [
            SocketAddrV4::new(IP, 29802),
            SocketAddrV4::new(IP, 29805),
            group(2, 4000),
            group(2, 5000),
            group(7, 4321),
        ]
    );
}

#[test]
fn a_slot_without_a_flow_gives_nothing_even_with_a_group_or_a_port() {
    let stale = RxPlan {
        src: None,
        group: Some(group(1, 4321)),
        ..flow(None)
    };
    assert_eq!(destinations(IP, [(stale, 29800)]), []);
    assert_eq!(destinations(IP, [(RxPlan::default(), 29801)]), []);
}

#[test]
fn a_unicast_flow_in_a_slot_with_no_socket_gives_nothing() {
    assert_eq!(destinations(IP, [(flow(None), 0)]), []);
    // A multicast flow needs no slot port: its group's port is the pair's.
    assert_eq!(
        destinations(IP, [(flow(Some(group(4, 4321))), 0)]),
        [group(4, 4321)]
    );
}

#[test]
fn one_group_in_two_slots_is_one_pair() {
    let slots = [
        (flow(Some(group(5, 4321))), 29800),
        (flow(Some(group(5, 4321))), 29801),
    ];
    assert_eq!(destinations(IP, slots), [group(5, 4321)]);
}

#[test]
fn every_one_of_32_slots_is_given_past_a_16_pair_filter() {
    let slots = (0..super::super::MAX_FLOWS as u16).map(|n| (flow(None), 29800 + n));
    let pairs = destinations(IP, slots);
    assert_eq!(pairs.len(), 32);
    assert_eq!(pairs[31], SocketAddrV4::new(IP, 29831));
}

#[test]
fn the_generation_moves_only_when_the_set_changes() {
    let mut d = RxDestinations::default();
    assert_eq!(d.generation, 0);
    assert!(d.pairs.is_empty());
    assert!(!d.update(Vec::new()), "the empty set again is no change");
    assert_eq!(d.generation, 0);
    let one = vec![SocketAddrV4::new(IP, 29800)];
    assert!(d.update(one.clone()));
    assert_eq!((d.generation, d.pairs.as_slice()), (1, one.as_slice()));
    assert!(!d.update(one.clone()));
    assert_eq!(d.generation, 1);
    let two = vec![SocketAddrV4::new(IP, 29800), group(1, 4321)];
    assert!(d.update(two.clone()));
    assert_eq!((d.generation, d.pairs.as_slice()), (2, two.as_slice()));
    assert!(d.update(Vec::new()), "every flow gone is a change");
    assert_eq!((d.generation, d.pairs.len()), (3, 0));
}
