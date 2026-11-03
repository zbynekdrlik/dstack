// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! `dstack-dev`: the stack's own device binary, for its tests and the
//! real-hardware gate. Glue only: [`dstack::daemon::cli::main`] does
//! the work, as it does for every application's binary.

#[cfg(any(target_os = "linux", windows))]
fn main() -> std::process::ExitCode {
    dstack::daemon::cli::main(dstack::daemon::Build::OWN)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn main() -> std::process::ExitCode {
    eprintln!("dstack-dev runs on Linux and Windows only");
    std::process::ExitCode::from(2)
}
