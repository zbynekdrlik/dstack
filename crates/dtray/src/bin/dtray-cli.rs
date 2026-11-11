// SPDX-License-Identifier: MIT OR Apache-2.0

//! `dtray-cli`: dtray's command-line modes as a console program (dstack
//! #218), for scripts and automation. `dtray.exe` is a GUI-subsystem
//! program on Windows: PowerShell's `&` neither waits for it nor sets
//! `$LASTEXITCODE`, and a capturing shell (a remote shell, CI, a script
//! with its output in a pipe) reads none of its lines. dtray-cli is a
//! console program on every platform, so its lines go to the pipes the
//! caller gave and the shell waits for its exit code.
//!
//! It runs the same modes as dtray, by the same code ([`dtray::cli`]):
//! `--print`, `--version`, `--set-latency <device> <ms>`,
//! `--switch <label> <source>` and `--autostart on|off`. It never starts a
//! tray: no arguments, like any other usage error, prints
//! [`dtray::cli::CLI_USAGE`] on stderr and exits 2. `--autostart` runs the
//! tray program beside it (`dtray`, `dtray.exe` on Windows:
//! [`dtray::cli::tray_program`]) with the same arguments and exits with
//! its code, so the start at logon names the tray, never dtray-cli; 1 when
//! that program does not start.

use std::process::{Command, ExitCode};

use dtray::cli;

fn main() -> ExitCode {
    let mode = dtray::mode_of(std::env::args().skip(1)).ok();
    cli::run(
        mode,
        cli::CLI_USAGE,
        || cli::usage_error(cli::CLI_USAGE),
        autostart,
    )
}

/// `dtray --autostart on|off` by the tray program beside this one, its
/// exit code relayed (its lines go to the same stdout and stderr).
fn autostart(on: bool) -> ExitCode {
    let tray = match std::env::current_exe() {
        Ok(exe) => cli::tray_program(&exe),
        Err(e) => {
            eprintln!("dtray-cli: the program's path: {e}");
            return ExitCode::FAILURE;
        }
    };
    match Command::new(&tray).args(cli::autostart_args(on)).status() {
        Ok(status) => match status.code().and_then(|c| u8::try_from(c).ok()) {
            Some(code) => ExitCode::from(code),
            None => {
                eprintln!(
                    "dtray-cli: {} --autostart ended without an exit code ({status})",
                    tray.display()
                );
                ExitCode::FAILURE
            }
        },
        Err(e) => {
            eprintln!(
                "dtray-cli: the tray program {} did not start: {e}",
                tray.display()
            );
            ExitCode::FAILURE
        }
    }
}
