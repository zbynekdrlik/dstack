// SPDX-License-Identifier: MIT OR Apache-2.0

//! The window's controls: their creation and layout, the combo boxes'
//! contents and selections, the texts.

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

pub(super) fn set_text(parent: HWND, id: usize, text: &str) {
    let w = wide(text);
    // SAFETY: a valid string.
    unsafe { SetWindowTextW(item(parent, id), w.as_ptr()) };
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
    st("", ID_STATUS, (16, 42, WIDTH - 32, 20));
    let label_x = 28;
    let ctl_x = 200;
    let ctl_w = 380;
    let mark_x = 592;
    let row = |y: i32, text: &str, id: usize, mark: usize, glyph: &str| {
        st(text, 0, (label_x, y + 3, 168, 20));
        combo(id, (ctl_x, y, ctl_w, 0), false);
        if !glyph.is_empty() {
            st(glyph, mark, (mark_x, y + 3, 20, 20));
        }
    };
    group("Network", (16, 68, WIDTH - 32, 52));
    row(88, "Ethernet interface", ID_IFACE, ID_RESTART, "↻");
    group("Audio", (16, 126, WIDTH - 32, 132));
    row(146, "Buffer (block)", ID_BLOCK, ID_RESTART + 1, "↻");
    row(174, "Dante latency (RX)", ID_RXLAT, ID_LIVE, "●");
    row(202, "TX latency", ID_TXLAT, ID_RESTART + 2, "↻");
    st("Sample rate", 0, (label_x, 233, 168, 20));
    st("", ID_RATE, (ctl_x + 4, 233, ctl_w, 20));
    group("Channels", (16, 264, WIDTH - 32, 80));
    row(284, "Inputs (Dante RX)", ID_IN, ID_RESTART + 3, "↻");
    row(312, "Outputs (Dante TX)", ID_OUT, ID_RESTART + 4, "↻");
    group("CPUs", (16, 350, WIDTH - 32, 92));
    let cw = 180;
    for (i, (text, id)) in [
        ("Audio pump", ID_PUMP),
        ("Send (TX)", ID_IO),
        ("Receive (RX)", ID_RXCPU),
    ]
    .into_iter()
    .enumerate()
    {
        let x = 28 + i as i32 * (cw + 16);
        st(text, 0, (x, 368, cw, 18));
        combo(id, (x, 388, cw, 0), true);
    }
    st(
        "Busy CPUs of the other instance are greyed out in the lists.",
        0,
        (28, 418, 540, 18),
    );
    st("↻", ID_RESTART + 5, (mark_x, 418, 20, 20));
    st("", ID_BANNER, (16, 450, WIDTH - 32, 44));
    st("", ID_RESULT, (16, 498, WIDTH - 32, 36));
    st(
        "●  applies at once      ↻  needs a restart",
        0,
        (16, 548, 300, 20),
    );
    control(
        h,
        font,
        "BUTTON",
        "Cancel",
        BS_PUSHBUTTON as u32 | WS_TABSTOP,
        ID_CANCEL,
        (404, 542, 90, 28),
    );
    control(
        h,
        font,
        "BUTTON",
        "Apply and restart",
        BS_DEFPUSHBUTTON as u32 | WS_TABSTOP,
        ID_APPLY,
        (502, 542, 122, 28),
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
