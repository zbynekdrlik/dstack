// SPDX-License-Identifier: MIT OR Apache-2.0

//! The tray on Windows: the OS's glue around [`Tray`] (the icon and menu,
//! shared with Linux). A thread timer wakes the message loop once a second
//! for [`Tray::tick`]; the menu's clicks are handled after each message.
//! One tray a desktop session (the named mutex [`MUTEX`]). Copy status goes
//! to the Windows clipboard; an error shows in a message box.
//!
//! [`autostart`]: the user's Run value for `dtray --autostart on|off`.
//! [`attach_console`]: the shell's console for the command-line modes.

use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, GetLastError, GlobalFree,
};
use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW,
};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MB_ICONERROR, MB_OK, MSG, MessageBoxW, SetTimer,
    TranslateMessage, WM_TIMER,
};

use crate::tray::{Action, Tray};

/// The single-instance mutex of a desktop session.
const MUTEX: &str = r"Local\dtray";
/// The timer's period, ms.
const TICK_MS: u32 = 1000;
/// The user's key of programs started at logon.
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Attaches to the console of the process that started dtray, when it has
/// one (cmd, PowerShell), so a command-line mode's stdout and stderr reach
/// that shell ([`dtray::wants_console`], dstack #213); a handle the caller
/// already gave (a redirect) is kept. As dlisten and dsend do
/// (`dstack_endpoint::win::attach_console`).
pub fn attach_console() {
    // SAFETY: plain call; failing (no parent console: Explorer, the Run
    // value) changes nothing.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
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

/// Sets (`on`) or removes the user's Run value [`dtray::RUN_VALUE_NAME`]
/// for this program ([`dtray::run_value`]); removing one that is not
/// there is done.
pub fn autostart(on: bool) -> Result<(), String> {
    let (key, name) = (wide(RUN_KEY), wide(dtray::RUN_VALUE_NAME));
    if on {
        let exe = std::env::current_exe().map_err(|e| format!("the program's path: {e}"))?;
        let text = dtray::run_value(&exe)
            .ok_or_else(|| "the program's path is not valid Unicode".to_string())?;
        let value = wide(&text);
        let bytes = u32::try_from(value.len() * std::mem::size_of::<u16>())
            .map_err(|_| "the program's path is too long".to_string())?;
        // SAFETY: valid NUL-terminated strings; `bytes` covers `value` and
        // its NUL.
        let r = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                REG_SZ,
                value.as_ptr().cast(),
                bytes,
            )
        };
        if r != ERROR_SUCCESS {
            return Err(format!("the Run value was not set (error {r})"));
        }
    } else {
        // SAFETY: valid NUL-terminated strings.
        let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) };
        if r != ERROR_SUCCESS && r != ERROR_FILE_NOT_FOUND {
            return Err(format!("the Run value was not removed (error {r})"));
        }
    }
    Ok(())
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
    let mut tray = Tray::start()?;
    // A thread timer: WM_TIMER in this thread's queue every TICK_MS.
    // SAFETY: no window, no callback.
    unsafe { SetTimer(std::ptr::null_mut(), 0, TICK_MS, None) };
    // SAFETY: MSG is plain data.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: a valid MSG; all of this thread's messages.
        let got = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if got == -1 {
            // SAFETY: plain call, right after GetMessageW failed.
            let code = unsafe { GetLastError() };
            return Err(format!(
                "the message loop failed (GetMessageW, error {code})"
            ));
        }
        if got == 0 {
            return Ok(());
        }
        if msg.message == WM_TIMER && msg.hwnd.is_null() {
            tray.tick()?;
        } else {
            // SAFETY: a message GetMessageW filled.
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        for action in tray.menu_events() {
            match action {
                Action::Exit => return Ok(()),
                Action::Copy(text) => {
                    if let Err(err) = copy(&text) {
                        message(&format!("dtray: {err}"));
                    }
                }
                Action::Error(err) => message(&err),
            }
        }
    }
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
