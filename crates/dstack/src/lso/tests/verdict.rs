//! The start line's driver verdict follows the NIC standard (dstack #466),
//! over the embedded file's real entries and the fleet's real drivers
//! (`docs/nic-standard.md`, the fleet table; resolume-pp's class key read
//! 20.11.2026).

use super::*;

fn facts(provider: &str, hardware_id: &str, file: &str, version: &str) -> DriverFacts {
    DriverFacts {
        provider: Some(provider.into()),
        hardware_id: Some(hardware_id.into()),
        file: Some(file.into()),
        version: Some(version.into()),
        description: None,
    }
}

fn entry_id(v: DriverVerdict<'_>) -> (&'static str, &str) {
    match v {
        DriverVerdict::Standard(e) => ("standard", e.id.as_str()),
        DriverVerdict::Bad(e, _) => ("bad", e.id.as_str()),
        DriverVerdict::File(e) => ("file", e.id.as_str()),
        DriverVerdict::Old(e) => ("old", e.id.as_str()),
        DriverVerdict::Unknown => ("unknown", ""),
        DriverVerdict::Other => ("other", ""),
    }
}

const FOH: &str = r"PCI\VEN_10EC&DEV_8125&SUBSYS_88671043&REV_05";
const DSG: &str = r"PCI\VEN_10EC&DEV_8168&SUBSYS_393917AA&REV_15";
const USB: &str = r"USB\VID_0BDA&PID_8156&REV_31F4";

/// FOH's RTL8125 on rt640x64 10.79.20.1003 is the standard's driver (its
/// minimum): quiet. The old list named it bad and warned at every start.
#[test]
fn foh_s_rt640x64_10_79_20_1003_follows_the_standard() {
    let std = standard::embedded().unwrap();
    let mut f = facts("Realtek", FOH, "rt640x64.sys", "10.79.20.1003");
    f.description = Some("Realtek Gaming 2.5GbE Family Controller".into());
    assert_eq!(
        entry_id(driver_verdict(std, &f)),
        ("standard", "rtl8125-rtl8168-windows")
    );
}

/// dsg's RTL8168 on 10.79.50.1003 (above the minimum): quiet.
#[test]
fn dsg_s_rt640x64_10_79_50_1003_follows_the_standard() {
    let std = standard::embedded().unwrap();
    let f = facts("Realtek", DSG, "rt640x64.sys", "10.79.50.1003");
    assert_eq!(
        entry_id(driver_verdict(std, &f)),
        ("standard", "rtl8125-rtl8168-windows")
    );
}

/// mbc's RTL8156B on Realtek's newer NetAdapterCx package
/// `rtu56cx22x64.sys` 1156.22.113.2026 (dasio peer 25.11.2026, dasio#24):
/// the standard's second allowed file, at its own minimum: quiet, in any
/// case. One build under that minimum is old; 1156.22 is no pass for the
/// other file's minimum alone.
#[test]
fn mbc_s_rtu56cx22x64_follows_the_standard_at_its_own_minimum() {
    let std = standard::embedded().unwrap();
    for file in ["rtu56cx22x64.sys", "RTU56CX22X64.SYS"] {
        let f = facts("Realtek", USB, file, "1156.22.113.2026");
        assert_eq!(
            entry_id(driver_verdict(std, &f)),
            ("standard", "rtl8156b-windows"),
            "{file}"
        );
    }
    for version in ["1156.22.112.2026", "11.19.602.2025"] {
        let f = facts("Realtek", USB, "rtu56cx22x64.sys", version);
        assert_eq!(
            entry_id(driver_verdict(std, &f)),
            ("old", "rtl8156b-windows"),
            "{version}"
        );
    }
}

/// The inbox driver `rtucx21x64.sys` stays bad at mbc's version too.
#[test]
fn the_inbox_driver_fails_at_mbc_s_version() {
    let std = standard::embedded().unwrap();
    let f = facts("Microsoft", USB, "rtucx21x64.sys", "1156.22.113.2026");
    assert_eq!(
        entry_id(driver_verdict(std, &f)),
        ("bad", "rtl8156b-windows")
    );
}

/// resolume-pp's RTL8156B on the standard's `rtucx22x64.sys`
/// 11.19.602.2025: quiet; the file name in any case.
#[test]
fn resolume_s_rtucx22x64_follows_the_standard() {
    let std = standard::embedded().unwrap();
    for file in ["rtucx22x64.sys", "RTUCX22X64.SYS"] {
        let f = facts("Realtek", USB, file, "11.19.602.2025");
        assert_eq!(
            entry_id(driver_verdict(std, &f)),
            ("standard", "rtl8156b-windows"),
            "{file}"
        );
    }
}

