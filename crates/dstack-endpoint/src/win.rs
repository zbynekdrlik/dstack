// SPDX-License-Identifier: MIT OR Apache-2.0

//! The Windows process glue dlisten and dsend share: both are
//! GUI-subsystem programs with no window (dtray shows them).
//! [`attach_console`] gives the command line's output the console it was
//! typed in, when there is one; a failure to start shows in a message box
//! ([`message`]) when nothing reads stderr ([`crate::alert::shows_box`]).

use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};

/// `s` as a NUL-terminated UTF-16 string.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Where a failure can be read: whether stderr was handed to us (a script
/// redirected it), then whether we got the console of the process that
/// started us (a command line). Neither from Explorer or the Run value
/// ([`crate::alert::shows_box`]).
pub fn attach_console() -> (bool, bool) {
    // SAFETY: plain call; reads the handle the process started with.
    let err = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
    let has_stderr = !err.is_null() && err != INVALID_HANDLE_VALUE;
    // SAFETY: plain call; failing (no parent console) changes nothing.
    let attached = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } != 0;
    (attached, has_stderr)
}

/// An error box titled `app` (a start from Explorer or the Run value has
/// no console).
pub fn message(app: &str, text: &str) {
    let (t, c) = (wide(text), wide(app));
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
