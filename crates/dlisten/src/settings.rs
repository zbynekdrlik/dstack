// SPDX-License-Identifier: MIT OR Apache-2.0

//! dlisten's own settings (dstack #139): the output chosen in the tray's
//! menu, kept in a small file next to the dlink state file, so a restart
//! plays on it again. The dlink state file is dstack's (what Dante
//! Controller sets); this one is dlisten's alone.
//!
//! The file is text, one `key=value` a line (`output=<the output's name>`;
//! a device name holds no line break). Lines it does not know are ignored,
//! so a later version may add keys.

use std::path::{Path, PathBuf};

/// The settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    /// The output's name, as the output list shows it.
    pub output: Option<String>,
}

const OUTPUT: &str = "output=";

/// The settings file next to the state file `state`: its name with
/// `.settings` in place of its extension (`pc-dlisten.toml` →
/// `pc-dlisten.settings`).
pub fn path_for(state: &Path) -> PathBuf {
    state.with_extension("settings")
}

/// The settings in a file's `text` (an empty or unknown line is skipped;
/// the last `output=` counts).
pub fn parse(text: &str) -> Settings {
    let mut s = Settings::default();
    for line in text.lines() {
        if let Some(name) = line.strip_prefix(OUTPUT) {
            s.output = (!name.is_empty()).then(|| name.to_string());
        }
    }
    s
}

/// The file's text for `s`.
pub fn text(s: &Settings) -> String {
    match &s.output {
        // A line break in a name would split it; it is a space instead.
        Some(name) => format!("{OUTPUT}{}\n", name.replace(['\r', '\n'], " ")),
        None => String::new(),
    }
}

/// The settings in `file`; the defaults when there is none yet. Another
/// read error is the caller's to log.
pub fn load(file: &Path) -> Result<Settings, String> {
    match std::fs::read_to_string(file) {
        Ok(t) => Ok(parse(&t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(e) => Err(format!("the settings {}: {e}", file.display())),
    }
}

/// Writes `s` into `file`, its folder made first.
pub fn save(file: &Path, s: &Settings) -> Result<(), String> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(file, text(s)).map_err(|e| format!("the settings {}: {e}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_sits_next_to_the_state_file() {
        assert_eq!(
            path_for(Path::new("/s/dstack/dlisten/pc-dlisten.toml")),
            PathBuf::from("/s/dstack/dlisten/pc-dlisten.settings")
        );
    }

    #[test]
    fn settings_round_trip_through_their_text() {
        let s = Settings {
            output: Some("Speakers (Realtek(R) Audio) = 2".into()),
        };
        assert_eq!(text(&s), "output=Speakers (Realtek(R) Audio) = 2\n");
        assert_eq!(parse(&text(&s)), s);
        assert_eq!(text(&Settings::default()), "");
        assert_eq!(parse(""), Settings::default());
    }

    #[test]
    fn unknown_and_empty_lines_are_skipped_and_the_last_output_counts() {
        let s = parse("volume=3\n\noutput=A\noutput=B\r\n");
        assert_eq!(s.output.as_deref(), Some("B"));
        assert_eq!(parse("output=\n").output, None);
        assert_eq!(parse("Output=A").output, None, "keys are exact");
    }

    #[test]
    fn a_line_break_in_a_name_cannot_split_the_file() {
        let s = Settings {
            output: Some("A\nB\rC".into()),
        };
        assert_eq!(text(&s), "output=A B C\n");
    }

    #[test]
    fn save_and_load_round_trip_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sub").join("pc-dlisten.settings");
        assert_eq!(load(&file), Ok(Settings::default()), "no file yet");
        let s = Settings {
            output: Some("HDMI".into()),
        };
        save(&file, &s).unwrap();
        assert_eq!(load(&file), Ok(s));
        // A folder where the file should be is an error, not the defaults.
        let err = load(dir.path()).unwrap_err();
        assert!(err.starts_with("the settings "), "{err}");
        assert!(save(&dir.path().join("sub"), &Settings::default()).is_err());
    }
}
