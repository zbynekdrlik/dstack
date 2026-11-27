// SPDX-License-Identifier: MIT OR Apache-2.0

//! dtray's panel on Windows (dstack #507): the window a left click on the
//! tray icon opens (the right click keeps the menu). Thin Win32 glue over
//! [`dtray::panel`], which decides where the window goes ([`place`]: next
//! to the icon, inside the work area of the icon's monitor, a scroll bar
//! when the content is taller) and what each row shows ([`rows`]: a
//! device's name, colour and line; an app's output, its warning and its
//! one Output picker). A pick sends `app output <n> <name>` to the device
//! ([`output::request`], as the menu's Output items do); the app saves it.
//!
//! The window is per-monitor DPI aware (v2) on its own: it is created, and
//! every call that reads or sets its geometry runs, under that thread
//! context ([`Pmv2`]), while the rest of dtray (the icon, the menu, the
//! dasio settings window with its fixed layout) stays as it was. It is a
//! flyout: it hides when it loses the focus, on Esc, and on a second click.
//! On the thread of the tray's message loop; outside the mutation gate,
//! as `win.rs`.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use dtray::discover::{Local, Reading};
use dtray::model::{Colour, View};
use dtray::panel::{self, Placement, Rect, Row, place, rows, scaled, scroll_to};
use dtray::{output, probe};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    COLOR_WINDOW, COLOR_WINDOWTEXT, CreateFontIndirectW, DT_CALCRECT, DT_NOPREFIX, DT_WORDBREAK,
    DeleteObject, DrawTextW, GetDC, GetMonitorInfoW, GetSysColor, GetSysColorBrush, HDC, HFONT,
    HGDIOBJ, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect, ReleaseDC, SelectObject,
    SetBkColor, SetTextColor,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::SetScrollInfo;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
    GetSystemMetricsForDpi, SetThreadDpiAwarenessContext, SystemParametersInfoForDpi,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CB_ADDSTRING, CB_GETCURSEL, CB_GETDROPPEDSTATE, CB_SETCURSEL, CBN_SELENDOK, CBS_DROPDOWNLIST,
    CBS_HASSTRINGS, CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetCursorPos,
    GetDlgCtrlID, GetScrollInfo, GetWindowRect, HWND_TOPMOST, IDC_ARROW, IsChild, IsDialogMessageW,
    IsWindowVisible, LoadCursorW, MSG, MoveWindow, NONCLIENTMETRICSW, RegisterClassW, SB_BOTTOM,
    SB_LINEDOWN, SB_LINEUP, SB_PAGEDOWN, SB_PAGEUP, SB_THUMBTRACK, SB_TOP, SB_VERT, SCROLLINFO,
    SIF_ALL, SIF_PAGE, SIF_POS, SIF_RANGE, SM_CXVSCROLL, SPI_GETNONCLIENTMETRICS, SW_HIDE, SW_SHOW,
    SWP_NOACTIVATE, SendMessageW, SetForegroundWindow, SetWindowPos, SetWindowTextW, ShowWindow,
    WA_INACTIVE, WM_ACTIVATE, WM_COMMAND, WM_CTLCOLORSTATIC, WM_DPICHANGED, WM_MOUSEWHEEL,
    WM_SETFONT, WM_VSCROLL, WNDCLASSW, WS_BORDER, WS_CHILD, WS_CLIPCHILDREN, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

/// The window's class.
const CLASS: &str = "dtray-panel";
/// Esc in the window (`IsDialogMessageW`'s `IDCANCEL`).
const IDCANCEL_ID: usize = 2;
/// A static's styles (`SS_LEFT`, `SS_NOPREFIX`: an `&` shows as itself).
const SS_NOPREFIX: u32 = 0x80;

/// The content's width, DIPs.
const WIDTH: i32 = 460;
/// The margin around the content and between a row's parts, DIPs.
const MARGIN: i32 = 12;
/// Between two rows, DIPs.
const ROW_GAP: i32 = 14;
/// Between the panel and the icon, DIPs.
const GAP: i32 = 8;
/// The picker's label's width, DIPs.
const LABEL: i32 = 56;
/// A wheel notch's scroll, DIPs.
const WHEEL: i32 = 48;
/// A click on the icon that comes this soon after the panel hid itself
/// (that click took its focus) only closes it.
const REOPEN: Duration = Duration::from_millis(300);

