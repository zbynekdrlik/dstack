// SPDX-License-Identifier: GPL-3.0-or-later

//! The dev CLI's Windows-only part.

use std::path::Path;
use std::process::ExitCode;

/// `ctl` on Windows: no control socket there (dasio's tray reads the
/// device's shared memory), exit 1.
pub(super) fn ctl_request(name: &str, _path: &Path, _request: &str) -> ExitCode {
    eprintln!("{name} ctl: Windows has no control socket");
    ExitCode::from(1)
}
