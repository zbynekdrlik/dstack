// SPDX-License-Identifier: MIT OR Apache-2.0

use std::ffi::OsString;
use std::path::PathBuf;

use super::{LINUX_DIR, PROGRAM_DATA, dir_for, expected, names_in};

fn os(s: &str) -> Option<OsString> {
    Some(OsString::from(s))
}

#[test]
fn the_folder_is_the_override_else_the_platform_s() {
    assert_eq!(dir_for(false, None, None), PathBuf::from(LINUX_DIR));
    assert_eq!(
        dir_for(false, None, os("/somewhere")),
        PathBuf::from(LINUX_DIR),
        "ProgramData is Windows only"
    );
    assert_eq!(
        dir_for(true, None, os("D:\\Data")),
        PathBuf::from("D:\\Data").join("dstack").join("devices")
    );
    let fallback = PathBuf::from(PROGRAM_DATA).join("dstack").join("devices");
    assert_eq!(dir_for(true, None, None), fallback);
    assert_eq!(dir_for(true, None, os("")), fallback);
    for windows in [false, true] {
        assert_eq!(
            dir_for(windows, os("/tmp/x"), os("D:\\Data")),
            PathBuf::from("/tmp/x")
        );
        assert_ne!(
            dir_for(windows, os(""), None),
            PathBuf::from(""),
            "an empty override is none"
        );
    }
}

#[test]
fn names_come_from_dot_device_files_only() {
    let files = [
        "zeta app.device",
        "Alpha.DEVICE",
        "notes.txt",
        "device",
        ".device",
        "beta.device.bak",
        "gamma.devices",
        "ALPHA.device",
    ]
    .map(OsString::from);
    assert_eq!(names_in(files), vec!["Alpha", "zeta app"]);
    assert!(names_in(Vec::<OsString>::new()).is_empty());
}

#[test]
fn a_folder_s_files_are_read_and_a_missing_folder_is_none() {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::write(dir.path().join("one.device"), b"").expect("a file");
    std::fs::write(dir.path().join("readme.txt"), b"").expect("a file");
    std::fs::create_dir(dir.path().join("sub.device")).expect("a folder");
    assert_eq!(expected(dir.path()), vec!["one"]);
    assert!(expected(&dir.path().join("gone")).is_empty());
    // A file where the folder should be: not readable as one, none.
    assert!(expected(&dir.path().join("one.device")).is_empty());
}