// Control ids: the header, then each row's from ROW + 10 x its index.
const ID_HEADER: usize = 100;
const ROW: usize = 1000;
const TITLE: usize = 0;
const LINE: usize = 1;
const PLAYS: usize = 2;
const WARN: usize = 3;
const LABEL_ID: usize = 4;
const PICK: usize = 5;
/// The ids a row takes.
const ROW_IDS: usize = 10;

const HEADER: &str = "Each device on this PC. Choose where an app plays with its Output; \
                      right-click the icon for latency, switches and settings.";

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

struct State {
    hwnd: HWND,
    font: HFONT,
    bold: HFONT,
    dpi: u32,
    /// The rows shown and the shape their controls were built for.
    rows: Vec<Row>,
    shape: Vec<(String, Option<Vec<String>>)>,
    /// The devices of the rows (a pick's port).
    devices: Vec<Local>,
    /// The icon's rectangle the panel was placed at (physical pixels).
    anchor: Rect,
    /// The content's height and the scroll position, physical pixels.
    content: i32,
    pos: i32,
    /// When the panel last hid itself on losing the focus.
    hidden_at: Option<Instant>,
}

/// The thread's DPI awareness set to per-monitor v2 while it lives, the
/// one before put back after (dtray's other windows are DPI unaware).
struct Pmv2(DPI_AWARENESS_CONTEXT);

