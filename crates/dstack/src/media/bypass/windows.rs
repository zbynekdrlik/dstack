// SPDX-License-Identifier: MIT OR Apache-2.0

//! The host frame sender's Windows glue (dstack #1): the neighbour table
//! (GetIpNetTable2, the analog of `/proc/net/arp`) and our NIC's MAC
//! (GetAdaptersAddresses). Thin: every decision is a pure function in
//! [`super`] ([`super::neighbour_row`], [`super::parse_mac`]), tested on
//! every OS.

use std::net::Ipv4Addr;

use windows_sys::Win32::Foundation::NO_ERROR;
use windows_sys::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetIpNetTable2, MIB_IPNET_ROW2, MIB_IPNET_TABLE2,
};
use windows_sys::Win32::Networking::WinSock::AF_INET;

use super::Neighbour;

/// The resolved IPv4 entries of the neighbour table; empty when it cannot
/// be read (every flow then stays on its UDP socket until the next read).
pub(crate) fn neighbours() -> Vec<Neighbour> {
    let mut table: *mut MIB_IPNET_TABLE2 = std::ptr::null_mut();
    // SAFETY: `table` receives a table the call allocates; freed below.
    let r = unsafe { GetIpNetTable2(AF_INET, &mut table) };
    if r != NO_ERROR || table.is_null() {
        return Vec::new();
    }
    // SAFETY: the call returned a valid table of `NumEntries` rows, laid
    // out after its count (the 1-row array is a C flexible array member, so
    // the base pointer comes from the whole table, not the array); it stays
    // valid until FreeMibTable.
    let rows: &[MIB_IPNET_ROW2] = unsafe {
        let base = std::ptr::addr_of!((*table).Table).cast::<MIB_IPNET_ROW2>();
        std::slice::from_raw_parts(base, (*table).NumEntries as usize)
    };
    let out = rows
        .iter()
        .filter_map(|row| {
            // SAFETY: the family is read through the union's common
            // field; the IPv4 member only for an AF_INET row.
            if unsafe { row.Address.si_family } != AF_INET {
                return None;
            }
            // SAFETY: an AF_INET row's address is its IPv4 member.
            let raw = unsafe { row.Address.Ipv4.sin_addr.S_un.S_addr };
            let n = (row.PhysicalAddressLength as usize).min(row.PhysicalAddress.len());
            super::neighbour_row(
                Ipv4Addr::from(u32::from_be(raw)),
                &row.PhysicalAddress[..n],
                row.State,
                row.InterfaceIndex,
            )
        })
        .collect();
    // SAFETY: the table GetIpNetTable2 allocated, freed once.
    unsafe { FreeMibTable(table.cast()) };
    out
}

/// The interface index, MAC and name (its adapter GUID) of the adapter
/// holding `ip`; `None` when none does or its MAC is not 6 bytes.
pub(crate) fn nic(ip: Ipv4Addr) -> Option<(u32, [u8; 6], String)> {
    let (mac, _, guid, index, _) = crate::host::windows::win_nic::of_with_index(ip).ok()?;
    Some((index, super::parse_mac(&mac)?, guid))
}
