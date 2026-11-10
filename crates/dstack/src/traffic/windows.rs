//! The traffic watch's Windows glue: the adapter's sent-bytes counter
//! (`GetIfEntry2`'s `OutOctets`). Windows-only, so outside the mutation
//! gate (`.cargo/mutants.toml`); CI tests it on Windows.

use std::net::Ipv4Addr;

use windows_sys::Win32::NetworkManagement::IpHelper::{GetIfEntry2, MIB_IF_ROW2};

/// The bytes the adapter holding `ip` has sent; `None` when none holds it
/// or Windows refuses the read.
pub fn nic_sent_bytes(ip: Ipv4Addr) -> Option<u64> {
    let (_, _, _, index, _) = crate::host::windows::win_nic::of_with_index(ip).ok()?;
    // SAFETY: a zeroed MIB_IF_ROW2 is a valid input; only InterfaceIndex
    // is read by the call, which fills the rest.
    let mut row: MIB_IF_ROW2 = unsafe { std::mem::zeroed() };
    row.InterfaceIndex = index;
    // SAFETY: `row` is a valid, writable MIB_IF_ROW2.
    (unsafe { GetIfEntry2(&mut row) } == 0).then_some(row.OutOctets)
}
