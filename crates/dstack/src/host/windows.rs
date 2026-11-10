//! The host's Windows glue: the adapter's MAC, link speed and GUID, and
//! the start line's NIC check. Windows-only, so outside the mutation gate
//! (`.cargo/mutants.toml`); CI builds and tests it on Windows.

use super::{HostError, run_failed, setup};
use crate::daemon::{self, IfAddr};
use crate::info::NetInfo;

/// On Windows: the MAC, the receive link speed and the default gateway
/// of the adapter holding the address (`GetAdaptersAddresses`).
pub fn interface_info(ifa: &IfAddr) -> Result<NetInfo, HostError> {
    let (mac, mbit, adapter, _, gateway) = win_nic::of_with_index(ifa.ip).map_err(run_failed)?;
    // The start line's NIC check (dstack #1): large send offload delays
    // our packets behind other traffic.
    crate::lso::report(&adapter);
    let speed = mbit.map(|m| m.to_string());
    let mut net = daemon::net_info(ifa, &mac, speed.as_deref(), None).map_err(setup)?;
    net.gateway = gateway.unwrap_or(std::net::Ipv4Addr::UNSPECIFIED);
    Ok(net)
}

pub(crate) mod win_nic {
    use std::net::Ipv4Addr;

    use windows_sys::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, NO_ERROR};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
        GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows_sys::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};

    /// The MAC (`aa:bb:..`), the receive speed in Mbit/s and the GUID
    /// (`{...}`, its registry name) of the adapter that holds `ip`.
    #[cfg(test)]
    pub fn of(ip: Ipv4Addr) -> Result<(String, Option<u64>, String), String> {
        of_with_index(ip).map(|(mac, speed, guid, _, _)| (mac, speed, guid))
    }

    /// The adapter's details as [`of`]'s, its interface index, and its
    /// first IPv4 default gateway (dstack #155: a Dante NIC should hold
    /// none).
    #[allow(clippy::type_complexity)]
    pub fn of_with_index(
        ip: Ipv4Addr,
    ) -> Result<(String, Option<u64>, String, u32, Option<Ipv4Addr>), String> {
        let flags = GAA_FLAG_SKIP_ANYCAST
            | GAA_FLAG_SKIP_MULTICAST
            | GAA_FLAG_SKIP_DNS_SERVER
            | GAA_FLAG_INCLUDE_GATEWAYS;
        let mut len: u32 = 16 * 1024;
        let mut buf: Vec<u64> = Vec::new();
        for _ in 0..4 {
            buf = vec![0u64; (len as usize).div_ceil(8)];
            // SAFETY: `buf` is `len` bytes, 8-aligned, writable; the call
            // fills it with a linked list of adapters inside it.
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
                break;
            }
            if r != ERROR_BUFFER_OVERFLOW {
                return Err(format!("GetAdaptersAddresses failed ({r})"));
            }
        }
        let mut a = buf.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        while !a.is_null() {
            // SAFETY: a node of the list the call built inside `buf`.
            let ad = unsafe { &*a };
            let mut u = ad.FirstUnicastAddress;
            while !u.is_null() {
                // SAFETY: a node of the adapter's unicast list.
                let ua = unsafe { &*u };
                let sa = ua.Address.lpSockaddr;
                // SAFETY: an AF_INET sockaddr (the call was asked for IPv4 only).
                if !sa.is_null() && unsafe { (*sa).sa_family } == AF_INET {
                    let sin = unsafe { &*sa.cast::<SOCKADDR_IN>() };
                    // SAFETY: the IPv4 union member of an AF_INET address.
                    let raw = unsafe { sin.sin_addr.S_un.S_addr };
                    if Ipv4Addr::from(u32::from_be(raw)) == ip {
                        let n = (ad.PhysicalAddressLength as usize).min(8);
                        let mac = ad.PhysicalAddress[..n]
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect::<Vec<_>>()
                            .join(":");
                        let speed = (ad.ReceiveLinkSpeed != 0 && ad.ReceiveLinkSpeed != u64::MAX)
                            .then_some(ad.ReceiveLinkSpeed / 1_000_000);
                        // SAFETY: the adapter's NUL-terminated ANSI name.
                        let guid = if ad.AdapterName.is_null() {
                            String::new()
                        } else {
                            unsafe { std::ffi::CStr::from_ptr(ad.AdapterName.cast()) }
                                .to_string_lossy()
                                .into_owned()
                        };
                        // SAFETY: the IPv4 index of the adapter's header union.
                        let index = unsafe { ad.Anonymous1.Anonymous.IfIndex };
                        let mut gateway = None;
                        let mut g = ad.FirstGatewayAddress;
                        while !g.is_null() && gateway.is_none() {
                            // SAFETY: a node of the adapter's gateway list.
                            let ga = unsafe { &*g };
                            let sa = ga.Address.lpSockaddr;
                            // SAFETY: as for the unicast addresses above.
                            if !sa.is_null() && unsafe { (*sa).sa_family } == AF_INET {
                                let sin = unsafe { &*sa.cast::<SOCKADDR_IN>() };
                                let raw = unsafe { sin.sin_addr.S_un.S_addr };
                                gateway = Some(Ipv4Addr::from(u32::from_be(raw)))
                                    .filter(|g| !g.is_unspecified());
                            }
                            g = ga.Next;
                        }
                        return Ok((mac, speed, guid, index, gateway));
                    }
                }
                u = ua.Next;
            }
            a = ad.Next;
        }
        Err("no adapter holds the device's address".to_string())
    }
}
