//! The NIC check's Windows glue: the adapter's settings read from the
//! registry and the start line. Windows-only, so outside the mutation gate
//! (`.cargo/mutants.toml`); CI tests it on Windows.

use tracing::{info, warn};

use super::{Verdict, verdict};

/// The verdict for the adapter whose GUID is `adapter` (`{...}`).
pub fn check(adapter: &str) -> Verdict {
    match sys::lso_values(adapter) {
        Some((v2, v1)) => verdict(v2.as_deref(), v1.as_deref()),
        None => Verdict::Unknown,
    }
}

/// [`check`] of `adapter`, logged: the start line's companion (log-only
/// glue, left out of the mutation gate).
pub fn report(adapter: &str) {
    match check(adapter) {
        Verdict::Off => info!("the Dante NIC's large send offload is off"),
        Verdict::On => warn!(
            "the Dante NIC has large send offload on: another sender's large sends delay our packets (late packets at a receiver); turn LSO off on this NIC (docs/windows-box.md)"
        ),
        Verdict::Unknown => info!("the Dante NIC's large send offload was not read"),
    }
}

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