impl Pmv2 {
    fn enter() -> Pmv2 {
        // SAFETY: plain call; the previous context is put back on drop.
        Pmv2(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
    }
}

impl Drop for Pmv2 {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the context SetThreadDpiAwarenessContext returned.
            unsafe { SetThreadDpiAwarenessContext(self.0) };
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn rect_of(r: RECT) -> Rect {
    Rect {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    }
}

/// The panel's handle when it exists.
fn hwnd() -> Option<HWND> {
    STATE.with(|s| s.try_borrow().ok()?.as_ref().map(|st| st.hwnd))
}

fn visible() -> bool {
    // SAFETY: our own window.
    hwnd().is_some_and(|h| unsafe { IsWindowVisible(h) } != 0)
}

/// A left click on the icon: opens the panel next to `icon` (the icon's
/// rectangle as the tray reads it, `None` unknown), or closes it when it
/// is open. `devices`, `readings` and `view` are the tray's last tick.
pub fn toggle(
    icon: impl FnOnce() -> Option<(i32, i32, u32, u32)>,
    devices: &[Local],
    readings: &[Reading],
    view: &View,
) -> Result<(), String> {
    let _dpi = Pmv2::enter();
    if visible() {
        hide();
        return Ok(());
    }
    let just_hid = STATE.with(|s| {
        s.try_borrow().ok().and_then(|s| {
            s.as_ref()
                .and_then(|st| st.hidden_at)
                .map(|t| t.elapsed() < REOPEN)
        })
    });
    if just_hid == Some(true) {
        return Ok(());
    }
    let anchor = anchor_of(icon());
    if hwnd().is_none() {
        create(anchor)?;
    }
    with_state(|st| {
        st.anchor = anchor;
        st.pos = 0;
        st.hidden_at = None;
        st.devices = devices.to_vec();
        update(st, rows(readings, view));
    });
    if let Some(h) = hwnd() {
        // SAFETY: our own window.
        unsafe {
            ShowWindow(h, SW_SHOW);
            SetForegroundWindow(h);
        }
    }
    Ok(())
}

/// The tray's tick: the rows again while the panel shows.
pub fn feed(devices: &[Local], readings: &[Reading], view: &View) {
    if !visible() {
        return;
    }
    let _dpi = Pmv2::enter();
    with_state(|st| {
        st.devices = devices.to_vec();
        update(st, rows(readings, view));
    });
}

/// The panel's keyboard handling (Tab, Esc): `true` when the message was
/// its own and is handled.
///
/// The wheel over the panel or one of its controls scrolls the panel: a
/// closed Output picker never takes it (Windows sends the wheel to the
/// control under the cursor, and the picker would change its item). An
/// open list (not a child of the panel) still scrolls itself.
pub fn dialog_message(msg: &MSG) -> bool {
    let Some(h) = hwnd().filter(|_| visible()) else {
        return false;
    };
    // SAFETY: plain call on our window.
    let ours = msg.hwnd == h || unsafe { IsChild(h, msg.hwnd) } != 0;
    if msg.message == WM_MOUSEWHEEL && ours {
        let delta = i32::from((msg.wParam >> 16) as u16 as i16);
        let _dpi = Pmv2::enter();
        with_state(|st| wheel(st, delta));
        return true;
    }
    // SAFETY: our window and a message GetMessageW filled.
    unsafe { IsDialogMessageW(h, msg) != 0 }
}

/// A wheel turn of `delta` (120 a notch) over the panel.
fn wheel(st: &mut State, delta: i32) {
    let by = -delta * scaled(WHEEL, st.dpi) / 120;
    scroll(st, by);
}

/// The icon's rectangle in physical pixels (`icon`: its position and size
/// as tray-icon reads it, under this thread's per-monitor context), or the
/// cursor's point when it is not known or the cursor is not on it (a
/// rectangle the shell gave in another DPI's pixels).
fn anchor_of(icon: Option<(i32, i32, u32, u32)>) -> Rect {
    let mut p = POINT { x: 0, y: 0 };
    // SAFETY: a valid POINT.
    let have = unsafe { GetCursorPos(&mut p) } != 0;
    let point = Rect {
        left: p.x,
        top: p.y,
        right: p.x + 1,
        bottom: p.y + 1,
    };
    match icon {
        Some((x, y, w, h)) => {
            let r = Rect {
                left: x,
                top: y,
                right: x.saturating_add(i32::try_from(w).unwrap_or(0)),
                bottom: y.saturating_add(i32::try_from(h).unwrap_or(0)),
            };
            let on = p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom;
            if on || !have { r } else { point }
        }
        None => point,
    }
}

fn create(anchor: Rect) -> Result<(), String> {
    // SAFETY: this program's module.
    let hinst = unsafe { GetModuleHandleW(std::ptr::null()) };
    let class = wide(CLASS);
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: hinst,
        // SAFETY: a system cursor.
        hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
        hbrBackground: (COLOR_WINDOW + 1) as usize as _,
        lpszClassName: class.as_ptr(),
        // SAFETY: the rest is plain data.
        ..unsafe { std::mem::zeroed() }
    };
    // SAFETY: a filled class; registering twice fails harmlessly.
    unsafe { RegisterClassW(&wc) };
    let title = wide("dtray");
    // SAFETY: the class registered above; hidden until placed.
    let h = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class.as_ptr(),
            title.as_ptr(),
            WS_POPUP | WS_BORDER | WS_VSCROLL | WS_CLIPCHILDREN,
            anchor.left,
            anchor.top,
            100,
            100,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        )
    };
    if h.is_null() {
        return Err("the panel window was not created".into());
    }
    // SAFETY: our window, created under the per-monitor context.
    let dpi = unsafe { GetDpiForWindow(h) }.max(96);
    let (font, bold) = fonts(dpi);
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            hwnd: h,
            font,
            bold,
            dpi,
            rows: Vec::new(),
            shape: Vec::new(),
            devices: Vec::new(),
            anchor,
            content: 0,
            pos: 0,
            hidden_at: None,
        })
    });
    let header = child(h, "STATIC", HEADER, SS_NOPREFIX, ID_HEADER);
    set_font(header, font);
    Ok(())
}

/// The message font at `dpi`, and its bold form (null when not made: the
/// system font then).
fn fonts(dpi: u32) -> (HFONT, HFONT) {
    let mut ncm: NONCLIENTMETRICSW = {
        // SAFETY: plain data.
        unsafe { std::mem::zeroed() }
    };
    ncm.cbSize = std::mem::size_of::<NONCLIENTMETRICSW>() as u32;
    // SAFETY: a sized structure for this action.
    let ok = unsafe {
        SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS,
            ncm.cbSize,
            (&mut ncm as *mut NONCLIENTMETRICSW).cast(),
            0,
            dpi,
        )
    } != 0;
    if !ok {
        return (std::ptr::null_mut(), std::ptr::null_mut());
    }
    let mut lf = ncm.lfMessageFont;
    // SAFETY: a filled LOGFONTW.
    let font = unsafe { CreateFontIndirectW(&lf) };
    lf.lfWeight = 700;
    // SAFETY: the same, bold.
    let bold = unsafe { CreateFontIndirectW(&lf) };
    (font, bold)
}

