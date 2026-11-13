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

fn policy(name: &str, values: &[(&str, PolicyValue)]) -> Policy {
    Policy {
        name: name.into(),
        values: values
            .iter()
            .map(|(k, v)| ((*k).into(), v.clone()))
            .collect(),
    }
}

fn text(s: &str) -> PolicyValue {
    PolicyValue::Text(s.into())
}

const EXE: &str = r"C:\Program Files\dasio\dsend.exe";

/// `New-NetQosPolicy`'s form (Version 2.0: DWORDs, protocol 2 = UDP).
fn netqos(name: &str, app: &str, dscp: u32, protocol: Option<u32>) -> Policy {
    let mut p = policy(
        name,
        &[("AppName", text(app)), ("DSCP", PolicyValue::Number(dscp))],
    );
    if let Some(n) = protocol {
        p.values.push(("Protocol".into(), PolicyValue::Number(n)));
    }
    p
}

/// The group policy editor's form (Version 1.0: all text).
fn gpedit(name: &str, app: &str, dscp: &str, protocol: &str) -> Policy {
    policy(
        name,
        &[
            ("Application Name", text(app)),
            ("DSCP Value", text(dscp)),
            ("Protocol", text(protocol)),
        ],
    )
}

#[test]
fn a_policy_for_this_exe_with_dscp_46_marks_the_audio() {
    // UDP (2), both (3), none (0) and no protocol value all cover UDP.
    for protocol in [Some(2), Some(3), Some(0), None] {
        let p = [netqos("dasio-dsend", "dsend.exe", 46, protocol)];
        assert_eq!(marking_policy(&p, EXE), Some("dasio-dsend"), "{protocol:?}");
    }
    for protocol in ["*", "UDP", "udp"] {
        let p = [gpedit("audio", "DSEND.EXE", "46", protocol)];
        assert_eq!(marking_policy(&p, EXE), Some("audio"), "{protocol}");
    }
    // A full path names this exe too, in any case.
    let p = [netqos(
        "path",
        r"c:\program files\dasio\DSEND.exe",
        46,
        Some(2),
    )];
    assert_eq!(marking_policy(&p, EXE), Some("path"));
    // The first matching policy is named.
    let p = [
        netqos("other", "dlisten.exe", 46, None),
        netqos("first", "dsend.exe", 46, None),
        netqos("second", "dsend.exe", 46, None),
    ];
    assert_eq!(marking_policy(&p, EXE), Some("first"));
}

#[test]
fn another_program_dscp_or_protocol_still_warns() {
    let none: Option<&str> = None;
    // Another program, by name or by path.
    assert_eq!(
        marking_policy(&[netqos("x", "dlisten.exe", 46, Some(2))], EXE),
        none
    );
    assert_eq!(
        marking_policy(&[netqos("x", r"D:\other\dsend.exe", 46, Some(2))], EXE),
        none
    );
    assert_eq!(
        marking_policy(&[netqos("x", "send.exe", 46, Some(2))], EXE),
        none
    );
    // Another DSCP.
    assert_eq!(
        marking_policy(&[netqos("x", "dsend.exe", 34, Some(2))], EXE),
        none
    );
    assert_eq!(
        marking_policy(&[gpedit("x", "dsend.exe", "0", "UDP")], EXE),
        none
    );
    assert_eq!(
        marking_policy(&[gpedit("x", "dsend.exe", "", "UDP")], EXE),
        none
    );
    // TCP only, or a protocol we do not know.
    assert_eq!(
        marking_policy(&[netqos("x", "dsend.exe", 46, Some(1))], EXE),
        none
    );
    assert_eq!(
        marking_policy(&[netqos("x", "dsend.exe", 46, Some(4))], EXE),
        none
    );
    assert_eq!(
        marking_policy(&[gpedit("x", "dsend.exe", "46", "TCP")], EXE),
        none
    );
    // No application (a policy by port only), or no DSCP.
    assert_eq!(
        marking_policy(&[policy("x", &[("DSCP", PolicyValue::Number(46))])], EXE),
        none
    );
    assert_eq!(
        marking_policy(&[policy("x", &[("AppName", text("dsend.exe"))])], EXE),
        none
    );
    assert_eq!(marking_policy(&[], EXE), none);
}

#[test]
fn a_marking_policy_turns_the_warning_into_an_info_line() {
    assert_eq!(
        unmarked_note(Some("dasio-dsend")).as_deref(),
        Some("TX audio marked DSCP 46 by QoS policy 'dasio-dsend'")
    );
    assert_eq!(unmarked_note(None), None);
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
