// SPDX-License-Identifier: MIT OR Apache-2.0

//! dlisten's start at logon on Linux: the user's XDG autostart entry, as
//! dtray's ([`dlisten::autostart`]).

use dlisten::autostart;

fn env_path(name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os(name).map(Into::into)
}

fn autostart_file() -> Option<std::path::PathBuf> {
    autostart::autostart_file(
        env_path("XDG_CONFIG_HOME").as_deref(),
        env_path("HOME").as_deref(),
    )
}

/// Whether the user's autostart entry is there.
pub fn autostart_on() -> bool {
    autostart_file().is_some_and(|f| f.exists())
}

/// Writes (`on`, starting dlisten with `args`) or removes the user's
/// autostart entry; removing one that is not there is done.
pub fn set_autostart(on: bool, args: &[String]) -> Result<(), String> {
    let file =
        autostart_file().ok_or_else(|| "neither XDG_CONFIG_HOME nor HOME is set".to_string())?;
    if on {
        let exe = std::env::current_exe().map_err(|e| format!("the program's path: {e}"))?;
        let text = autostart::desktop_entry(&exe, args).ok_or_else(|| {
            "the program's path cannot be written in an autostart entry".to_string()
        })?;
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&file, text).map_err(|e| format!("{}: {e}", file.display()))
    } else {
        match std::fs::remove_file(&file) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("{}: {e}", file.display())),
        }
    }
}