fn child(parent: HWND, class: &str, text: &str, style: u32, id: usize) -> HWND {
    let (c, t) = (wide(class), wide(text));
    // SAFETY: a child of our window; the id is its menu handle by Win32's
    // convention for children.
    unsafe {
        CreateWindowExW(
            0,
            c.as_ptr(),
            t.as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            0,
            0,
            10,
            10,
            parent,
            id as _,
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        )
    }
}

fn item(h: HWND, id: usize) -> HWND {
    // SAFETY: plain call on our window.
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::GetDlgItem(h, i32::try_from(id).unwrap_or(0))
    }
}

fn set_font(c: HWND, font: HFONT) {
    if !c.is_null() && !font.is_null() {
        // SAFETY: our control and font.
        unsafe { SendMessageW(c, WM_SETFONT, font as WPARAM, 1) };
    }
}

fn set_text(c: HWND, text: &str) {
    let t = wide(text);
    // SAFETY: our control, a NUL-terminated string.
    unsafe { SetWindowTextW(c, t.as_ptr()) };
}

fn title_of(r: &Row) -> String {
    format!(
        "\u{25CF} {}  \u{2014}  {}",
        r.name,
        panel::colour_word(r.colour)
    )
}

/// The panel set to `new`: its controls built again when the shape
/// changed, else their texts set; then laid out and placed.
fn update(st: &mut State, new: Vec<Row>) {
    let h = st.hwnd;
    let shape = panel::shape(&new);
    if shape != st.shape {
        for i in 0..st.rows.len() {
            for k in 0..ROW_IDS {
                let c = item(h, ROW + i * ROW_IDS + k);
                if !c.is_null() {
                    // SAFETY: our child.
                    unsafe { DestroyWindow(c) };
                }
            }
        }
        for (i, r) in new.iter().enumerate() {
            let base = ROW + i * ROW_IDS;
            set_font(
                child(h, "STATIC", &title_of(r), SS_NOPREFIX, base + TITLE),
                st.bold,
            );
            set_font(
                child(h, "STATIC", &r.line, SS_NOPREFIX, base + LINE),
                st.font,
            );
            if let Some(o) = &r.output {
                set_font(
                    child(h, "STATIC", &o.plays, SS_NOPREFIX, base + PLAYS),
                    st.bold,
                );
                let w = o.warning.as_deref().unwrap_or("");
                set_font(child(h, "STATIC", w, SS_NOPREFIX, base + WARN), st.bold);
                set_font(
                    child(h, "STATIC", "Output:", SS_NOPREFIX, base + LABEL_ID),
                    st.font,
                );
                let pick = child(
                    h,
                    "COMBOBOX",
                    "",
                    (CBS_DROPDOWNLIST | CBS_HASSTRINGS) as u32 | WS_TABSTOP | WS_VSCROLL,
                    base + PICK,
                );
                set_font(pick, st.font);
                for c in &o.choices {
                    let t = wide(&c.label);
                    // SAFETY: our combobox, a NUL-terminated string.
                    unsafe { SendMessageW(pick, CB_ADDSTRING, 0, t.as_ptr() as LPARAM) };
                }
            }
        }
        st.shape = shape;
    } else {
        for (i, r) in new.iter().enumerate() {
            let base = ROW + i * ROW_IDS;
            set_text(item(h, base + TITLE), &title_of(r));
            set_text(item(h, base + LINE), &r.line);
            if let Some(o) = &r.output {
                set_text(item(h, base + PLAYS), &o.plays);
                set_text(item(h, base + WARN), o.warning.as_deref().unwrap_or(""));
            }
        }
    }
    for (i, r) in new.iter().enumerate() {
        let Some(o) = &r.output else { continue };
        let pick = item(h, ROW + i * ROW_IDS + PICK);
        // SAFETY: our combobox.
        let dropped = unsafe { SendMessageW(pick, CB_GETDROPPEDSTATE, 0, 0) } != 0;
        if !dropped {
            let sel = o.selected.unwrap_or(usize::MAX);
            // SAFETY: our combobox; usize::MAX is -1, no selection.
            unsafe { SendMessageW(pick, CB_SETCURSEL, sel, 0) };
        }
    }
    st.rows = new;
    layout(st);
}

