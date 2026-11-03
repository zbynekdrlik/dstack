// SPDX-License-Identifier: MIT OR Apache-2.0

//! The devices this PC should have (dstack #104): an app's installer
//! registers its device with one file in [`devices_dir`], named after the
//! device plus [`EXTENSION`] (`My App.device`), and its uninstaller removes
//! it. The file's content is not read. dtray shows an expected device that
//! discovery does not find as "expected, not running" (red): a stopped
//! device announces nothing on mDNS, so without the file it would just
//! vanish from the tray.
//!
//! - [`dir_for`] (pure): the folder for a platform, an override first.
//! - [`names_in`] (pure): the device names of a folder's file names.
//! - [`expected`]: the names in a folder on disk.

use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use tracing::warn;

/// A registration file's extension.
pub const EXTENSION: &str = "device";
/// The environment variable that names another folder (tests, a check by
/// hand).
pub const DIR_VAR: &str = "DSTACK_DEVICES_DIR";
/// The folder on Linux.
pub const LINUX_DIR: &str = "/etc/dstack/devices";
/// The ProgramData folder when Windows does not name one.
pub const PROGRAM_DATA: &str = r"C:\ProgramData";

/// The registration folder: `over` when set and not empty; else on
/// Windows `<program_data>\dstack\devices` (`%ProgramData%`, its usual
/// place without it), on other platforms [`LINUX_DIR`].
pub fn dir_for(windows: bool, over: Option<OsString>, program_data: Option<OsString>) -> PathBuf {
    if let Some(dir) = over.filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    if windows {
        let base = program_data
            .filter(|d| !d.is_empty())
            .map_or_else(|| PathBuf::from(PROGRAM_DATA), PathBuf::from);
        base.join("dstack").join("devices")
    } else {
        PathBuf::from(LINUX_DIR)
    }
}

/// This PC's registration folder ([`dir_for`] with [`DIR_VAR`] and
/// `%ProgramData%`).
pub fn devices_dir() -> PathBuf {
    dir_for(
        cfg!(windows),
        std::env::var_os(DIR_VAR),
        std::env::var_os("ProgramData"),
    )
}

/// The device names of `files` (file names): each name ending in
/// `.device` (any case: Windows file names have none), less the extension,
/// not empty; sorted, each once (case aside, the way Dante names compare).
pub fn names_in<I: IntoIterator<Item = OsString>>(files: I) -> Vec<String> {
    let mut out: Vec<String> = files
        .into_iter()
        .filter_map(|f| {
            let path = PathBuf::from(f);
            let ext = path.extension()?.to_str()?;
            if !ext.eq_ignore_ascii_case(EXTENSION) {
                return None;
            }
            let stem = path.file_stem()?.to_str()?;
            (!stem.is_empty()).then(|| stem.to_string())
        })
        .collect();
    out.sort_by_key(|n| n.to_ascii_lowercase());
    out.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    out
}

/// The warning for a registration folder that could not be read with
/// `e`; `None` when there is none (nothing registered on this PC yet).
pub fn unreadable(e: &std::io::Error) -> Option<&'static str> {
    (e.kind() != ErrorKind::NotFound).then_some("the expected devices' folder could not be read")
}

/// The devices registered in `dir` ([`names_in`] of its files). No folder
/// is no device (nothing registered yet); a folder that cannot be read is
/// warned and counts as none.
pub fn expected(dir: &Path) -> Vec<String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            if let Some(why) = unreadable(&e) {
                warn!(error = %e, "dtray: {why}");
            }
            return Vec::new();
        }
    };
    let files = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.file_name());
    names_in(files)
}

#[cfg(test)]
mod tests;
