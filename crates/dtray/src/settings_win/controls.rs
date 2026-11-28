// SPDX-License-Identifier: MIT OR Apache-2.0

//! The window's controls: their creation and layout, the combo boxes'
//! contents and selections, the texts.

use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowTextLengthW, GetWindowTextW};

use super::*;

pub(super) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// The message font (Segoe UI on current Windows).
pub(super) fn message_font() -> HFONT {
    // SAFETY: plain data.
    let mut m: NONCLIENTMETRICSW = unsafe { std::mem::zeroed() };
    m.cbSize = std::mem::size_of::<NONCLIENTMETRICSW>() as u32;
    // SAFETY: a sized, writable structure.
    let ok =
        unsafe { SystemParametersInfoW(SPI_GETNONCLIENTMETRICS, m.cbSize, (&raw mut m).cast(), 0) };
    if ok == 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: the font the system returned.
    unsafe { CreateFontIndirectW(&m.lfMessageFont) }
}

/// A child control.
pub(super) fn control(
    parent: HWND,
    font: HFONT,
    class: &str,
    text: &str,
    style: u32,
    id: usize,
    r: (i32, i32, i32, i32),
) -> HWND {
    let (c, t) = (wide(class), wide(text));
    // SAFETY: valid strings; the id travels in the menu handle's place.
    let h = unsafe {
        CreateWindowExW(
            0,
            c.as_ptr(),
            t.as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            r.0,
            r.1,
            r.2,
            r.3,
            parent,
            id as _,
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        )
    };
    if !font.is_null() {
        // SAFETY: a control and a font handle.
        unsafe { SendMessageW(h, WM_SETFONT, font as WPARAM, 1) };
    }
    h
}

pub(super) fn item(parent: HWND, id: usize) -> HWND {
    // SAFETY: plain call.
    unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetDlgItem(parent, id as i32) }
}

/// A control's text.
pub(super) fn control_text(h: HWND, id: usize) -> String {
    let c = item(h, id);
    // SAFETY: a control of ours; the buffer holds the length and the 0.
    unsafe {
        let len = usize::try_from(GetWindowTextLengthW(c)).unwrap_or(0);
        let mut buf = vec![0u16; len + 1];
        let n = GetWindowTextW(c, buf.as_mut_ptr(), (len + 1) as i32);
        String::from_utf16_lossy(&buf[..usize::try_from(n).unwrap_or(0)])
    }
}

pub(super) fn set_text(parent: HWND, id: usize, text: &str) {
    let w = wide(text);
    // SAFETY: a valid string.
    unsafe { SetWindowTextW(item(parent, id), w.as_ptr()) };
}

/// The primary screen's work area (the screen less the taskbar), in this
/// DPI-unaware window's pixels; the client size when it is not read.
pub(super) fn work_area() -> dtray::panel::Rect {
    let mut r = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: a writable RECT.
    let ok = unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, (&raw mut r).cast(), 0) };
    if ok == 0 {
        return dtray::panel::Rect {
            left: 0,
            top: 0,
            right: OUTER.0,
            bottom: OUTER.1,
        };
    }
    dtray::panel::Rect {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    }
}

