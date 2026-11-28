// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where the settings window goes (dstack #512): inside the work area of
//! the screen (the screen less the taskbar), so a small screen (1366 x 768
//! at 125 %) still shows its title bar and its buttons. Every tab has the
//! same size (a bridge instance's group takes the latency group's place).
//! Pure, every platform; the window's glue only reads the work area and
//! moves it.

use crate::panel::Rect;

/// The window's top-left corner for an outer size of `w` x `h` inside
/// `work`: centred, and never left of or above the work area (a window
/// larger than the work area keeps its title bar on the screen).
pub fn place(work: Rect, w: i32, h: i32) -> (i32, i32) {
    (
        work.left + (work.width() - w).max(0) / 2,
        work.top + (work.height() - h).max(0) / 2,
    )
}

#[cfg(test)]
mod tests;
