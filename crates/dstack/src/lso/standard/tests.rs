use std::cmp::Ordering;

use super::*;

/// The embedded copy is the standard itself: a change to
/// `docs/nic-standard.json` without the copy fails here (dstack #466).
#[test]
fn the_embedded_copy_equals_the_standard_in_docs() {
    assert_eq!(FILE, include_str!("../../../../../docs/nic-standard.json"));
}

/// The embedded file parses, with the Windows entries the start line uses.
#[test]
fn the_embedded_standard_parses() {
    let std = embedded().expect("the embedded standard parses");
    assert!(std.version >= 1);
    let ids: Vec<&str> = std.adapters.iter().map(|e| e.id.as_str()).collect();
    for id in [
        "rtl8156b-windows",
        "rtl8125-rtl8168-windows",
        "realtek-vlan-windows",
    ] {
        assert!(ids.contains(&id), "{id} missing from {ids:?}");
    }
}

/// The hardware ids read on resolume-pp (20.11.2026, the class key's
/// `MatchingDeviceId`) and FOH's RTL8125 find their entries, in any case;
/// a Linux entry or another chip finds none.
#[test]
fn an_adapter_finds_its_windows_entry_by_hardware_id_prefix() {
    let std = embedded().unwrap();
    let id = |h: &str| std.windows_entry(h).map(|e| e.id.as_str());
    assert_eq!(
        id(r"PCI\VEN_10EC&DEV_8168&SUBSYS_393917AA&REV_15"),
        Some("rtl8125-rtl8168-windows")
    );
    assert_eq!(
        id(r"pci\ven_10ec&dev_8125&subsys_88671043&rev_05"),
        Some("rtl8125-rtl8168-windows")
    );
    assert_eq!(
        id(r" USB\VID_0BDA&PID_8156&REV_31F4 "),
        Some("rtl8156b-windows")
    );
    assert_eq!(id("RTK_VLANMP"), Some("realtek-vlan-windows"));
    assert_eq!(id("rtk_vlanmp"), Some("realtek-vlan-windows"));
    assert_eq!(id(r"PCI\VEN_10EC&DEV_8161"), None);
    assert_eq!(id(r"USB\VID_0BDA&PID_8153"), None);
    assert_eq!(id(r"PCI\VEN_8086&DEV_15F3"), None);
    // Prefix, not substring: the id must start with the entry's prefix.
    assert_eq!(id(r"X\PCI\VEN_10EC&DEV_8168"), None);
    assert_eq!(id(""), None);
}

/// Only Windows entries answer a Windows lookup: a Linux entry with a
/// Windows-looking id is skipped.
#[test]
fn a_linux_entry_never_answers_a_windows_lookup() {
    let mut std = embedded().unwrap().clone();
    for e in &mut std.adapters {
        if e.os == "windows" {
            e.os = "linux".into();
        }
    }
    assert_eq!(std.windows_entry(r"PCI\VEN_10EC&DEV_8168"), None);
}

/// The checkers' version order: number by number, zero-padded.
#[test]
fn versions_compare_number_by_number_padded_with_zeros() {
    assert_eq!(
        compare_versions("10.79.20.1003", "10.79.20.1003"),
        Ordering::Equal
    );
    assert_eq!(compare_versions("10.79.20", "10.79.20.0"), Ordering::Equal);
    assert_eq!(
        compare_versions("10.79.50.1003", "10.79.20.1003"),
        Ordering::Greater
    );
    assert_eq!(
        compare_versions("10.79.20.1002", "10.79.20.1003"),
        Ordering::Less
    );
    // Numbers, not text: 10.9 is under 10.79, 1156.22 over 11.19.
    assert_eq!(compare_versions("10.9", "10.79"), Ordering::Less);
    assert_eq!(
        compare_versions("1156.22.113.2026", "11.19.602.2025"),
        Ordering::Greater
    );
    assert_eq!(
        compare_versions("10.79.20.1", "10.79.20"),
        Ordering::Greater
    );
    assert_eq!(compare_versions("", "10.79"), Ordering::Less);
}

/// The driver file at the end of a service's `ImagePath`, as read on
/// resolume-pp (20.11.2026).
#[test]
fn the_driver_file_is_the_image_path_s_last_part() {
    assert_eq!(
        file_name(r"\SystemRoot\System32\drivers\rt640x64.sys"),
        "rt640x64.sys"
    );
    assert_eq!(
        file_name(
            r"\SystemRoot\System32\DriverStore\FileRepository\rtucx22x64.inf_amd64_01a5900d61d5555f\rtucx22x64.sys"
        ),
        "rtucx22x64.sys"
    );
    assert_eq!(file_name(r#" "C:\x\RtVlan640.sys" "#), "RtVlan640.sys");
    assert_eq!(file_name("system32/drivers/netvsc.sys"), "netvsc.sys");
    assert_eq!(file_name("rt640x64.sys"), "rt640x64.sys");
}

/// A service's driver file: its `ImagePath`'s file, else the file Windows
/// loads without one (`<service>.sys`; resolume-pp's VLAN service `VLAN`
/// has `RtVlan640.sys`, so the path wins).
#[test]
fn a_service_s_driver_file_is_its_image_path_s_or_its_name_s() {
    assert_eq!(
        driver_file("VLAN", Some(r"\SystemRoot\System32\drivers\RtVlan640.sys")),
        "RtVlan640.sys"
    );
    assert_eq!(driver_file("rt640x64", None), "rt640x64.sys");
    assert_eq!(driver_file(" rt640x64 ", Some("")), "rt640x64.sys");
}