/// Builds every control (WM_CREATE).
pub(super) fn build(h: HWND, font: HFONT, tabs: &[Tab]) {
    let st =
        |text: &str, id: usize, r| control(h, font, "STATIC", text, SS_LEFT | SS_NOPREFIX, id, r);
    let group = |text: &str, r| control(h, font, "BUTTON", text, BS_GROUPBOX as u32, 0, r);
    let combo = |id: usize, r: (i32, i32, i32, i32), owner: bool| {
        let extra = if owner {
            (CBS_OWNERDRAWFIXED | CBS_HASSTRINGS) as u32
        } else {
            0
        };
        control(
            h,
            font,
            "COMBOBOX",
            "",
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL | WS_TABSTOP | extra,
            id,
            (r.0, r.1, r.2, 320),
        )
    };
    let tab = control(
        h,
        font,
        "SysTabControl32",
        "",
        WS_CLIPCHILDREN,
        ID_TAB,
        (8, 8, WIDTH - 16, 26),
    );
    for (i, t) in tabs.iter().enumerate() {
        let name = match &t.fields {
            Ok((old, _)) => old.name.clone(),
            Err(_) => t.inst.service(),
        };
        let mut w = wide(&name);
        // SAFETY: plain data.
        let mut ti: TCITEMW = unsafe { std::mem::zeroed() };
        ti.mask = TCIF_TEXT;
        ti.pszText = w.as_mut_ptr();
        // SAFETY: a tab control and a filled item.
        unsafe { SendMessageW(tab, TCM_INSERTITEMW, i, (&raw const ti) as LPARAM) };
    }
    st("", ID_STATUS, (16, 40, WIDTH - 32, 18));
    // The Audio group: a row a value, its grey hint on the right (what it
    // adds to the latency the DAW is told, dstack #512).
    let row = |y: i32, field: Field, id: usize, mark: usize, glyph: &str, hint: usize| {
        st(field.label(), 0, (LABEL_X, y + 3, LABEL_W, 20));
        combo(id, (CTL_X, y, CTL_W, 0), false);
        st(glyph, mark, (MARK_X, y + 3, 20, 20));
        st(told::hint(field), hint, (HINT_X, y + 3, HINT_W, 20));
    };
    group(
        "Audio          ●  applies at once      ↻  needs a restart",
        (16, 62, WIDTH - 32, 196),
    );
    row(80, Field::Iface, ID_IFACE, ID_RESTART, "↻", ID_HINT);
    row(
        106,
        Field::Block,
        ID_BLOCK,
        ID_RESTART + 1,
        "↻",
        ID_HINT + 1,
    );
    row(
        132,
        Field::WorkMargin,
        ID_WMARGIN,
        ID_RESTART + 12,
        "↻",
        ID_HINT + 2,
    );
    row(158, Field::RxLatency, ID_RXLAT, ID_LIVE, "●", ID_HINT + 3);
    row(
        184,
        Field::TxLatency,
        ID_TXLAT,
        ID_RESTART + 2,
        "↻",
        ID_HINT + 4,
    );
    st("Spare buffers", 0, (LABEL_X, 213, LABEL_W, 20));
    st("", ID_SPARE, (CTL_X + 4, 213, CTL_W, 20));
    st(told::SPARE_HINT, ID_HINT + 5, (HINT_X, 213, HINT_W, 20));
    st("Sample rate", 0, (LABEL_X, 235, LABEL_W, 20));
    st("", ID_RATE, (CTL_X + 4, 235, CTL_W, 20));
    // The adapters' texts are long: the open list is wider than the box.
    // SAFETY: a combo box of ours.
    unsafe { SendMessageW(item(h, ID_IFACE), CB_SETDROPPEDWIDTH, 640, 0) };
    // The lower row: the latency the DAW is told (a bridge instance's
    // group takes its place), and the channels and CPUs.
    control(
        h,
        font,
        "BUTTON",
        "Latency Ableton is told (from the values above)",
        BS_GROUPBOX as u32,
        ID_TOLD_GROUP,
        (16, LOWER_Y, HALF_W, LOWER_H),
    );
    st("", ID_TOLD, TOLD_DAW);
    let right = 16 + HALF_W + 8;
    group(
        "Channels and CPUs (another instance's CPUs are greyed)",
        (right, LOWER_Y, HALF_W, LOWER_H),
    );
    for (i, (text, id, mark, owner)) in [
        ("Inputs (Dante RX)", ID_IN, ID_RESTART + 3, false),
        ("Outputs (Dante TX)", ID_OUT, ID_RESTART + 4, false),
        ("Audio pump CPU", ID_PUMP, ID_RESTART + 5, true),
        ("Send (TX) CPU", ID_IO, ID_RESTART + 10, true),
        ("Receive (RX) CPU", ID_RXCPU, ID_RESTART + 11, true),
    ]
    .into_iter()
    .enumerate()
    {
        let y = LOWER_Y + 18 + i as i32 * 26;
        st(text, 0, (right + 12, y + 3, 150, 20));
        combo(id, (right + 166, y, 300, 0), owner);
        st("↻", mark, (right + HALF_W - 26, y + 3, 20, 20));
    }
    // A bridge instance's group, in the latency group's place (hidden on a
    // DAW instance's tab), before the controls under it in the Tab order.
    bridge::build(h, font);
    let at = |id: usize| {
        bridge::BELOW
            .iter()
            .find(|(i, _)| *i == id)
            .map_or((0, 0, 0, 0), |(_, r)| *r)
    };
    st("", ID_BANNER, at(ID_BANNER));
    st("", ID_RESULT, at(ID_RESULT));
    control(
        h,
        font,
        "BUTTON",
        "Cancel",
        BS_PUSHBUTTON as u32 | WS_TABSTOP,
        ID_CANCEL,
        at(ID_CANCEL),
    );
    control(
        h,
        font,
        "BUTTON",
        "Apply and restart",
        BS_DEFPUSHBUTTON as u32 | WS_TABSTOP,
        ID_APPLY,
        at(ID_APPLY),
    );
}

/// Fills `id`'s combo box with `list`, `selected` chosen; the greyed
/// entries carry item data 1 (the owner-drawn CPU lists).
pub(super) fn fill<T>(h: HWND, id: usize, list: &[Choice<T>], selected: usize) {
    let c = item(h, id);
    // SAFETY: a combo box of ours; the strings outlive each call.
    unsafe {
        SendMessageW(c, CB_RESETCONTENT, 0, 0);
        for (i, ch) in list.iter().enumerate() {
            let w = wide(&ch.text);
            SendMessageW(c, CB_ADDSTRING, 0, w.as_ptr() as LPARAM);
            SendMessageW(c, CB_SETITEMDATA, i, isize::from(ch.greyed));
        }
        SendMessageW(c, CB_SETCURSEL, selected, 0);
    }
}

/// The selected index of `id`'s combo box (0 when none).
pub(super) fn selected(h: HWND, id: usize) -> usize {
    // SAFETY: a combo box of ours.
    let i = unsafe { SendMessageW(item(h, id), CB_GETCURSEL, 0, 0) };
    usize::try_from(i).unwrap_or(0)
}

pub(super) fn enable(h: HWND, id: usize, on: bool) {
    // SAFETY: a control of ours.
    unsafe { EnableWindow(item(h, id), i32::from(on)) };
}