/// The panel at a new `dpi`: its fonts made again and its controls built
/// again in them (`update` destroys the old rows' controls by `st.rows`'
/// count), then laid out.
fn rescale(st: &mut State, dpi: u32) {
    let dpi = dpi.max(96);
    if dpi == st.dpi {
        return;
    }
    for f in [st.font, st.bold] {
        if !f.is_null() {
            // SAFETY: a font this panel made.
            unsafe { DeleteObject(f as HGDIOBJ) };
        }
    }
    st.dpi = dpi;
    (st.font, st.bold) = fonts(dpi);
    st.shape.clear();
    set_font(item(st.hwnd, ID_HEADER), st.font);
    let rows = st.rows.clone();
    update(st, rows);
}

/// The text's height at `width` pixels in `font`, wrapped.
fn text_height(h: HWND, font: HFONT, text: &str, width: i32) -> i32 {
    if text.is_empty() {
        return 0;
    }
    let t: Vec<u16> = text.encode_utf16().collect();
    let mut r = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: 0,
    };
    // SAFETY: our window's DC, released below; the font is ours.
    unsafe {
        let dc = GetDC(h);
        let old = SelectObject(dc, font as HGDIOBJ);
        DrawTextW(
            dc,
            t.as_ptr(),
            i32::try_from(t.len()).unwrap_or(i32::MAX),
            &mut r,
            DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
        );
        SelectObject(dc, old);
        ReleaseDC(h, dc);
    }
    r.bottom - r.top
}

/// Every control moved to its place at the scroll position, the window
/// placed next to the icon inside the work area and the scroll bar set.
fn layout(st: &mut State) {
    let h = st.hwnd;
    let d = |dips: i32| scaled(dips, st.dpi);
    // SAFETY: plain call.
    let bar = unsafe { GetSystemMetricsForDpi(SM_CXVSCROLL, st.dpi) };
    let inner = d(WIDTH) - 2 * d(MARGIN);
    // The content, laid out from the top (y) at scroll 0.
    let mut places: Vec<Place> = Vec::new();
    let mut y = d(MARGIN);
    let header = item(h, ID_HEADER);
    let hh = text_height(h, st.font, HEADER, inner);
    put(&mut places, header, d(MARGIN), inner, hh, &mut y);
    for (i, r) in st.rows.iter().enumerate() {
        let base = ROW + i * ROW_IDS;
        y += d(ROW_GAP);
        let th = text_height(h, st.bold, &title_of(r), inner);
        put(
            &mut places,
            item(h, base + TITLE),
            d(MARGIN),
            inner,
            th,
            &mut y,
        );
        let lh = text_height(h, st.font, &r.line, inner);
        put(
            &mut places,
            item(h, base + LINE),
            d(MARGIN),
            inner,
            lh,
            &mut y,
        );
        if let Some(o) = &r.output {
            y += d(4);
            let ph = text_height(h, st.bold, &o.plays, inner);
            put(
                &mut places,
                item(h, base + PLAYS),
                d(MARGIN),
                inner,
                ph,
                &mut y,
            );
            let w = o.warning.as_deref().unwrap_or("");
            let wh = text_height(h, st.bold, w, inner);
            put(
                &mut places,
                item(h, base + WARN),
                d(MARGIN),
                inner,
                wh,
                &mut y,
            );
            y += d(4);
            let pick = item(h, base + PICK);
            let mut pr = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            // SAFETY: our combobox; its window is its closed field.
            unsafe { GetWindowRect(pick, &mut pr) };
            let field = (pr.bottom - pr.top).max(d(22));
            let label_h = text_height(h, st.font, "Output:", d(LABEL));
            places.push((
                item(h, base + LABEL_ID),
                d(MARGIN),
                y + (field - label_h) / 2,
                d(LABEL),
                label_h,
            ));
            // The list drops below the field: its height is the field's
            // and the list's (8 items).
            places.push((
                pick,
                d(MARGIN) + d(LABEL),
                y,
                inner - d(LABEL),
                field + d(8 * 20),
            ));
            y += field;
        }
    }
    y += d(MARGIN);
    st.content = y;
    // The window: the content's width and height plus the scroll bar's
    // room and the borders.
    let border = 2;
    let anchor = st.anchor;
    let work = work_area(anchor);
    let Placement { rect, .. } = place(work, anchor, d(WIDTH) + bar + border, y + border, d(GAP));
    // SAFETY: our window, physical pixels (per-monitor context).
    unsafe {
        SetWindowPos(
            h,
            HWND_TOPMOST,
            rect.left,
            rect.top,
            rect.width(),
            rect.height(),
            SWP_NOACTIVATE,
        )
    };
    // Moved onto a monitor of another DPI: laid out again in its pixels
    // (once: the next placement is on the same monitor).
    // SAFETY: our window.
    let now = unsafe { GetDpiForWindow(h) };
    if now != 0 && now.max(96) != st.dpi {
        rescale(st, now);
        return;
    }
    let mut client = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: our window.
    unsafe { GetClientRect(h, &mut client) };
    let page = client.bottom - client.top;
    st.pos = scroll_to(st.pos, 0, y, page);
    let si = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_ALL,
        nMin: 0,
        nMax: (y - 1).max(0),
        nPage: u32::try_from(page.max(0)).unwrap_or(0),
        nPos: st.pos,
        nTrackPos: 0,
    };
    // SAFETY: our window, a filled structure.
    unsafe { SetScrollInfo(h, SB_VERT, &si, 1) };
    for (c, x, cy, w, ch) in places {
        if !c.is_null() {
            // SAFETY: our child.
            unsafe { MoveWindow(c, x, cy - st.pos, w, ch, 1) };
        }
    }
}

