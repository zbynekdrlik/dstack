// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{Mode, USAGE, mode_of};

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

#[test]
fn no_argument_is_the_tray_and_print_prints() {
    assert_eq!(mode_of(args(&[])), Ok(Mode::Tray));
    assert_eq!(mode_of(args(&["--print"])), Ok(Mode::Print));
}

#[test]
fn anything_else_is_the_usage() {
    for a in [
        &["--prin"][..],
        &["print"],
        &["--print", "--print"],
        &["-h"],
    ] {
        assert_eq!(mode_of(args(a)), Err(USAGE), "{a:?}");
    }
}
