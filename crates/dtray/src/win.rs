// SPDX-License-Identifier: MIT OR Apache-2.0

//! The tray on Windows: thin glue over the library. A thread timer wakes
//! the message loop once a second; each tick refreshes discovery, reads
//! every local device and shows [`Tracker`]'s view. One tray a desktop
//! session (the named mutex [`MUTEX`]). The menu: a line per device, Copy
//! status (every device's JSON to the clipboard), Exit (the tray only; the
//! devices are other processes and keep running).

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, GlobalFree};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MB_ICONERROR, MB_OK, MSG, MessageBoxW, SetTimer,
    TranslateMessage, WM_TIMER,
};

use dtray::discover::{Reading, Scanner};
use dtray::icon;
use dtray::model::{self, Colour, Tracker, View};

/// The single-instance mutex of a desktop session.
const MUTEX: &str = r"Local\dtray";
/// The timer's period, ms.
const TICK_MS: u32 = 1000;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// An error box (the tray has no console).
pub fn message(text: &str) {
    let (t, c) = (wide(text), wide("dtray"));
    // SAFETY: valid NUL-terminated strings, no owner window.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            t.as_ptr(),
            c.as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
}

/// The menu's fixed items.
struct Fixed {
    version: MenuItem,
    copy: MenuItem,
    exit: MenuItem,
}

/// Runs the tray until Exit; a second start in the session leaves at once.
pub fn run() -> Result<(), String> {
    let name = wide(MUTEX);
    // SAFETY: a valid name; the handle is held until the process ends.
    let _mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    // SAFETY: plain call, right after CreateMutexW.
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return Ok(());
    }
    let fixed = Fixed {
        version: MenuItem::new(format!("dstack {}", env!("CARGO_PKG_VERSION")), false, None),
        copy: MenuItem::new("Copy status", true, None),
        exit: MenuItem::new("Exit (the devices keep running)", true, None),
    };
    let mut scanner = Scanner::new();
    let mut tracker = Tracker::default();
    let mut readings = tick(&mut scanner);
    let first = tracker.update(&readings);
    let (first_menu, mut lines) = menu(&fixed, &first)?;
    let tray: TrayIcon = TrayIconBuilder::new()
        .with_menu(Box::new(first_menu))
        .with_tooltip(&first.tooltip)
        .with_icon(tray_icon(first.colour)?)
        .build()
        .map_err(|e| format!("the tray icon: {e}"))?;
    let mut shown = first;
    // A thread timer: WM_TIMER in this thread's queue every TICK_MS.
    // SAFETY: no window, no callback.
    unsafe { SetTimer(std::ptr::null_mut(), 0, TICK_MS, None) };
    let menu_events = MenuEvent::receiver();
    // SAFETY: MSG is plain data.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: a valid MSG; all of this thread's messages.
        let got = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if got <= 0 {
            return Ok(());
        }
        if msg.message == WM_TIMER && msg.hwnd.is_null() {
            readings = tick(&mut scanner);
            let v = tracker.update(&readings);
            if v.lines.len() == lines.len() && !lines.is_empty() {
                for (item, text) in lines.iter().zip(&v.lines) {
                    if item.text() != *text {
                        item.set_text(text);
                    }
                }
            } else if v.lines != shown.lines {
                let (m, items) = menu(&fixed, &v)?;
                lines = items;
                tray.set_menu(Some(Box::new(m)));
            }
            if v.colour != shown.colour {
                tray.set_icon(Some(tray_icon(v.colour)?))
                    .map_err(|e| format!("the tray icon: {e}"))?;
            }
            if v.tooltip != shown.tooltip {
                tray.set_tooltip(Some(&v.tooltip))
                    .map_err(|e| format!("the tooltip: {e}"))?;
            }
            shown = v;
        } else {
            // SAFETY: a message GetMessageW filled.
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        while let Ok(e) = menu_events.try_recv() {
            if e.id == *fixed.exit.id() {
                return Ok(());
            }
            if e.id == *fixed.copy.id()
                && let Err(err) = copy(&model::copy_json(&readings))
            {
                message(&format!("dtray: {err}"));
            }
        }
    }
}

/// Discovery kept in step with the PC's addresses (a failure is tried
/// again on the next tick), then every local device's reading.
fn tick(scanner: &mut Scanner) -> Vec<Reading> {
    let _failed = scanner.refresh();
    scanner.readings()
}

/// The menu: the version, a line per device, Copy status, Exit.
fn menu(fixed: &Fixed, v: &View) -> Result<(Menu, Vec<MenuItem>), String> {
    let menu = Menu::new();
    let e = |r: Result<(), tray_icon::menu::Error>| r.map_err(|e| format!("the menu: {e}"));
    e(menu.append(&fixed.version))?;
    e(menu.append(&PredefinedMenuItem::separator()))?;
    let mut items = Vec::new();
    if v.lines.is_empty() {
        e(menu.append(&MenuItem::new("No dstack device on this PC", false, None)))?;
    }
    for text in &v.lines {
        let item = MenuItem::new(text, false, None);
        e(menu.append(&item))?;
        items.push(item);
    }
    e(menu.append(&PredefinedMenuItem::separator()))?;
    e(menu.append(&fixed.copy))?;
    e(menu.append(&fixed.exit))?;
    Ok((menu, items))
}

/// The icon in the view's colour ([`icon::rgba`]).
fn tray_icon(c: Colour) -> Result<Icon, String> {
    Icon::from_rgba(icon::rgba(c), icon::SIZE, icon::SIZE).map_err(|e| format!("the icon: {e}"))
}

/// `text` on the clipboard as Unicode text.
fn copy(text: &str) -> Result<(), String> {
    let w = wide(text);
    let bytes = w.len() * std::mem::size_of::<u16>();
    // SAFETY: no owner window; every path below closes the clipboard once.
    if unsafe { OpenClipboard(std::ptr::null_mut()) } == 0 {
        return Err("the clipboard could not be opened".into());
    }
    // SAFETY: the clipboard is open (above); the memory is ours until
    // SetClipboardData takes it, and freed by us when it does not.
    let result = unsafe {
        EmptyClipboard();
        let mem = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if mem.is_null() {
            Err("no memory for the clipboard".to_string())
        } else {
            let dst = GlobalLock(mem).cast::<u16>();
            if dst.is_null() {
                GlobalFree(mem);
                Err("the clipboard memory could not be locked".to_string())
            } else {
                std::ptr::copy_nonoverlapping(w.as_ptr(), dst, w.len());
                GlobalUnlock(mem);
                if SetClipboardData(u32::from(CF_UNICODETEXT), mem).is_null() {
                    GlobalFree(mem);
                    Err("the clipboard did not take the text".to_string())
                } else {
                    Ok(())
                }
            }
        }
    };
    // SAFETY: opened above.
    unsafe { CloseClipboard() };
    result
}