/// A control's place at `y`, the next one under it.
type Place = (HWND, i32, i32, i32, i32);

fn put(places: &mut Vec<Place>, c: HWND, x: i32, w: i32, height: i32, y: &mut i32) {
    places.push((c, x, *y, w, height));
    *y += height;
}

/// The work area of the monitor holding `anchor` (the nearest one).
fn work_area(anchor: Rect) -> Rect {
    let r = RECT {
        left: anchor.left,
        top: anchor.top,
        right: anchor.right,
        bottom: anchor.bottom,
    };
    // SAFETY: a valid RECT.
    let mon = unsafe { MonitorFromRect(&r, MONITOR_DEFAULTTONEAREST) };
    let mut mi: MONITORINFO = {
        // SAFETY: plain data.
        unsafe { std::mem::zeroed() }
    };
    mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    // SAFETY: a monitor handle and a sized structure.
    if unsafe { GetMonitorInfoW(mon, &mut mi) } == 0 {
        return Rect {
            left: anchor.left - 400,
            top: anchor.top - 400,
            right: anchor.right,
            bottom: anchor.bottom,
        };
    }
    rect_of(mi.rcWork)
}

fn hide() {
    with_state(|st| {
        st.hidden_at = Some(Instant::now());
        // SAFETY: our window.
        unsafe { ShowWindow(st.hwnd, SW_HIDE) };
    });
}

/// Runs `f` on the state when it is free (a message sent while another
/// handler holds it is left to the default handling).
fn with_state(f: impl FnOnce(&mut State)) -> bool {
    STATE.with(|s| match s.try_borrow_mut() {
        Ok(mut s) => match s.as_mut() {
            Some(st) => {
                f(st);
                true
            }
            None => false,
        },
        Err(_) => false,
    })
}

/// A pick on row `row`'s Output: the app asked to play there, unless it
/// already does.
fn picked(st: &State, row: usize) -> Option<String> {
    let r = st.rows.get(row)?;
    let o = r.output.as_ref()?;
    let pick = item(st.hwnd, ROW + row * ROW_IDS + PICK);
    // SAFETY: our combobox.
    let sel = unsafe { SendMessageW(pick, CB_GETCURSEL, 0, 0) };
    let i = usize::try_from(sel).ok()?;
    if o.selected == Some(i) {
        return None;
    }
    let choice = o.choices.get(i)?;
    let port = st.devices.get(row)?.port;
    probe::ask(port, &output::request(choice.index, &choice.name))
        .err()
        .map(|e| format!("dtray: the output was not chosen: {e}"))
}

