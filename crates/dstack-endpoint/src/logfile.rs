// SPDX-License-Identifier: MIT OR Apache-2.0

//! The run's log file dlisten and dsend open at start (`<state file>.log`,
//! beside the settings file): the run before is kept as `<name>.log.1`
//! (dstack #470), so a restart (a deploy, a crash, a reboot) no longer
//! erases it, and since dstack #506 the log also rotates by size: past
//! [`MAX_BYTES`] it moves to `.log.1` (the older ones to `.2` and `.3`,
//! [`KEPT`]) and a new one starts. The disk use stays bounded at four files
//! of [`MAX_BYTES`] (dasio's bound), about two weeks of a dlisten that logs
//! its status every 10 s, so a field case is still readable days later.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The size past which the log rotates (10 MiB, dasio's bound).
pub const MAX_BYTES: u64 = 10 * 1024 * 1024;

/// How many older logs are kept (`.log.1` to `.log.3`).
pub const KEPT: u32 = 3;

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
    pub file: Rotating,
    pub previous: Previous,
}

/// Where the run before's log is kept: `<name>.log.1` beside `log`.
pub fn previous_of(log: &Path) -> PathBuf {
    kept_of(log, 1)
}

/// The `n`th older log beside `log`: `<name>.log.<n>`.
pub fn kept_of(log: &Path, n: u32) -> PathBuf {
    let mut name = OsString::from(log.as_os_str());
    name.push(format!(".{n}"));
    PathBuf::from(name)
}

/// Moves the older logs one up (`.2` to `.3`, `.1` to `.2`; the oldest
/// replaced), then `log` to `.1`. Only files are moved: something else in
/// a kept log's place (a folder) stays, and `log`'s move onto it fails.
/// An older log missing is no error.
fn shift(log: &Path, kept: u32) -> io::Result<()> {
    for n in (1..kept).rev() {
        let from = kept_of(log, n);
        if std::fs::symlink_metadata(&from).is_ok_and(|m| m.is_file()) {
            std::fs::rename(&from, kept_of(log, n + 1))?;
        }
    }
    std::fs::rename(log, previous_of(log))
}

/// Opens a new log at `log` (its folder created), keeping the run before:
/// the older logs move one up and an existing `log` becomes
/// [`previous_of`] ([`KEPT`] in all; `std::fs::rename` replaces on Linux
/// and Windows), then the new log is created. A move that fails for
/// another reason than "no log yet" still gives the new log, over the old
/// one, as before #470.
pub fn open(log: &Path) -> io::Result<Opened> {
    open_bounded(log, MAX_BYTES, KEPT)
}

