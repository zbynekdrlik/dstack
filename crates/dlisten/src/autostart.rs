// SPDX-License-Identifier: MIT OR Apache-2.0

//! The start at logon from the tray's menu (dstack #139), as dtray's: the
//! user's Run value on Windows, an XDG autostart entry on Linux. Pure: the
//! texts and paths; the binary's `win.rs` and `linux.rs` write them. The
//! entry starts dlisten with the arguments it runs with now (an `--ip`, an
//! `--output`), so the logon start is the same device.

use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

/// The name of dlisten's value under the user's
/// `Software\Microsoft\Windows\CurrentVersion\Run` key.
pub const RUN_VALUE_NAME: &str = "dlisten";
/// The autostart entry's name.
pub const DESKTOP_NAME: &str = "dlisten.desktop";

/// One argument on a Windows command line: as it is when it has no space,
/// tab or quote and is not empty; else in double quotes, each quote and the
/// backslashes before it escaped (the C runtime's rule).
pub fn windows_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                // The n backslashes already out become 2n, then one for
                // the quote itself.
                out.push_str(&"\\".repeat(slashes + 1));
                slashes = 0;
                out.push('"');
                continue;
            }
            _ => slashes = 0,
        }
        out.push(c);
    }
    // Backslashes before the closing quote are doubled.
    out.push_str(&"\\".repeat(slashes));
    out.push('"');
    out
}

/// The Run value's text: the program at `exe` in double quotes
/// (`dtray::run_value`), then `args`; `None` for a path that is not valid
/// Unicode.
pub fn run_value(exe: &Path, args: &[String]) -> Option<String> {
    let mut text = dtray::run_value(exe)?;
    for a in args {
        text.push(' ');
        text.push_str(&windows_arg(a));
    }
    Some(text)
}

/// The autostart entry's path: dtray's folder (`XDG_CONFIG_HOME`, else
/// `$HOME/.config`, then `autostart`) with [`DESKTOP_NAME`].
#[cfg(unix)]
pub fn autostart_file(config: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    dtray::desktop::autostart_file(config, home).map(|f| f.with_file_name(DESKTOP_NAME))
}

/// The autostart entry's text for the program at `exe` with `args`, each
/// quoted as dtray quotes the program (`dtray::desktop::exec_arg`);
/// `None` when one cannot be written.
#[cfg(unix)]
pub fn desktop_entry(exe: &Path, args: &[String]) -> Option<String> {
    let mut exec = dtray::desktop::exec_arg(exe)?;
    for a in args {
        exec.push(' ');
        exec.push_str(&dtray::desktop::exec_arg(Path::new(a))?);
    }
    Some(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=dlisten\n\
         Comment=Plays Dante audio on this PC's output\n\
         Exec={exec}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_windows_arguments_stay_and_others_are_quoted() {
        assert_eq!(windows_arg("--ip"), "--ip");
        assert_eq!(windows_arg(r"C:\x\y"), r"C:\x\y");
        assert_eq!(windows_arg(""), "\"\"");
        assert_eq!(windows_arg("Speakers (USB)"), "\"Speakers (USB)\"");
        assert_eq!(windows_arg("a\tb"), "\"a\tb\"");
        assert_eq!(windows_arg(r#"say "hi""#), r#""say \"hi\"""#);
        // A backslash before a quote, and at the end, is doubled.
        assert_eq!(windows_arg(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(windows_arg(r"C:\my dir\"), r#""C:\my dir\\""#);
        assert_eq!(windows_arg(r"a\b c"), r#""a\b c""#);
    }

    #[test]
    fn the_run_value_is_the_quoted_program_and_its_arguments() {
        let exe = Path::new(r"C:\Program Files\dstack\dlisten.exe");
        assert_eq!(
            run_value(exe, &["--output".into(), "HDMI 2".into()]),
            Some(r#""C:\Program Files\dstack\dlisten.exe" --output "HDMI 2""#.into())
        );
        assert_eq!(
            run_value(exe, &[]),
            Some(r#""C:\Program Files\dstack\dlisten.exe""#.into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_autostart_entry_starts_dlisten_with_its_arguments() {
        let file = autostart_file(None, Some(Path::new("/home/u"))).unwrap();
        assert_eq!(
            file,
            PathBuf::from("/home/u/.config/autostart/dlisten.desktop")
        );
        assert_eq!(autostart_file(None, None), None);
        let text = desktop_entry(
            Path::new("/opt/d listen"),
            &["--ip".into(), "192.0.2.5".into()],
        )
        .unwrap();
        assert!(
            text.contains("\nExec=\"/opt/d listen\" \"--ip\" \"192.0.2.5\"\n"),
            "{text}"
        );
        assert!(text.starts_with("[Desktop Entry]\nType=Application\nName=dlisten\n"));
        assert!(text.contains("\nTerminal=false\n"));
        assert_eq!(desktop_entry(Path::new("/x"), &["a\nb".into()]), None);
    }
}