/// The scroll moved to `pos` (clamped), the controls with it.
fn scroll(st: &mut State, by: i32) {
    let mut si = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_PAGE | SIF_POS | SIF_RANGE,
        nMin: 0,
        nMax: 0,
        nPage: 0,
        nPos: 0,
        nTrackPos: 0,
    };
    // SAFETY: our window, a sized structure.
    unsafe { GetScrollInfo(st.hwnd, SB_VERT, &mut si) };
    let page = i32::try_from(si.nPage).unwrap_or(i32::MAX);
    let to = scroll_to(st.pos, by, st.content, page);
    if to != st.pos {
        st.pos = to;
        layout(st);
    }
}

fn colour_ref(c: Colour) -> u32 {
    // COLORREF is 0x00BBGGRR.
    match c {
        Colour::Grey => 0x0080_8080,
        Colour::Green => 0x0037_7F1A,
        Colour::Orange => 0x0000_6CC4,
        Colour::Red => 0x0000_00C0,
    }
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ACTIVATE if (wp & 0xffff) as u32 == WA_INACTIVE => {
            hide();
            0
        }
        WM_CTLCOLORSTATIC => {
            // SAFETY: plain call on the child the system names.
            let id = usize::try_from(unsafe { GetDlgCtrlID(lp as HWND) }).unwrap_or(0);
            let colour = STATE.with(|s| {
                let s = s.try_borrow().ok()?;
                let st = s.as_ref()?;
                let row = id.checked_sub(ROW)? / ROW_IDS;
                match id.checked_sub(ROW)? % ROW_IDS {
                    TITLE => st.rows.get(row).map(|r| colour_ref(r.colour)),
                    WARN => Some(colour_ref(Colour::Red)),
                    _ => None,
                }
            });
            let dc = wp as HDC;
            // SAFETY: the DC the system gave for this child.
            unsafe {
                SetTextColor(dc, colour.unwrap_or(GetSysColor(COLOR_WINDOWTEXT)));
                SetBkColor(dc, GetSysColor(COLOR_WINDOW));
                GetSysColorBrush(COLOR_WINDOW) as LRESULT
            }
        }
        WM_COMMAND => {
            let id = wp & 0xffff;
            let code = (wp >> 16) as u32;
            if id == IDCANCEL_ID && code == 0 {
                hide();
            } else if code == CBN_SELENDOK && id >= ROW && (id - ROW) % ROW_IDS == PICK {
                // Only a pick the user accepted (a click on an item, Enter
                // in the open list), never each item the arrow keys pass in
                // the open list (CBN_SELCHANGE): that would move the app's
                // output at every step.
                let row = (id - ROW) / ROW_IDS;
                let mut failed = None;
                with_state(|st| failed = picked(st, row));
                if let Some(err) = failed {
                    crate::win::message(&err);
                }
            }
            0
        }
        WM_VSCROLL => {
            let code = (wp & 0xffff) as i32;
            let track = (wp >> 16) as i32;
            with_state(|st| {
                let line = scaled(20, st.dpi);
                let page = scaled(200, st.dpi);
                let by = match code {
                    SB_LINEUP => -line,
                    SB_LINEDOWN => line,
                    SB_PAGEUP => -page,
                    SB_PAGEDOWN => page,
                    SB_TOP => -st.content,
                    SB_BOTTOM => st.content,
                    SB_THUMBTRACK => track - st.pos,
                    _ => 0,
                };
                scroll(st, by);
            });
            0
        }
        WM_MOUSEWHEEL => {
            let delta = i32::from((wp >> 16) as u16 as i16);
            with_state(|st| wheel(st, delta));
            0
        }
        WM_DPICHANGED => {
            // Sent while `layout` moves the window onto another monitor
            // (the state is then held, and `layout` checks the DPI itself).
            let dpi = (wp & 0xffff) as u32;
            with_state(|st| rescale(st, dpi));
            0
        }
        // SAFETY: the default handling.
        _ => unsafe { DefWindowProcW(h, msg, wp, lp) },
    }
}
