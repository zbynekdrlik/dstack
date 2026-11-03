// SPDX-License-Identifier: MIT OR Apache-2.0

use std::path::Path;

use super::{autostart_file, desktop_entry, exec_arg, lock_path};

#[test]
fn the_lock_is_in_the_runtime_directory() {
    assert_eq!(
        lock_path(Some(Path::new("/run/user/1000"))),
        Some(Path::new("/run/user/1000/dtray.lock").to_path_buf())
    );
    assert_eq!(lock_path(None), None);
    assert_eq!(lock_path(Some(Path::new(""))), None, "empty");
    assert_eq!(lock_path(Some(Path::new("run/user"))), None, "relative");
}

#[test]
fn the_autostart_entry_is_in_the_config_home_or_home_s_config() {
    let home = Some(Path::new("/home/u"));
    assert_eq!(
        autostart_file(None, home),
        Some(Path::new("/home/u/.config/autostart/dtray.desktop").to_path_buf())
    );
    assert_eq!(
        autostart_file(Some(Path::new("/cfg")), home),
        Some(Path::new("/cfg/autostart/dtray.desktop").to_path_buf())
    );
    // A relative or empty XDG_CONFIG_HOME is ignored.
    for ignored in ["", "cfg"] {
        assert_eq!(
            autostart_file(Some(Path::new(ignored)), home),
            Some(Path::new("/home/u/.config/autostart/dtray.desktop").to_path_buf()),
            "{ignored:?}"
        );
    }
    assert_eq!(autostart_file(None, None), None);
    assert_eq!(autostart_file(None, Some(Path::new("u"))), None);
    assert_eq!(
        autostart_file(Some(Path::new("/cfg")), None),
        Some(Path::new("/cfg/autostart/dtray.desktop").to_path_buf())
    );
}

#[test]
fn the_exec_argument_is_quoted_and_escaped_by_the_entry_rules() {
    assert_eq!(
        exec_arg(Path::new("/usr/local/bin/dtray")).as_deref(),
        Some("\"/usr/local/bin/dtray\"")
    );
    assert_eq!(
        exec_arg(Path::new("/opt/my apps/dtray")).as_deref(),
        Some("\"/opt/my apps/dtray\"")
    );
    // `"`, backtick, `$`: one backslash by quoting, doubled by the string
    // rule; a backslash: two by quoting, four in the file; `%` doubled.
    assert_eq!(
        exec_arg(Path::new(r#"/a"b`c$d\e%f/dtray"#)).as_deref(),
        Some(r#""/a\\"b\\`c\\$d\\\\e%%f/dtray""#)
    );
    for refused in ["/a\nb/dtray", "/a\tb/dtray"] {
        assert_eq!(exec_arg(Path::new(refused)), None, "{refused:?}");
    }
}

#[cfg(unix)]
#[test]
fn a_path_that_is_no_unicode_has_no_entry() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let p = Path::new(OsStr::from_bytes(b"/opt/\xff/dtray"));
    assert_eq!(exec_arg(p), None);
    assert_eq!(desktop_entry(p), None);
}

#[test]
fn the_entry_starts_the_program_at_logon() {
    assert_eq!(
        desktop_entry(Path::new("/usr/local/bin/dtray")).as_deref(),
        Some(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=dtray\n\
             Comment=The tray icon for this PC's dstack devices\n\
             Exec=\"/usr/local/bin/dtray\"\n\
             Terminal=false\n\
             X-GNOME-Autostart-enabled=true\n"
        )
    );
    assert_eq!(desktop_entry(Path::new("/a\nb")), None);
}
