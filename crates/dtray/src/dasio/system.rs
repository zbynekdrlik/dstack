// SPDX-License-Identifier: MIT OR Apache-2.0

//! The settings window's Windows calls (dstack #246): the PC's adapters
//! for the interface list ([`adapters`]) and the elevated helper's start
//! ([`run_elevated`]). Thin glue, outside the mutation gate
//! (`.cargo/mutants.toml`); the lists and the helper are tested in
//! [`super::choices`] and [`super::apply_staged`].

use std::net::Ipv4Addr;
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BUFFER_OVERFLOW, ERROR_CANCELLED, GetLastError, NO_ERROR, WAIT_OBJECT_0,
};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses,
    IP_ADAPTER_ADDRESSES_LH,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};
use windows_sys::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
use windows_sys::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

use super::choices::Adapter;
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::UI::Shell::{
    FOLDERID_ProgramData, FOLDERID_ProgramFiles, SHGetKnownFolderPath,
};

/// A known folder's path (the system's, never the environment's).
fn known_folder(id: &windows_sys::core::GUID) -> Option<std::ffi::OsString> {
    let mut p: *mut u16 = std::ptr::null_mut();
    // SAFETY: a valid id; the path is the shell's until freed below.
    let hr = unsafe { SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut p) };
    // SAFETY: a NUL-terminated string from the call (or null).
    let text = (hr >= 0).then(|| unsafe { text_of(p) });
    // SAFETY: the shell's allocation; null is fine.
    unsafe { CoTaskMemFree(p.cast()) };
    text.filter(|t| !t.is_empty()).map(Into::into)
}

/// The ProgramData folder.
pub fn program_data() -> Option<std::ffi::OsString> {
    known_folder(&FOLDERID_ProgramData)
}

/// The (64-bit) Program Files folder.
pub fn program_files() -> Option<std::ffi::OsString> {
    known_folder(&FOLDERID_ProgramFiles)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// A NUL-terminated UTF-16 string as text; empty for null.
///
/// # Safety
/// `p` is null or points at a NUL-terminated UTF-16 string.
unsafe fn text_of(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0;
    // SAFETY: the caller's promise: NUL-terminated.
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    // SAFETY: `n` units before the NUL.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) })
}

/// The PC's network adapters (IPv4 view): alias, description, IPv4
/// addresses and receive speed.
pub fn adapters() -> Result<Vec<Adapter>, String> {
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut len: u32 = 16 * 1024;
    let mut buf: Vec<u64> = Vec::new();
    let mut done = false;
    for _ in 0..4 {
        buf = vec![0u64; (len as usize).div_ceil(8)];
        // SAFETY: `buf` is `len` bytes, 8-aligned, writable.
        let r = unsafe {
            GetAdaptersAddresses(
                u32::from(AF_INET),
                flags,
                std::ptr::null(),
                buf.as_mut_ptr().cast(),
                &mut len,
            )
        };
        if r == NO_ERROR {
            done = true;
            break;
        }
        if r != ERROR_BUFFER_OVERFLOW {
            return Err(format!("GetAdaptersAddresses failed ({r})"));
        }
    }
    if !done {
        return Err("GetAdaptersAddresses: the list kept growing".to_string());
    }
    let mut out = Vec::new();
    let mut a = buf.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
    while !a.is_null() {
        // SAFETY: a node of the list the call built inside `buf`.
        let ad = unsafe { &*a };
        let mut addresses = Vec::new();
        let mut u = ad.FirstUnicastAddress;
        while !u.is_null() {
            // SAFETY: a node of the adapter's unicast list.
            let ua = unsafe { &*u };
            let sa = ua.Address.lpSockaddr;
            // SAFETY: an AF_INET sockaddr (IPv4 only was asked for).
            if !sa.is_null() && unsafe { (*sa).sa_family } == AF_INET {
                // SAFETY: as above.
                let sin = unsafe { &*sa.cast::<SOCKADDR_IN>() };
                // SAFETY: the IPv4 member of an AF_INET address.
                let raw = unsafe { sin.sin_addr.S_un.S_addr };
                addresses.push(Ipv4Addr::from(u32::from_be(raw)));
            }
            u = ua.Next;
        }
        let mbit = (ad.ReceiveLinkSpeed != 0 && ad.ReceiveLinkSpeed != u64::MAX)
            .then_some(ad.ReceiveLinkSpeed / 1_000_000);
        out.push(Adapter {
            // SAFETY: the adapter's NUL-terminated wide strings.
            alias: unsafe { text_of(ad.FriendlyName) },
            // SAFETY: as above.
            description: unsafe { text_of(ad.Description) },
            addresses,
            mbit,
        });
        a = ad.Next;
    }
    Ok(out)
}

/// Runs `exe` with `params` as administrator (Windows asks once: UAC,
/// in front of the window `owner`), hidden, and waits for it at most
/// `wait`: its exit code; the reason when it did not start (declined:
/// "administrator rights were not given") or did not end in time. COM is
/// set up on the calling thread first, as `ShellExecuteExW` asks.
pub fn run_elevated(
    exe: &std::path::Path,
    params: &str,
    wait: Duration,
    owner: windows_sys::Win32::Foundation::HWND,
) -> Result<u32, String> {
    // SAFETY: plain call; a second call on the thread is harmless (S_FALSE).
    unsafe {
        CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    };
    let verb = wide("runas");
    let file = wide(&exe.display().to_string());
    let params = wide(params);
    // SAFETY: plain data.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.hwnd = owner;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_HIDE;
    // SAFETY: a filled structure whose strings outlive the call.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        // SAFETY: plain call, right after the failed one.
        let code = unsafe { GetLastError() };
        if code == ERROR_CANCELLED {
            return Err("administrator rights were not given".to_string());
        }
        return Err(format!("the helper did not start (error {code})"));
    }
    if info.hProcess.is_null() {
        return Err("the helper started without a process handle".to_string());
    }
    let ms = u32::try_from(wait.as_millis()).unwrap_or(u32::MAX);
    // SAFETY: the process handle ShellExecuteExW returned.
    let waited = unsafe { WaitForSingleObject(info.hProcess, ms) };
    let mut code = 0u32;
    // SAFETY: as above; `code` is writable.
    let got =
        waited == WAIT_OBJECT_0 && unsafe { GetExitCodeProcess(info.hProcess, &mut code) } != 0;
    // SAFETY: ours to close.
    unsafe { CloseHandle(info.hProcess) };
    if got {
        Ok(code)
    } else {
        Err(format!(
            "the helper did not end within {} s",
            wait.as_secs()
        ))
    }
}