/// [`open`] with another size bound and count (the tests' small ones).
pub fn open_bounded(log: &Path, max_bytes: u64, kept: u32) -> io::Result<Opened> {
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let previous = match shift(log, kept) {
        Ok(()) => Previous::Kept(previous_of(log)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Previous::Nothing,
        Err(e) => Previous::Lost(e),
    };
    let file = File::create(log)?;
    Ok(Opened {
        file: Rotating {
            path: log.to_path_buf(),
            file: Some(file),
            written: 0,
            max_bytes,
            kept,
        },
        previous,
    })
}

/// The run's log file, rotated by size: a write that would take it past
/// its bound first moves it to `.log.1` (the older ones up, [`KEPT`]) and
/// starts a new one, so a line written in one write is never split across
/// two files (tracing's formatter writes each event at once). A rotation
/// that cannot move the log goes on appending to it (the bound then holds
/// no longer, until a later rotation moves it).
#[derive(Debug)]
pub struct Rotating {
    path: PathBuf,
    /// `None` only when a rotation could not open any log again: the
    /// writes then fail (stderr still has the log).
    file: Option<File>,
    written: u64,
    max_bytes: u64,
    kept: u32,
}

impl Rotating {
    /// Moves the log to `.1` and opens a new one; when the move fails the
    /// old log goes on (appended), and only when neither opens is there
    /// no file.
    fn rotate(&mut self) {
        // Closed before the move (Windows renames an open file only with
        // FILE_SHARE_DELETE, which std gives, but closing is plain).
        self.file = None;
        let moved = shift(&self.path, self.kept).is_ok();
        self.file = if moved {
            File::create(&self.path).ok()
        } else {
            None
        }
        .or_else(|| {
            std::fs::OpenOptions::new()
                .append(true)
                .open(&self.path)
                .ok()
        });
        self.written = 0;
    }
}

impl Write for Rotating {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written > 0 && self.written.saturating_add(buf.len() as u64) > self.max_bytes {
            self.rotate();
        }
        if self.file.is_none() {
            // No log after a rotation: each write tries it again.
            self.file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .ok();
        }
        let Some(file) = self.file.as_mut() else {
            return Err(io::Error::other(format!(
                "the log {} could not be opened again after its rotation",
                self.path.display()
            )));
        };
        let n = file.write(buf)?;
        self.written = self.written.saturating_add(n as u64);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.as_mut().map_or(Ok(()), File::flush)
    }
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
    fn older_starts_move_up_and_the_oldest_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("dsend.log");
        start(&log, "first run");
        start(&log, "second run");
        start(&log, "third run");

        assert_eq!(read(&log), "third run\n");
        assert_eq!(read(&dir.path().join("dsend.log.1")), "second run\n");
        assert_eq!(read(&dir.path().join("dsend.log.2")), "first run\n");
        assert_eq!(
            names(dir.path()),
            ["dsend.log", "dsend.log.1", "dsend.log.2"]
        );
        // Two starts more: four files at most, the oldest run gone.
        start(&log, "fourth run");
        start(&log, "fifth run");
        assert_eq!(
            names(dir.path()),
            ["dsend.log", "dsend.log.1", "dsend.log.2", "dsend.log.3"]
        );
        assert_eq!(read(&dir.path().join("dsend.log.3")), "second run\n");
        assert_eq!(read(&log), "fifth run\n");
    }

    #[test]
    fn the_log_rotates_by_size_keeping_whole_lines_and_four_files() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("dlisten.log");
        let mut opened = open_bounded(&log, 100, 3).expect("the log opens");
        // 30-byte lines: three fit in 100 bytes, the fourth starts a file.
        for i in 0..20 {
            // One write a line, as tracing's formatter writes an event.
            let line = format!("{i:02} {}\n", "x".repeat(26));
            opened.file.write_all(line.as_bytes()).unwrap();
        }
        opened.file.flush().unwrap();
        let line = |i: usize| format!("{i:02} {}\n", "x".repeat(26));
        let lines = |r: std::ops::Range<usize>| r.map(line).collect::<String>();
        assert_eq!(
            names(dir.path()),
            [
                "dlisten.log",
                "dlisten.log.1",
                "dlisten.log.2",
                "dlisten.log.3"
            ]
        );
        assert_eq!(read(&log), lines(18..20));
        assert_eq!(read(&dir.path().join("dlisten.log.1")), lines(15..18));
        assert_eq!(read(&dir.path().join("dlisten.log.2")), lines(12..15));
        assert_eq!(read(&dir.path().join("dlisten.log.3")), lines(9..12));
        for name in names(dir.path()) {
            let len = std::fs::metadata(dir.path().join(&name)).unwrap().len();
            assert!(len <= 100, "{name}: {len}");
        }
    }

    #[test]
    fn a_line_longer_than_the_bound_gets_a_file_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("dlisten.log");
        let mut opened = open_bounded(&log, 10, 3).expect("the log opens");
        let long = "y".repeat(25);
        for text in [&long, &long, "ok"] {
            opened
                .file
                .write_all(format!("{text}\n").as_bytes())
                .unwrap();
        }
        assert_eq!(read(&log), "ok\n");
        assert_eq!(read(&dir.path().join("dlisten.log.1")), format!("{long}\n"));
        assert_eq!(read(&dir.path().join("dlisten.log.2")), format!("{long}\n"));
    }

    #[test]
    fn a_rotation_that_cannot_move_the_log_goes_on_in_it() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("dlisten.log");
        let mut opened = open_bounded(&log, 10, 3).expect("the log opens");
        opened.file.write_all(b"first line\n").unwrap();
        // A non-empty folder where `.1` goes: the log cannot move there.
        let blocker = dir.path().join("dlisten.log.1");
        std::fs::create_dir(&blocker).unwrap();
        std::fs::write(blocker.join("inside"), "x").unwrap();
        opened.file.write_all(b"second line\n").unwrap();
        assert_eq!(read(&log), "first line\nsecond line\n");
        assert_eq!(read(&blocker.join("inside")), "x");
    }

    #[test]
    fn a_write_up_to_the_bound_stays_and_one_past_it_rotates() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("dlisten.log");
        let mut opened = open_bounded(&log, 10, 3).expect("the log opens");
        opened.file.write_all(b"1234\n").unwrap();
        // 5 + 5 = 10 bytes: exactly the bound, one file.
        opened.file.write_all(b"5678\n").unwrap();
        assert_eq!(names(dir.path()), ["dlisten.log"]);
        assert_eq!(read(&log), "1234\n5678\n");
        // One byte past it: the log moves first.
        opened.file.write_all(b"9\n").unwrap();
        assert_eq!(names(dir.path()), ["dlisten.log", "dlisten.log.1"]);
        assert_eq!(read(&log), "9\n");
        assert_eq!(read(&dir.path().join("dlisten.log.1")), "1234\n5678\n");
    }

    #[test]
    fn a_first_write_past_the_bound_rotates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("dlisten.log");
        let mut opened = open_bounded(&log, 10, 3).expect("the log opens");
        let long = format!("{}\n", "z".repeat(30));
        opened.file.write_all(long.as_bytes()).unwrap();
        // No empty `.1`: an empty log is never moved.
        assert_eq!(names(dir.path()), ["dlisten.log"]);
        assert_eq!(read(&log), long);
    }

    #[test]
    fn a_log_that_could_not_be_opened_after_a_rotation_is_tried_again() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("dlisten.log");
        let mut opened = open_bounded(&log, 10, 3).expect("the log opens");
        // As a rotation that could open nothing leaves it.
        opened.file.file = None;
        opened.file.write_all(b"back\n").unwrap();
        assert_eq!(read(&log), "back\n");
        // A folder in the log's place: nothing opens, the write fails.
        let other = dir.path().join("x.log");
        let mut gone = open_bounded(&other, 10, 3).expect("the log opens");
        gone.file.file = None;
        std::fs::remove_file(&other).unwrap();
        std::fs::create_dir(&other).unwrap();
        let err = gone.file.write_all(b"lost\n").unwrap_err();
        assert!(
            err.to_string().contains("could not be opened again"),
            "{err}"
        );
    }

    #[test]
    fn the_bound_is_ten_mib_with_three_older_logs() {
        assert_eq!(MAX_BYTES, 10_485_760);
        assert_eq!(KEPT, 3);
        assert_eq!(
            kept_of(Path::new("s/dlisten.log"), 3),
            PathBuf::from("s/dlisten.log.3")
        );
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
