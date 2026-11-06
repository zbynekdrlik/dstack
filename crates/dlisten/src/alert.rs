// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where a failure the user must see goes (dstack #139). A start from
//! Explorer or the Run value has no console and no stderr: there only a
//! message box tells the user. A start from a command line or a script has
//! one or the other, and a modal box there blocks the exit until someone
//! clicks it on the desktop (seen live: `dlisten --list-outputs` on a PC
//! with no output hung its caller).

/// Whether a failure shows a message box: only when the process got no
/// parent console (`attached` false) and has no stderr handle of its own
/// (`has_stderr` false: nothing redirected it).
pub fn shows_box(attached: bool, has_stderr: bool) -> bool {
    !attached && !has_stderr
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_start_with_a_console_or_a_redirected_stderr_shows_no_box() {
        assert!(!shows_box(true, false), "a command line's console");
        assert!(!shows_box(false, true), "a script's redirected stderr");
        assert!(!shows_box(true, true));
    }

    #[test]
    fn a_start_with_neither_shows_the_box() {
        assert!(shows_box(false, false), "Explorer or the Run value");
    }
}
