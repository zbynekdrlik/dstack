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

const EXE: &str = r"C:\Apps\tx\dsend.exe";

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
        let p = [netqos("audio-tx", "dsend.exe", 46, protocol)];
        assert_eq!(marking_policy(&p, EXE), Some("audio-tx"), "{protocol:?}");
    }
    for protocol in ["*", "UDP", "udp"] {
        let p = [gpedit("audio", "DSEND.EXE", "46", protocol)];
        assert_eq!(marking_policy(&p, EXE), Some("audio"), "{protocol}");
    }
    // A full path names this exe too, in any case.
    let p = [netqos("path", r"c:\apps\TX\DSEND.exe", 46, Some(2))];
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
fn a_condition_that_narrows_the_policy_still_warns() {
    let with = |k: &str, v: PolicyValue| {
        let mut p = netqos("p", "dsend.exe", 46, Some(2));
        p.values.push((k.into(), v));
        [p]
    };
    let n = PolicyValue::Number;
    // A port, an address, a profile not all of them, or one we do not know.
    for (k, v) in [
        ("DstPortLow", n(14336)),
        ("RemotePort", text("5004")),
        ("Remote IP", text("10.0.0.0")),
        ("NetProfile", n(2)),
        ("Something New", n(0)),
    ] {
        assert_eq!(marking_policy(&with(k, v.clone()), EXE), None, "{k}");
    }
    // Neutral: the form, the order, the throttle, all profiles, a `*`.
    for (k, v) in [
        ("Version", text("2.0")),
        ("Precedence", n(127)),
        ("Throttle Rate", text("-1")),
        ("ThrottleRate", n(0)),
        ("NetProfile", n(7)),
        ("Local Port", text("*")),
        ("Remote IP", text(" ")),
        ("version", text("1.0")),
    ] {
        assert_eq!(marking_policy(&with(k, v.clone()), EXE), Some("p"), "{k}");
    }
    // The group policy editor's full form, every unused condition `*`.
    let mut p = gpedit("gp", "dsend.exe", "46", "UDP");
    for k in [
        "Local Port",
        "Local IP",
        "Local IP Prefix Length",
        "Remote Port",
        "Remote IP",
        "Remote IP Prefix Length",
    ] {
        p.values.push((k.into(), text("*")));
    }
    assert_eq!(marking_policy(&[p], EXE), Some("gp"));
}

#[test]
fn a_marking_policy_turns_the_warning_into_an_info_line() {
    assert_eq!(
        unmarked_note(Some("audio-tx")).as_deref(),
        Some("TX audio marked DSCP 46 by QoS policy 'audio-tx'")
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

/// On Windows the policies are read from the registry: both value types,
/// as `New-NetQosPolicy` writes them, under a temporary HKCU key (the
/// test may not write HKLM), removed after.
#[cfg(windows)]
#[test]
fn windows_reads_the_qos_policies() {
    use crate::winreg::wide;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, REG_DWORD, REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW,
        RegSetValueExW,
    };

    fn set(key: HKEY, name: &str, kind: u32, data: &[u8]) {
        let name = wide(name);
        // SAFETY: an open key, a NUL-terminated name, `data` readable.
        let r = unsafe {
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                kind,
                data.as_ptr(),
                data.len() as u32,
            )
        };
        assert_eq!(r, 0, "set a test value");
    }
    fn text(s: &str) -> Vec<u8> {
        wide(s).iter().flat_map(|w| w.to_le_bytes()).collect()
    }
    fn create(path: &str) -> HKEY {
        let path = wide(path);
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: a NUL-terminated path and a writable handle; the rest
        // default (null).
        let r = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                0,
                std::ptr::null(),
                0,
                windows_sys::Win32::System::Registry::KEY_ALL_ACCESS,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(r, 0, "create a test key");
        key
    }

    let root = format!(r"Software\dstack-test-qos-{}", std::process::id());
    let exe = std::env::current_exe().unwrap();
    let file = exe.file_name().unwrap().to_string_lossy().into_owned();

    let k = create(&format!(r"{root}\audio-tx"));
    set(k, "Version", REG_SZ, &text("2.0"));
    set(k, "AppName", REG_SZ, &text(&file));
    set(k, "DSCP", REG_DWORD, &46u32.to_le_bytes());
    set(k, "Protocol", REG_DWORD, &2u32.to_le_bytes());
    set(k, "NetProfile", REG_DWORD, &7u32.to_le_bytes());
    // SAFETY: the key opened above.
    unsafe { RegCloseKey(k) };
    let k = create(&format!(r"{root}\by-port"));
    set(k, "AppName", REG_SZ, &text(&file));
    set(k, "DSCP", REG_DWORD, &46u32.to_le_bytes());
    set(k, "DstPortLow", REG_DWORD, &4321u32.to_le_bytes());
    // SAFETY: the key opened above.
    unsafe { RegCloseKey(k) };

    let mut policies = windows::read_policies(HKEY_CURRENT_USER, &root);
    let path = wide(&root);
    // SAFETY: a NUL-terminated path under HKCU this test created.
    unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, path.as_ptr()) };

    policies.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(policies.len(), 2);
    let (tx, port) = (&policies[0], &policies[1]);
    assert_eq!(tx.name, "audio-tx");
    assert!(
        tx.values
            .contains(&("DSCP".into(), PolicyValue::Number(46)))
    );
    assert!(
        tx.values
            .contains(&("AppName".into(), PolicyValue::Text(file.clone())))
    );
    assert!(
        tx.values
            .contains(&("Version".into(), PolicyValue::Text("2.0".into())))
    );
    assert!(
        port.values
            .contains(&("DstPortLow".into(), PolicyValue::Number(4321)))
    );
    // The port-bound one never counts, whichever order the registry lists.
    let exe = exe.to_string_lossy();
    assert_eq!(marking_policy(&policies, &exe), Some("audio-tx"));
    assert_eq!(marking_policy(&policies[1..], &exe), None);
    // The machine's own policies read without failing.
    let _ = windows::read_policies(crate::winreg::HKEY_LOCAL_MACHINE, POLICY_KEY);
}
