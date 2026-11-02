//! The Dante NIC's large send offload, checked at start on Windows (dstack
//! #1, the FOH PC, 1.11.2026). With LSO on, another sender's large TCP
//! send leaves the NIC as one long burst, and our packets wait behind it:
//! dasio's A/B on the FOH PC counted 184 gaps in 30 s with LSO on and 117
//! with it off.
//!
//! The device reads the adapter's `*LsoV2IPv4` and `*LsoV1IPv4` settings
//! (the values Windows' advanced NIC properties keep in the registry, under
//! the network class key, by the adapter's GUID) and warns when either is
//! on. It never changes the NIC: that is the PC owner's step (the link
//! resets when applied).

#[cfg(windows)]
use tracing::{info, warn};

/// What the start line says about the NIC's large send offload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// LSO is off (every version the adapter has).
    Off,
    /// LSO is on for IPv4 (v2 or v1).
    On,
    /// Not read: the adapter keeps no such setting, or it was not found.
    Unknown,
}

/// The verdict on an adapter's `*LsoV2IPv4` and `*LsoV1IPv4` values (each
/// `"1"` when on, `"0"` when off, absent when the adapter has none).
pub fn verdict(v2: Option<&str>, v1: Option<&str>) -> Verdict {
    let on = |v: Option<&str>| v.map(|s| s.trim() == "1");
    match (on(v2), on(v1)) {
        (Some(true), _) | (_, Some(true)) => Verdict::On,
        (None, None) => Verdict::Unknown,
        _ => Verdict::Off,
    }
}

/// The verdict for the adapter whose GUID is `adapter` (`{...}`).
#[cfg(windows)]
pub fn check(adapter: &str) -> Verdict {
    match sys::lso_values(adapter) {
        Some((v2, v1)) => verdict(v2.as_deref(), v1.as_deref()),
        None => Verdict::Unknown,
    }
}

/// [`check`] of `adapter`, logged: the start line's companion (log-only
/// glue, left out of the mutation gate).
#[cfg(windows)]
pub fn report(adapter: &str) {
    match check(adapter) {
        Verdict::Off => info!("the Dante NIC's large send offload is off"),
        Verdict::On => warn!(
            "the Dante NIC has large send offload on: another sender's large sends delay our packets (late packets at a receiver); turn LSO off on this NIC (docs/windows-box.md)"
        ),
        Verdict::Unknown => info!("the Dante NIC's large send offload was not read"),
    }
}

#[cfg(windows)]
pub(crate) mod sys {
    use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};

    /// The network adapters' class key.
    const CLASS: &str =
        r"SYSTEM\CurrentControlSet\Control\Class\{4d36e972-e325-11ce-bfc1-08002be10318}";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// A string value of HKLM\`key`, `None` when absent.
    fn read(key: &str, value: &str) -> Option<String> {
        let (k, v) = (wide(key), wide(value));
        let mut buf = [0u16; 128];
        let mut len = (buf.len() * 2) as u32;
        // SAFETY: valid NUL-terminated key and value names; `buf` is `len`
        // writable bytes; no type out.
        let r = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                k.as_ptr(),
                v.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut len,
            )
        };
        if r != 0 {
            return None;
        }
        let n = (len as usize / 2).min(buf.len());
        Some(
            String::from_utf16_lossy(&buf[..n])
                .trim_end_matches('\0')
                .to_string(),
        )
    }

    /// The `*LsoV2IPv4` and `*LsoV1IPv4` values of the adapter whose
    /// `NetCfgInstanceId` is `adapter`; `None` when no class subkey has it.
    pub fn lso_values(adapter: &str) -> Option<(Option<String>, Option<String>)> {
        (0..1000).find_map(|i| {
            let key = format!(r"{CLASS}\{i:04}");
            let id = read(&key, "NetCfgInstanceId")?;
            id.eq_ignore_ascii_case(adapter)
                .then(|| (read(&key, "*LsoV2IPv4"), read(&key, "*LsoV1IPv4")))
        })
    }
}

#[cfg(test)]
mod tests;
