// SPDX-License-Identifier: MIT OR Apache-2.0

//! A bridge instance's group (dstack #292): the ASIO driver, the two
//! channel maps and the app buffer, shown only on a bridge instance's tab,
//! in the place of the latency group (dstack #512: the window keeps its
//! size; the told latency's text moves under the group's rows and says
//! only what dasio reports).
//! Every decision is [`dtray::dasio::bridge`]'s and
//! [`dtray::dasio::choices::drivers`]'s. A tab switch shows the tab's
//! last valid bridge again: text typed there that was not yet a number
//! (the reason under the fields) is dropped.

use dtray::dasio::bridge as pure;
use windows_sys::Win32::UI::WindowsAndMessaging::{ES_AUTOHSCROLL, MoveWindow, SW_HIDE, WS_BORDER};

use super::*;

/// The group's top: the lower row's, the latency group's place.
const TOP: i32 = LOWER_Y;

/// The group's controls (all hidden on a DAW instance's tab).
const IDS: [usize; 15] = [
    ID_BRIDGE_GROUP,
    ID_DRIVER,
    ID_TOASIO,
    ID_FROMASIO,
    ID_APPBUF,
    ID_BRIDGE_LABEL,
    ID_BRIDGE_LABEL + 1,
    ID_BRIDGE_LABEL + 2,
    ID_BRIDGE_LABEL + 3,
    ID_BRIDGE_LABEL + 4,
    ID_RESTART + 6,
    ID_RESTART + 7,
    ID_RESTART + 8,
    ID_RESTART + 9,
    ID_BRIDGE_LABEL + 5,
];

/// The controls under the lower row (x, y, width, height), the same on
/// every tab.
pub(super) const BELOW: [(usize, (i32, i32, i32, i32)); 4] = [
    (ID_BANNER, (16, LOWER_Y + LOWER_H + 6, WIDTH - 32, 34)),
    (ID_RESULT, (16, LOWER_Y + LOWER_H + 44, WIDTH - 280, 34)),
    (ID_CANCEL, (WIDTH - 236, LOWER_Y + LOWER_H + 46, 90, 28)),
    (ID_APPLY, (WIDTH - 138, LOWER_Y + LOWER_H + 46, 122, 28)),
];

/// Builds the group (hidden until a bridge instance's tab shows it).
pub(super) fn build(h: HWND, font: HFONT) {
    let st =
        |text: &str, id: usize, r| control(h, font, "STATIC", text, SS_LEFT | SS_NOPREFIX, id, r);
    control(
        h,
        font,
        "BUTTON",
        "ASIO bridge (this instance bridges Dante to a local ASIO driver)",
        BS_GROUPBOX as u32,
        ID_BRIDGE_GROUP,
        (16, TOP, HALF_W, LOWER_H),
    );
    let (label_x, ctl_x, ctl_w, mark_x) = (28, 184, 290, 16 + HALF_W - 26);
    let rows = [
        (pure::DRIVER, ID_DRIVER),
        (pure::TO_ASIO, ID_TOASIO),
        (pure::FROM_ASIO, ID_FROMASIO),
        ("App buffer, ms", ID_APPBUF),
    ];
    for (i, (text, id)) in rows.into_iter().enumerate() {
        let y = TOP + 18 + i as i32 * 26;
        st(text, ID_BRIDGE_LABEL + i, (label_x, y + 3, 150, 20));
        if id == ID_DRIVER {
            control(
                h,
                font,
                "COMBOBOX",
                "",
                CBS_DROPDOWNLIST as u32 | WS_VSCROLL | WS_TABSTOP,
                id,
                (ctl_x, y, ctl_w, 320),
            );
        } else {
            let w = if id == ID_APPBUF { 80 } else { ctl_w };
            control(
                h,
                font,
                "EDIT",
                "",
                WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32,
                id,
                (ctl_x, y, w, 22),
            );
        }
        st("↻", ID_RESTART + 6 + i, (mark_x, y + 3, 20, 20));
    }
    st(
        pure::DEFAULT_NOTE,
        ID_BRIDGE_LABEL + 5,
        (ctl_x + 88, TOP + 18 + 3 * 26 + 1, ctl_w - 88, 34),
    );
    show(h, false);
}

/// Shows or hides the group; the latency group the other way, and the
/// told latency's text in the place of the shown one.
pub(super) fn show(h: HWND, on: bool) {
    let (cmd, other) = if on {
        (SW_SHOW, SW_HIDE)
    } else {
        (SW_HIDE, SW_SHOW)
    };
    for id in IDS {
        // SAFETY: a control of ours.
        unsafe { ShowWindow(item(h, id), cmd) };
    }
    // SAFETY: controls of ours.
    unsafe {
        ShowWindow(item(h, ID_TOLD_GROUP), other);
        let (x, y, w, ht) = if on { TOLD_BRIDGE } else { TOLD_DAW };
        MoveWindow(item(h, ID_TOLD), x, y, w, ht, 1);
    }
}

/// The group's values into its controls.
pub(super) fn fill_values(h: HWND, list: &[Choice<String>], b: &dasio::Bridge) {
    fill(h, ID_DRIVER, list, choices::index_of(list, &b.driver));
    set_text(h, ID_TOASIO, &pure::map_text(&b.to_asio));
    set_text(h, ID_FROMASIO, &pure::map_text(&b.from_asio));
    set_text(h, ID_APPBUF, &pure::buffer_text(b.app_buffer_ns));
    // The file's keys the window has no control for (dstack #387).
    set_text(h, ID_BRIDGE_LABEL + 5, &pure::note_text(b));
}

/// The group's controls as a bridge ([`pure::from_texts`]).
pub(super) fn read(h: HWND, list: &[Choice<String>]) -> Result<dasio::Bridge, String> {
    let driver = list
        .get(selected(h, ID_DRIVER))
        .map_or("", |c| c.value.as_str());
    pure::from_texts(
        driver,
        &control_text(h, ID_TOASIO),
        &control_text(h, ID_FROMASIO),
        &control_text(h, ID_APPBUF),
    )
}