/// The version the standard names bad (10.72.524.2024: the 20-30 ms
/// receive DPC) warns with that evidence; the same file at another version
/// is not that bad entry.
#[test]
fn a_bad_version_warns_with_the_standard_s_evidence() {
    let std = standard::embedded().unwrap();
    let f = facts("Realtek", FOH, "rt640x64.sys", "10.72.524.2024");
    match driver_verdict(std, &f) {
        DriverVerdict::Bad(e, bad) => {
            assert_eq!(e.id, "rtl8125-rtl8168-windows");
            assert!(bad.evidence.contains("20-30 ms receive DPC"), "{bad:?}");
        }
        v => panic!("{v:?}"),
    }
    let f = facts("Realtek", FOH, "rt640x64.sys", "10.72.524.2025");
    assert_eq!(
        entry_id(driver_verdict(std, &f)),
        ("old", "rtl8125-rtl8168-windows")
    );
}

/// A bad driver named by its file alone (Microsoft's inbox
/// `rtucx21x64.sys`, provider Microsoft, as read on resolume-pp) warns at
/// any version, found by its hardware id, not its provider.
#[test]
fn a_bad_file_warns_whoever_provides_it() {
    let std = standard::embedded().unwrap();
    for version in ["11.4.211.2022", "99.0"] {
        let f = facts("Microsoft", USB, "rtucx21x64.sys", version);
        match driver_verdict(std, &f) {
            DriverVerdict::Bad(e, bad) => {
                assert_eq!(e.id, "rtl8156b-windows");
                assert_eq!(bad.file.as_deref(), Some("rtucx21x64.sys"));
            }
            v => panic!("{version}: {v:?}"),
        }
    }
}

/// Under the minimum warns; at it is quiet; no version read warns.
#[test]
fn a_version_under_the_minimum_warns() {
    let std = standard::embedded().unwrap();
    let v = |version: Option<&str>| {
        let mut f = facts("Realtek", DSG, "rt640x64.sys", "");
        f.version = version.map(Into::into);
        entry_id(driver_verdict(std, &f)).0
    };
    assert_eq!(v(Some("10.39.212.2020")), "old");
    assert_eq!(v(Some("10.79.20.1002")), "old");
    assert_eq!(v(Some("10.79.20.1003")), "standard");
    assert_eq!(v(Some(" 10.79.20.1003 ")), "standard");
    assert_eq!(v(None), "old");
}

/// No driver file read is not the entry's file.
#[test]
fn a_driver_file_not_read_warns() {
    let std = standard::embedded().unwrap();
    let mut f = facts("Realtek", DSG, "", "10.79.50.1003");
    f.file = None;
    assert_eq!(entry_id(driver_verdict(std, &f)).0, "file");
}

/// The Realtek VLAN adapter on the fleet's RtVlan640.sys 10.4.1117.2020
/// (`RTK_VLANMP`) is its own entry's: quiet.
#[test]
fn the_vlan_adapter_follows_its_own_entry() {
    let std = standard::embedded().unwrap();
    let mut f = facts("Realtek", "RTK_VLANMP", "RtVlan640.sys", "10.4.1117.2020");
    f.description = Some("Realtek Virtual Adapter".into());
    assert_eq!(
        entry_id(driver_verdict(std, &f)),
        ("standard", "realtek-vlan-windows")
    );
}

/// A Realtek chip the standard has no entry for warns: by its provider, or
/// by a Realtek PCI or USB vendor id under another provider; another
/// vendor's adapter is not judged.
#[test]
fn a_realtek_adapter_without_an_entry_warns() {
    let std = standard::embedded().unwrap();
    let v = |provider: Option<&str>, hw: Option<&str>| {
        let f = DriverFacts {
            provider: provider.map(Into::into),
            hardware_id: hw.map(Into::into),
            file: Some("x.sys".into()),
            version: Some("1.0".into()),
            description: None,
        };
        entry_id(driver_verdict(std, &f)).0
    };
    assert_eq!(
        v(Some("Realtek"), Some(r"PCI\VEN_10EC&DEV_8161")),
        "unknown"
    );
    assert_eq!(
        v(Some("Realtek"), Some(r"PCI\VEN_1234&DEV_0001")),
        "unknown"
    );
    assert_eq!(v(Some("Realtek"), None), "unknown");
    assert_eq!(
        v(Some("Microsoft"), Some(r"USB\VID_0BDA&PID_8153")),
        "unknown"
    );
    assert_eq!(v(None, Some(r"pci\ven_10ec&dev_8161")), "unknown");
    assert_eq!(v(Some("Intel"), Some(r"PCI\VEN_8086&DEV_15F3")), "other");
    assert_eq!(
        v(
            Some("Microsoft"),
            Some(r"VMBUS\{F8615163-DF3E-46C5-913F-F2D2F965ED0E}")
        ),
        "other"
    );
    assert_eq!(v(None, None), "other");
}

