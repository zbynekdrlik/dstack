// SPDX-License-Identifier: MIT OR Apache-2.0

//! A bridge instance's group (dstack #292): the ASIO driver, the two
//! channel maps and the app buffer, shown only on a bridge instance's tab
//! (the window grows by [`GROUP_H`] and the controls under it move down).
//! Every decision is [`dtray::dasio::bridge`]'s and
//! [`dtray::dasio::choices::drivers`]'s.

use dtray::dasio::bridge as pure;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ES_AUTOHSCROLL, GetWindowTextLengthW, GetWindowTextW, MoveWindow, SW_HIDE, SWP_NOMOVE,
    SWP_NOZORDER, SetWindowPos, WS_BORDER,
};

use super::*;

/// The group's height: the window grows by it on a bridge instance's tab.
pub(super) const GROUP_H: i32 = 140;
/// The group's top (under the CPUs group).
const TOP: i32 = 448;

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

/// The controls under the group, with their place on a DAW instance's tab
/// (x, y, width, height); a bridge instance's tab moves them down by
/// [`GROUP_H`].
pub(super) const BELOW: [(usize, (i32, i32, i32, i32)); 5] = [
    (ID_BANNER, (16, 450, WIDTH - 32, 44)),
    (ID_RESULT, (16, 498, WIDTH - 32, 36)),
    (ID_LEGEND, (16, 548, 300, 20)),
    (ID_CANCEL, (404, 542, 90, 28)),
    (ID_APPLY, (502, 542, 122, 28)),
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
        (16, TOP, WIDTH - 32, GROUP_H - 8),
    );
    let (label_x, ctl_x, ctl_w, mark_x) = (28, 200, 380, 592);
    let rows = [
        (pure::DRIVER, ID_DRIVER),
        (pure::TO_ASIO, ID_TOASIO),
        (pure::FROM_ASIO, ID_FROMASIO),
        ("App buffer, ms", ID_APPBUF),
    ];
    for (i, (text, id)) in rows.into_iter().enumerate() {
        let y = TOP + 20 + i as i32 * 28;
        st(text, ID_BRIDGE_LABEL + i, (label_x, y + 3, 168, 20));
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
            let w = if id == ID_APPBUF { 120 } else { ctl_w };
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
        "empty: the default (the ASIO buffer + one block)",
        ID_BRIDGE_LABEL + 5,
        (ctl_x + 128, TOP + 107, 252, 20),
    );
    show(h, false);
}

/// Shows or hides the group, moves the controls under it and sizes the
/// window to match.
pub(super) fn show(h: HWND, on: bool) {
    let cmd = if on { SW_SHOW } else { SW_HIDE };
    for id in IDS {
        // SAFETY: a control of ours.
        unsafe { ShowWindow(item(h, id), cmd) };
    }
    let down = if on { GROUP_H } else { 0 };
    for (id, (x, y, w, ht)) in BELOW {
        // SAFETY: a control of ours.
        unsafe { MoveWindow(item(h, id), x, y + down, w, ht, 1) };
    }
    // SAFETY: our window; the size only (the same outer size as `create`).
    unsafe {
        SetWindowPos(
            h,
            std::ptr::null_mut(),
            0,
            0,
            WIDTH + 16,
            HEIGHT + 39 + down,
            SWP_NOMOVE | SWP_NOZORDER,
        )
    };
}

/// The driver list of `current` from the PC's drivers (`installed`, or
/// why they could not be read).
pub(super) fn drivers(
    installed: &Result<Vec<String>, String>,
    current: &str,
) -> Vec<Choice<String>> {
    match installed {
        Ok(list) => choices::drivers(list, current),
        Err(e) => vec![Choice {
            value: current.to_string(),
            text: format!("{current} (the PC's driver list is not readable: {e})"),
            greyed: false,
        }],
    }
}

/// The group's values into its controls.
pub(super) fn fill_values(h: HWND, list: &[Choice<String>], b: &dasio::Bridge) {
    fill(h, ID_DRIVER, list, choices::index_of(list, &b.driver));
    set_text(h, ID_TOASIO, &pure::map_text(&b.to_asio));
    set_text(h, ID_FROMASIO, &pure::map_text(&b.from_asio));
    set_text(h, ID_APPBUF, &pure::buffer_text(b.app_buffer_ns));
}

/// An edit's text.
fn text(h: HWND, id: usize) -> String {
    let c = item(h, id);
    // SAFETY: an edit of ours; the buffer holds the length and the 0.
    unsafe {
        let len = usize::try_from(GetWindowTextLengthW(c)).unwrap_or(0);
        let mut buf = vec![0u16; len + 1];
        let n = GetWindowTextW(c, buf.as_mut_ptr(), (len + 1) as i32);
        String::from_utf16_lossy(&buf[..usize::try_from(n).unwrap_or(0)])
    }
}

/// The group's controls as a bridge ([`pure::from_texts`]).
pub(super) fn read(h: HWND, list: &[Choice<String>]) -> Result<dasio::Bridge, String> {
    let driver = list
        .get(selected(h, ID_DRIVER))
        .map_or("", |c| c.value.as_str());
    pure::from_texts(
        driver,
        &text(h, ID_TOASIO),
        &text(h, ID_FROMASIO),
        &text(h, ID_APPBUF),
    )
}
