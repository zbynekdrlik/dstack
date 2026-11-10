// SPDX-License-Identifier: MIT OR Apache-2.0

//! The command line's parts dlisten and dsend share (dstack #164): each app
//! parses its own flags by hand and calls these. Pure: the binaries pass
//! the process's environment in.

use std::ffi::OsString;
use std::path::PathBuf;

use dlink::Ports;

/// The longest Dante device name.
pub const NAME_MAX: usize = 31;

/// `text` as a number for `flag`; the error names both.
pub fn number<T: std::str::FromStr>(flag: &str, text: &str) -> Result<T, String> {
    text.parse()
        .map_err(|_| format!("{flag} needs a number, not {text:?}"))
}

/// `<arc>,<cmc>,<flows>`, each a port over 0.
pub fn parse_ports(text: &str) -> Result<Ports, String> {
    let bad = || format!("--ports is <arc>,<cmc>,<flows>, not {text:?}");
    let parts: Vec<u16> = text
        .split(',')
        .map(|p| p.trim().parse::<u16>().ok().filter(|&p| p > 0))
        .collect::<Option<_>>()
        .ok_or_else(bad)?;
    match parts[..] {
        [arc, cmc, flows] => Ok(Ports { arc, cmc, flows }),
        _ => Err(bad()),
    }
}

/// The default device name for a host named `host`: `<host><suffix>`
/// (`-dlisten`, `-dsend`), the host's name made a Dante name (A-Z a-z 0-9
/// and '-', others as '-', no '-' at its ends) and cut so the whole is at
/// most [`NAME_MAX`]; `pc` for a name with nothing left.
pub fn default_name(host: &str, suffix: &str) -> String {
    let cleaned: String = host
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let room = NAME_MAX.saturating_sub(suffix.len());
    let short: String = cleaned.trim_matches('-').chars().take(room).collect();
    let short = short.trim_end_matches('-');
    let host = if short.is_empty() { "pc" } else { short };
    format!("{host}{suffix}")
}

/// Where the device of app `app` keeps what Dante Controller sets (names,
/// subscriptions, latency) when `--state` is not given: on Windows
/// `<local_app_data>\dstack\<app>\<name>.toml` (`%LOCALAPPDATA%`), else
/// `<xdg_state_home>/dstack/<app>/<name>.toml` or, without it,
/// `<home>/.local/state/...`; the temporary folder when none is set.
pub fn state_path(
    windows: bool,
    app: &str,
    name: &str,
    local_app_data: Option<OsString>,
    xdg_state_home: Option<OsString>,
    home: Option<OsString>,
    temp: PathBuf,
) -> PathBuf {
    let set = |v: Option<OsString>| v.filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if windows {
        set(local_app_data)
    } else {
        set(xdg_state_home).or_else(|| set(home).map(|h| h.join(".local").join("state")))
    };
    base.unwrap_or(temp)
        .join("dstack")
        .join(app)
        .join(format!("{name}.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_number_or_the_flag_s_error() {
        assert_eq!(number::<u32>("--rate", "44100"), Ok(44_100));
        assert_eq!(
            number::<u32>("--rate", "x"),
            Err("--rate needs a number, not \"x\"".into())
        );
    }

    #[test]
    fn ports_are_three_ports_over_zero() {
        assert_eq!(
            parse_ports("1, 2,3"),
            Ok(Ports {
                arc: 1,
                cmc: 2,
                flows: 3
            })
        );
        for bad in ["1,2", "1,2,3,4", "0,2,3", "a,2,3", ""] {
            assert_eq!(
                parse_ports(bad),
                Err(format!("--ports is <arc>,<cmc>,<flows>, not {bad:?}"))
            );
        }
    }

    #[test]
    fn the_default_name_takes_the_app_s_suffix_within_31() {
        assert_eq!(default_name("FOH-PC", "-dsend"), "FOH-PC-dsend");
        assert_eq!(default_name("a.b", "-dlisten"), "a-b-dlisten");
        assert_eq!(default_name("", "-dsend"), "pc-dsend");
        let long = default_name("abcdefghijklmnopqrstuvwxyz0123", "-dsend");
        assert_eq!(long, "abcdefghijklmnopqrstuvwxy-dsend");
        assert_eq!(long.len(), NAME_MAX);
        // A suffix longer than a name leaves no room, never a panic.
        assert_eq!(default_name("pc", &"x".repeat(40)), format!("pc{}", "x".repeat(40)));
    }

    #[test]
    fn the_state_file_is_under_the_app_s_folder() {
        let os = |s: &str| Some(OsString::from(s));
        assert_eq!(
            state_path(true, "dsend", "pc-dsend", os("C:\\L"), None, None, "/t".into()),
            PathBuf::from("C:\\L")
                .join("dstack")
                .join("dsend")
                .join("pc-dsend.toml")
        );
        assert_eq!(
            state_path(false, "dsend", "pc-dsend", None, None, os("/h"), "/t".into()),
            PathBuf::from("/h/.local/state/dstack/dsend/pc-dsend.toml")
        );
    }
}
