// SPDX-License-Identifier: MIT OR Apache-2.0

//! The colours: the owner-drawn CPU lists (another instance's CPUs grey)
//! and the markers and banner. Stateless: they may run while the window's
//! state is borrowed.

use windows_sys::Win32::Graphics::Gdi::{COLOR_BTNFACE, DrawFocusRect};
use windows_sys::Win32::UI::Controls::{ODS_DISABLED, ODS_FOCUS};

use super::*;

thread_local! {
    static WARN_BRUSH: Cell<HBRUSH> = const { Cell::new(std::ptr::null_mut()) };
}

pub(super) const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

pub(super) const WARN: COLORREF = rgb(0xb4, 0x53, 0x09);
pub(super) const WARN_BG: COLORREF = rgb(0xff, 0xf7, 0xe6);
pub(super) const OK: COLORREF = rgb(0x15, 0x80, 0x3d);

/// An owner-drawn CPU list's entry: grey for another instance's CPU
/// (item data 1). Stateless: it may run while the state is borrowed.
pub(super) fn draw_item(d: &DRAWITEMSTRUCT) {
    let hdc: HDC = d.hDC;
    let disabled = d.itemState & ODS_DISABLED != 0;
    let selected = d.itemState & ODS_SELECTED != 0 && !disabled;
    // SAFETY: the system colours and the DC the system gave.
    unsafe {
        let bg = if selected {
            COLOR_HIGHLIGHT
        } else if disabled {
            COLOR_BTNFACE
        } else {
            COLOR_WINDOW
        };
        FillRect(hdc, &d.rcItem, GetSysColorBrush(bg));
        if d.itemID == u32::MAX {
            return;
        }
        let combo = d.hwndItem;
        let len = SendMessageW(combo, CB_GETLBTEXTLEN, d.itemID as usize, 0);
        let mut buf = vec![0u16; usize::try_from(len).unwrap_or(0) + 1];
        SendMessageW(
            combo,
            CB_GETLBTEXT,
            d.itemID as usize,
            buf.as_mut_ptr() as LPARAM,
        );
        let greyed = SendMessageW(combo, CB_GETITEMDATA, d.itemID as usize, 0) == 1;
        let fg = if selected {
            COLOR_HIGHLIGHTTEXT
        } else if greyed || disabled {
            COLOR_GRAYTEXT
        } else {
            COLOR_WINDOWTEXT
        };
        SetTextColor(hdc, GetSysColor(fg));
        SetBkMode(hdc, TRANSPARENT as i32);
        let mut r: RECT = d.rcItem;
        r.left += 4;
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        DrawTextW(
            hdc,
            buf.as_ptr(),
            n as i32,
            &mut r,
            DT_SINGLELINE | DT_VCENTER,
        );
        if d.itemState & ODS_FOCUS != 0 {
            DrawFocusRect(hdc, &d.rcItem);
        }
    }
}

/// The colours of the markers and the banner (stateless, as
/// [`draw_item`]).
pub(super) fn static_colour(hdc: HDC, id: usize) -> HBRUSH {
    // SAFETY: the DC the system gave; system brushes and our one brush.
    unsafe {
        if id == ID_BANNER {
            SetTextColor(hdc, WARN);
            SetBkColor(hdc, WARN_BG);
            let b = WARN_BRUSH.with(|c| {
                if c.get().is_null() {
                    c.set(CreateSolidBrush(WARN_BG));
                }
                c.get()
            });
            return b;
        }
        if id == ID_LIVE {
            SetTextColor(hdc, OK);
        } else if (ID_RESTART..ID_RESTART + 10).contains(&id) {
            SetTextColor(hdc, WARN);
        }
        SetBkColor(hdc, GetSysColor(COLOR_WINDOW));
        GetSysColorBrush(COLOR_WINDOW)
    }
}
