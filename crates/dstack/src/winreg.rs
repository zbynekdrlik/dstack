// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! Read-only access to the Windows registry, shared by the checks that read
//! it (the NIC's offload in [`crate::lso`], the QoS policies in
//! [`crate::media::dscp`]). Windows-only glue, so outside the mutation gate
//! (`.cargo/mutants.toml`); the decisions over what it reads are tested on
//! every platform where they live.

use windows_sys::Win32::Foundation::{ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS};
use windows_sys::Win32::System::Registry::{
    KEY_READ, REG_DWORD, REG_EXPAND_SZ, REG_SZ, RRF_RT_ANY, RRF_RT_REG_SZ, RegCloseKey,
    RegEnumKeyExW, RegEnumValueW, RegGetValueW, RegOpenKeyExW,
};

pub use windows_sys::Win32::System::Registry::{HKEY, HKEY_LOCAL_MACHINE};

/// A registry value as read: a string (REG_SZ, or REG_EXPAND_SZ expanded)
/// or a DWORD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// A string.
    Text(String),
    /// A DWORD.
    Number(u32),
}

/// A NUL-terminated UTF-16 copy of `s`.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// An open key, closed on drop.
pub struct Key(HKEY);

impl Key {
    /// Opens `root`\\`path` for reading; the Windows error code when it
    /// cannot (2: no such key).
    pub fn open(root: HKEY, path: &str) -> Result<Self, u32> {
        let path = wide(path);
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: a NUL-terminated path and a writable handle.
        let r = unsafe { RegOpenKeyExW(root, path.as_ptr(), 0, KEY_READ, &mut key) };
        if r == 0 { Ok(Key(key)) } else { Err(r) }
    }

    /// The names of its subkeys; on an enumeration error the names read
    /// before it and the error code.
    pub fn subkeys(&self) -> (Vec<String>, Option<u32>) {
        let mut names = Vec::new();
        for index in 0u32.. {
            // A key name holds 255 characters at most.
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            // SAFETY: an open key; `name` holds `len` characters; the
            // other out pointers are null (not wanted).
            let r = unsafe {
                RegEnumKeyExW(
                    self.0,
                    index,
                    name.as_mut_ptr(),
                    &mut len,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            match r {
                0 => names.push(String::from_utf16_lossy(
                    &name[..(len as usize).min(name.len())],
                )),
                ERROR_NO_MORE_ITEMS => return (names, None),
                code => return (names, Some(code)),
            }
        }
        (names, None)
    }

    /// Every string and DWORD value of its subkey `sub` (other types are
    /// left out); the Windows error code when the subkey cannot be opened.
    pub fn values(&self, sub: &str) -> Result<Vec<(String, Value)>, u32> {
        let sub = Key::open(self.0, sub)?;
        let mut out = Vec::new();
        for index in 0u32.. {
            // A value name holds 16383 characters at most.
            let mut name = vec![0u16; 16384];
            let mut len = name.len() as u32;
            // SAFETY: an open key; `name` holds `len` characters; no data
            // wanted (null type, data and size).
            let r = unsafe {
                RegEnumValueW(
                    sub.0,
                    index,
                    name.as_mut_ptr(),
                    &mut len,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if r != 0 {
                break;
            }
            let name = String::from_utf16_lossy(&name[..(len as usize).min(name.len())]);
            if let Some(v) = sub.get(&name, RRF_RT_ANY) {
                out.push((name, v));
            }
        }
        Ok(out)
    }

    /// The value `name` of this key, restricted to the `RRF_RT_*` types
    /// `types`; none when absent or of another type.
    fn get(&self, name: &str, types: u32) -> Option<Value> {
        let name = wide(name);
        let mut buf: Vec<u16> = vec![0; 128];
        // An expanded string can outgrow the size asked first: try again.
        for _ in 0..4 {
            let mut len = (buf.len() * 2) as u32;
            let mut kind = 0u32;
            // SAFETY: an open key, a NUL-terminated name, `buf` is `len`
            // writable bytes, `kind` writable; no subkey (null).
            let r = unsafe {
                RegGetValueW(
                    self.0,
                    std::ptr::null(),
                    name.as_ptr(),
                    types,
                    &mut kind,
                    buf.as_mut_ptr().cast(),
                    &mut len,
                )
            };
            if r == ERROR_MORE_DATA {
                buf = vec![0; (len as usize).div_ceil(2) + 64];
                continue;
            }
            if r != 0 {
                return None;
            }
            let bytes: Vec<u8> = buf.iter().flat_map(|w| w.to_le_bytes()).collect();
            return match kind {
                REG_DWORD => Some(Value::Number(u32::from_le_bytes(
                    bytes.get(..4)?.try_into().ok()?,
                ))),
                REG_SZ | REG_EXPAND_SZ => {
                    let n = (len as usize / 2).min(buf.len());
                    Some(Value::Text(
                        String::from_utf16_lossy(&buf[..n])
                            .trim_end_matches('\0')
                            .to_string(),
                    ))
                }
                _ => None,
            };
        }
        None
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the key this value opened.
        unsafe { RegCloseKey(self.0) };
    }
}

/// The string value `value` of HKLM\\`key`; none when absent.
pub fn read_sz(key: &str, value: &str) -> Option<String> {
    match Key::open(HKEY_LOCAL_MACHINE, key)
        .ok()?
        .get(value, RRF_RT_REG_SZ)?
    {
        Value::Text(s) => Some(s),
        Value::Number(_) => None,
    }
}
