// SPDX-License-Identifier: MIT OR Apache-2.0

//! The run's log file dlisten and dsend open at start (`<state file>.log`,
//! dstack #470): the run before is kept as `<name>.log.1`, so a restart
//! (a deploy, a crash, a reboot) no longer erases it, and the disk use
//! stays bounded at two files.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

/// What became of the run before's log.
#[derive(Debug)]
pub enum Previous {
    /// There was none (a first start).
    Nothing,
    /// Kept at this path (`<name>.log.1`).
    Kept(PathBuf),
    /// It could not be kept (why); the new log was created over it.
    Lost(io::Error),
}

/// An opened log and what became of the run before's.
#[derive(Debug)]
pub struct Opened {
    pub file: File,
    pub previous: Previous,
}

/// Where the run before's log is kept: `<name>.log.1` beside `log`.
pub fn previous_of(log: &Path) -> PathBuf {
    let mut name = OsString::from(log.as_os_str());
    name.push(".1");
    PathBuf::from(name)
}

/// Opens a new log at `log` (its folder created), keeping the run before:
/// an existing `log` is renamed to [`previous_of`] (replacing an older
/// one; `std::fs::rename` replaces on Linux and Windows), then the new log
/// is created. A rename that fails for another reason than "no log yet"
/// still gives the new log, over the old one, as before #470.
pub fn open(log: &Path) -> io::Result<Opened> {
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let kept = previous_of(log);
    let previous = match std::fs::rename(log, &kept) {
        Ok(()) => Previous::Kept(kept),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Previous::Nothing,
        Err(e) => Previous::Lost(e),
    };
    let file = File::create(log)?;
    Ok(Opened { file, previous })
}

/// The start's line about its log (`app` the app's name): whether it is a
/// warning (the run before could not be kept), and its text, which names
/// the kept previous log when one was kept.
pub fn start_line(app: &str, log: &Path, previous: &Previous) -> (bool, String) {
    let log = log.display();
    match previous {
        Previous::Nothing => (false, format!("{app}: the log {log}")),
        Previous::Kept(p) => (
            false,
            format!(
                "{app}: the log {log}, the run before kept as {}",
                p.display()
            ),
        ),
        Previous::Lost(e) => (
            true,
            format!("{app}: the log {log}; the run before's was not kept ({e})"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    /// One start: opens the log and writes `line` into it.
    fn start(log: &Path, line: &str) -> Previous {
        let mut opened = open(log).expect("the log opens");
        writeln!(opened.file, "{line}").expect("the line is written");
        opened.previous
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).expect("the file reads")
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("the folder reads")
            .map(|e| {
                e.expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn two_starts_keep_the_first_starts_lines() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("dlisten.log");
        start(&log, "first run");
        let previous = start(&log, "second run");

        assert_eq!(read(&log), "second run\n");
        assert_eq!(read(&dir.path().join("dlisten.log.1")), "first run\n");
        match previous {
            Previous::Kept(p) => assert_eq!(p, dir.path().join("dlisten.log.1")),
            other => panic!("the first run's log was not kept: {other:?}"),
        }
    }

    #[test]
    fn a_third_start_replaces_the_older_previous_two_files_stay() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("dsend.log");
        start(&log, "first run");
        start(&log, "second run");
        start(&log, "third run");

        assert_eq!(read(&log), "third run\n");
        assert_eq!(read(&dir.path().join("dsend.log.1")), "second run\n");
        assert_eq!(names(dir.path()), ["dsend.log", "dsend.log.1"]);
    }

    #[test]
    fn a_first_start_keeps_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("dlisten.log");
        let previous = start(&log, "first run");

        assert!(matches!(previous, Previous::Nothing), "{previous:?}");
        assert_eq!(read(&log), "first run\n");
        assert_eq!(names(dir.path()), ["dlisten.log"]);
    }

    #[test]
    fn a_missing_folder_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("state").join("dlisten.log");
        start(&log, "first run");

        assert_eq!(read(&log), "first run\n");
    }

    #[test]
    fn a_previous_that_cannot_be_replaced_still_gives_a_new_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("dlisten.log");
        // A non-empty folder where `.1` goes: no file is renamed over it
        // (Linux EISDIR, Windows access denied).
        let blocker = dir.path().join("dlisten.log.1");
        std::fs::create_dir(&blocker).unwrap();
        std::fs::write(blocker.join("inside"), "x").unwrap();
        start(&log, "first run");
        let previous = start(&log, "second run");

        assert!(matches!(previous, Previous::Lost(_)), "{previous:?}");
        assert_eq!(read(&log), "second run\n");
        assert_eq!(read(&blocker.join("inside")), "x");
    }

    #[test]
    fn the_start_line_names_the_kept_previous_log() {
        let log = Path::new("state/dlisten.log");
        let kept = Previous::Kept(PathBuf::from("state/dlisten.log.1"));
        assert_eq!(
            start_line("dlisten", log, &kept),
            (
                false,
                format!(
                    "dlisten: the log {}, the run before kept as {}",
                    log.display(),
                    Path::new("state/dlisten.log.1").display()
                )
            )
        );
        assert_eq!(
            start_line("dsend", log, &Previous::Nothing),
            (false, format!("dsend: the log {}", log.display()))
        );
        let lost = Previous::Lost(io::Error::other("denied"));
        assert_eq!(
            start_line("dlisten", log, &lost),
            (
                true,
                format!(
                    "dlisten: the log {}; the run before's was not kept (denied)",
                    log.display()
                )
            )
        );
    }

    #[test]
    fn the_previous_is_the_log_with_dot_1() {
        assert_eq!(
            previous_of(Path::new("state/dlisten.log")),
            Path::new("state/dlisten.log.1")
        );
    }
}
