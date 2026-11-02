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

#[cfg(windows)]
mod windows;
#[cfg(all(windows, test))]
use windows::sys;
#[cfg(windows)]
pub use windows::{check, report};

#[cfg(test)]
mod tests;