/// A Realtek VLAN virtual adapter the standard has no entry for (its
/// hardware id not read) is a Realtek adapter without an entry: it warns,
/// as the Windows checker fails it (review of #466: the earlier info line
/// was quieter than the checker).
#[test]
fn a_virtual_adapter_without_an_entry_warns_as_a_realtek_adapter() {
    let std = standard::embedded().unwrap();
    let f = DriverFacts {
        provider: Some("Realtek".into()),
        description: Some("Realtek Virtual Adapter #2".into()),
        version: Some("10.4.1117.2020".into()),
        ..DriverFacts::default()
    };
    assert_eq!(entry_id(driver_verdict(std, &f)).0, "unknown");
}

/// Another file at a bad entry's version is that file's verdict, not the
/// bad one's: a bad entry with a file matches that file only.
#[test]
fn a_bad_version_is_bad_only_for_its_own_file() {
    let std = standard::embedded().unwrap();
    let f = facts("Realtek", FOH, "rt68cx21x64.sys", "10.72.524.2024");
    assert_eq!(
        entry_id(driver_verdict(std, &f)),
        ("file", "rtl8125-rtl8168-windows")
    );
    let mut f = facts("Realtek", FOH, "", "10.72.524.2024");
    f.file = None;
    assert_eq!(entry_id(driver_verdict(std, &f)).0, "file");
}

fn line_of(f: &DriverFacts) -> Option<Line> {
    driver_line(&driver_verdict(standard::embedded().unwrap(), f), f)
}

/// The standard's driver is an info line naming its entry; FOH no longer
/// warns.
#[test]
fn the_standard_s_driver_is_an_info_line() {
    let line = line_of(&facts("Realtek", FOH, "rt640x64.sys", "10.79.20.1003")).unwrap();
    assert!(!line.warn, "{line:?}");
    assert!(
        line.text.contains("rt640x64.sys 10.79.20.1003")
            && line.text.contains("rtl8125-rtl8168-windows"),
        "{line:?}"
    );
}

/// The 20-30 ms receive DPC is the standard's evidence text, printed with
/// the bad driver's warning, beside the driver to install.
#[test]
fn a_bad_driver_warns_with_the_dpc_evidence_from_the_standard() {
    let line = line_of(&facts("Realtek", FOH, "rt640x64.sys", "10.72.524.2024")).unwrap();
    assert!(line.warn, "{line:?}");
    assert!(line.text.contains("20-30 ms receive DPC"), "{line:?}");
    assert!(
        line.text.contains("rt640x64.sys 10.79.20.1003 or newer"),
        "{line:?}"
    );
}

/// Another file and an old version warn, with the plan while the entry is
/// open and the evidence when it is settled.
#[test]
fn a_wrong_file_or_an_old_version_warns_with_the_note() {
    let ndis = line_of(&facts(
        "Realtek",
        USB,
        "rtu5cx22x64.sys",
        "1156.22.113.2026",
    ))
    .unwrap();
    assert!(ndis.warn, "{ndis:?}");
    assert!(
        ndis.text.contains(
            "install rtucx22x64.sys 11.19.602.2025 or newer or rtu56cx22x64.sys 1156.22.113.2026 or newer"
        ) && ndis.text.contains("rtl8156b-windows")
            && !ndis.text.contains("open: "),
        "{ndis:?}"
    );
    let old = line_of(&facts("Realtek", USB, "rtucx22x64.sys", "11.18.1.2024")).unwrap();
    assert!(old.warn && old.text.contains("minimum"), "{old:?}");
    let open = line_of(&facts("Realtek", DSG, "rt640x64.sys", "10.39.212.2020")).unwrap();
    assert!(
        open.warn && open.text.contains("minimum") && open.text.contains("open: "),
        "{open:?}"
    );
    let open = line_of(&facts("Realtek", DSG, "rtx.sys", "10.79.50.1003")).unwrap();
    assert!(
        open.warn
            && open.text.contains("is not the NIC standard's")
            && open.text.contains("open: "),
        "{open:?}"
    );
}

