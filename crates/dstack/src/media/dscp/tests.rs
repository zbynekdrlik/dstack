use super::*;
use std::net::Ipv4Addr;

fn at(last: u8, port: u16) -> Option<SocketAddrV4> {
    Some(SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, last), port))
}

#[test]
fn only_a_slot_whose_destination_moved_changes() {
    let marked = [None, at(1, 4321), at(2, 4321), at(3, 4321)];
    let dests = [None, at(1, 4321), at(9, 4321), None];
    assert_eq!(changes(&marked, &dests), vec![(2, at(9, 4321)), (3, None)]);
    // Another port of the same receiver is another destination.
    assert_eq!(changes(&[at(1, 1)], &[at(1, 2)]), vec![(0, at(1, 2))]);
    assert_eq!(changes(&dests, &dests), vec![]);
}

#[test]
fn a_refusal_for_access_reads_as_not_admin() {
    assert_eq!(verdict(None), Verdict::Set);
    assert_eq!(verdict(Some(ERROR_ACCESS_DENIED)), Verdict::NotAdmin);
    assert_eq!(verdict(Some(87)), Verdict::Failed(87));
}

#[test]
fn only_not_found_reads_as_a_local_destination() {
    assert!(local_destination(ERROR_NOT_FOUND));
    assert!(!local_destination(ERROR_ACCESS_DENIED));
    assert!(!local_destination(0));
}

#[test]
fn the_marks_follow_the_destinations() {
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let sockets = [socket];
    let mut marks = Marks::new();
    let mut dests = [None; MAX_FLOWS];
    dests[0] = at(7, 4321);
    marks.follow(&sockets, &dests);
    assert_eq!(marks.marked(), &dests);
    // A slot without a socket is still followed (nothing to mark).
    dests[5] = at(8, 4321);
    dests[0] = None;
    marks.follow(&sockets, &dests);
    assert_eq!(marks.marked(), &dests);
}

/// On Windows (CI's runner is an administrator) a flow socket is marked
/// EF for its destination, and marked again when it moves.
#[cfg(windows)]
#[test]
fn windows_marks_a_flow_socket_ef() {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
    let mut qos = windows::Qos::new();
    let dest = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 1), 4321);
    qos.mark(0, &socket, Some(dest));
    assert_eq!(qos.last, Some(Verdict::Set));
    qos.mark(
        0,
        &socket,
        Some(SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 2), 4321)),
    );
    assert_eq!(qos.last, Some(Verdict::Set));
    qos.mark(0, &socket, None);
}
