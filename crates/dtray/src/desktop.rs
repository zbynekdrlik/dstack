// SPDX-License-Identifier: MIT OR Apache-2.0

//! The Linux desktop's files for the tray (pure, every platform; the
//! binary's `linux.rs` only reads the environment and calls them):
//!
//! - [`lock_path`]: the single-instance lock, `$XDG_RUNTIME_DIR/dtray.lock`.
//! - [`autostart_file`]: the start at logon, the XDG autostart entry
//!   `~/.config/autostart/dtray.desktop` (`$XDG_CONFIG_HOME` when set).
//! - [`desktop_entry`]: that entry's text.

use std::path::{Path, PathBuf};

/// The lock file's name.
pub const LOCK_NAME: &str = "dtray.lock";
/// The autostart entry's name.
pub const DESKTOP_NAME: &str = "dtray.desktop";

/// An environment directory as given: `None` when unset, empty or not
/// absolute (the XDG rule: a relative path is ignored).
fn absolute(dir: Option<&Path>) -> Option<&Path> {
    dir.filter(|d| d.is_absolute())
}

/// The single-instance lock in the user's runtime directory
/// (`XDG_RUNTIME_DIR`, `runtime`); `None` without one: a desktop session
/// always has it.
pub fn lock_path(runtime: Option<&Path>) -> Option<PathBuf> {
    absolute(runtime).map(|d| d.join(LOCK_NAME))
}

/// The autostart entry: in `XDG_CONFIG_HOME` (`config`) or else
/// `$HOME/.config` (`home`), under `autostart`; `None` with neither.
pub fn autostart_file(config: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    let base = match absolute(config) {
        Some(c) => c.to_path_buf(),
        None => absolute(home)?.join(".config"),
    };
    Some(base.join("autostart").join(DESKTOP_NAME))
}

/// The program at `exe` as one quoted argument of an entry's `Exec` key:
/// in double quotes, each `"`, `` ` ``, `$` and `\` escaped with a
/// backslash (the quoting rule), then each backslash doubled (the string
/// rule, applied when the file is read before the quoting rule), and each
/// `%` doubled (a field code otherwise). `None` for a path that is not
/// Unicode or holds a control character (a line break would end the key).
pub fn exec_arg(exe: &Path) -> Option<String> {
    let path = exe.to_str()?;
    if path.chars().any(char::is_control) {
        return None;
    }
    let mut out = String::from("\"");
    for c in path.chars() {
        match c {
            '\\' => out.push_str(r"\\\\"),
            '"' | '`' | '$' => {
                out.push_str(r"\\");
                out.push(c);
            }
            '%' => out.push_str("%%"),
            _ => out.push(c),
        }
    }
    out.push('"');
    Some(out)
}

/// The autostart entry's text for the program at `exe`; `None` when the
/// path cannot be written as an argument ([`exec_arg`]).
pub fn desktop_entry(exe: &Path) -> Option<String> {
    let exec = exec_arg(exe)?;
    Some(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=dtray\n\
         Comment=The tray icon for this PC's dstack devices\n\
         Exec={exec}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n"
    ))
}

#[cfg(test)]
mod tests;