/// A Realtek adapter without an entry warns with its hardware id (a
/// virtual one too); another vendor's has no line.
#[test]
fn the_other_verdicts_lines() {
    let unknown = line_of(&facts("Realtek", r"PCI\VEN_10EC&DEV_8161", "x.sys", "1.0")).unwrap();
    assert!(unknown.warn, "{unknown:?}");
    assert!(
        unknown.text.contains(r"PCI\VEN_10EC&DEV_8161"),
        "{unknown:?}"
    );
    let virt = line_of(&DriverFacts {
        provider: Some("Realtek".into()),
        description: Some("Realtek Virtual Adapter".into()),
        version: Some("10.4.1117.2020".into()),
        ..DriverFacts::default()
    })
    .unwrap();
    assert!(
        virt.warn && virt.text.contains("hardware id not read"),
        "{virt:?}"
    );
    assert_eq!(
        line_of(&facts("Intel", r"PCI\VEN_8086&DEV_15F3", "e2f.sys", "2.1")),
        None
    );
}

/// A Realtek adapter as the Windows checker tells one
/// (`Test-RealtekAdapter`): its description, or a hardware id that STARTS
/// with a Realtek prefix (the VLAN adapter's `RTK_` too); its provider also
/// counts. Without an entry each warns.
#[test]
fn a_realtek_adapter_is_told_as_the_checker_tells_it() {
    let std = standard::embedded().unwrap();
    let v = |description: Option<&str>, hw: Option<&str>| {
        let f = DriverFacts {
            provider: Some("Microsoft".into()),
            hardware_id: hw.map(Into::into),
            description: description.map(Into::into),
            ..DriverFacts::default()
        };
        entry_id(driver_verdict(std, &f)).0
    };
    assert_eq!(
        v(Some("Realtek USB GbE Family Controller"), None),
        "unknown"
    );
    assert_eq!(v(Some("realtek gaming 2.5gbe"), None), "unknown");
    assert_eq!(v(None, Some("RTK_VLANX")), "unknown");
    assert_eq!(v(None, Some(r"usb\vid_0bda&pid_8153&rev_3000")), "unknown");
    // Prefix, not substring; and the vendor id with its `&`.
    assert_eq!(v(None, Some(r"X\PCI\VEN_10EC&DEV_8161")), "other");
    assert_eq!(v(None, Some(r"USB\VID_0BDAX")), "other");
    assert_eq!(v(Some("Intel(R) Ethernet I225-V"), None), "other");
}

/// The rule's other shapes, which the real file does not have yet: a bad
/// entry with a version only (any file), an entry without a minimum (any
/// version of its file), and its sentence without a minimum.
#[test]
fn a_version_only_bad_entry_and_an_entry_without_a_minimum() {
    let std = standard::Standard::parse(
        r#"{"version": 9, "adapters": [
            {"id": "t-windows", "chip": "RTL-T", "os": "windows",
             "match": {"hardware_id": ["PCI\\VEN_10EC&DEV_7777"]},
             "driver": {"allowed": [{"file": "t.sys"}], "status": "settled",
                        "evidence": "the test's evidence",
                        "bad": [{"version": "1.2.3", "evidence": "bad 1.2.3"}]}}
        ]}"#,
    )
    .unwrap();
    let hw = r"PCI\VEN_10EC&DEV_7777&REV_01";
    let f = |file: &str, version: &str| facts("Realtek", hw, file, version);
    let bad = f("other.sys", "1.2.3");
    match driver_verdict(&std, &bad) {
        DriverVerdict::Bad(e, b) => assert_eq!(
            (e.id.as_str(), b.evidence.as_str()),
            ("t-windows", "bad 1.2.3")
        ),
        v => panic!("{v:?}"),
    }
    assert_eq!(
        entry_id(driver_verdict(&std, &f("t.sys", "0.1"))),
        ("standard", "t-windows")
    );
    let mut none = f("t.sys", "");
    none.version = None;
    assert_eq!(entry_id(driver_verdict(&std, &none)).0, "standard");
    let line = driver_line(&driver_verdict(&std, &f("u.sys", "2")), &f("u.sys", "2")).unwrap();
    assert!(
        line.warn
            && line
                .text
                .contains("install t.sys (docs/nic-standard.md); the test's evidence"),
        "{line:?}"
    );
}

/// The sentence prints the file trimmed, as it was judged.
#[test]
fn the_sentence_prints_the_file_trimmed() {
    let line = line_of(&facts("Realtek", DSG, " rt640x64.sys ", "10.79.50.1003")).unwrap();
    assert!(
        !line.warn && line.text.contains("driver rt640x64.sys 10.79.50.1003 is"),
        "{line:?}"
    );
}
