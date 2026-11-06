// SPDX-License-Identifier: MIT OR Apache-2.0

//! dlisten on Windows: a GUI-subsystem program with no window (dtray shows
//! it). [`attach_console`] gives the command line's output
//! (`--list-outputs`, `--help`, the log) the console it was typed in, when
//! there is one; a failure to start shows in a message box ([`message`]).
//! The start at logon is the user's Run value, as dtray's
//! ([`dlisten::autostart`]).

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};

use dlisten::autostart;

/// The user's key of programs started at logon.
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// The console of the process that started us, when there is one (a
/// command line); none from Explorer or the Run value.
pub fn attach_console() {
    // SAFETY: plain call; failing (no parent console) changes nothing.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

/// An error box (a start from Explorer or the Run value has no console).
pub fn message(text: &str) {
    let (t, c) = (wide(text), wide("dlisten"));
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

/// Whether the user's Run value [`autostart::RUN_VALUE_NAME`] is there.
pub fn autostart_on() -> bool {
    let (key, name) = (wide(RUN_KEY), wide(autostart::RUN_VALUE_NAME));
    // SAFETY: valid NUL-terminated strings; no data asked, only whether
    // the value exists.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    r == ERROR_SUCCESS
}

/// Sets (`on`, starting dlisten with `args`) or removes the user's Run
/// value; removing one that is not there is done.
pub fn set_autostart(on: bool, args: &[String]) -> Result<(), String> {
    let (key, name) = (wide(RUN_KEY), wide(autostart::RUN_VALUE_NAME));
    if on {
        let exe = std::env::current_exe().map_err(|e| format!("the program's path: {e}"))?;
        let text = autostart::run_value(&exe, args)
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
