// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The RX flow sockets' OS side: the receive buffer, the multicast join,
//! and the kernel's receive time of each datagram (dstack #234).
//!
//! The RX thread judges a packet's lateness at its pass, when it reads the
//! datagram. A packet late on the wire and a packet that came on time but
//! waited for a late RX-thread wake look the same there. On Linux each flow
//! socket asks the kernel to stamp every datagram's receive time
//! (`SO_TIMESTAMPNS`, the wall clock), and the thread reads it with the
//! datagram (`recvmsg`, the `SCM_TIMESTAMPNS` control message). The pass
//! reads the wall clock once at its start; the difference is the packet's
//! wake delay ([`wake_delay_ns`]), and its lateness on the wire is its
//! lateness at the pass's PTP time less that delay ([`wire_lateness`]).
//! Only differences of one clock's reads are used, so the wall clock's
//! offset from PTP does not matter.
//!
//! Windows reads with `recv_from` as before: no stamp, so no wake delay
//! and no wire lateness are measured there ([`super::RxShared::stamps`] is
//! false).

use std::io::{self, ErrorKind};
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};

use crate::clock;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(not(target_os = "linux"))]
mod other;

#[cfg(target_os = "linux")]
pub(super) use linux::{recv_stamped, stamp_receives};
#[cfg(not(target_os = "linux"))]
pub(super) use other::{recv_stamped, stamp_receives};

/// One datagram read from a flow socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Received {
    /// Its bytes in the buffer.
    pub len: usize,
    /// Its sender; `None` for any address that is no IPv4 one.
    pub from: Option<SocketAddrV4>,
    /// When the kernel received it, wall-clock ns since 1970; `None`
    /// without a stamp (Windows, or a socket the kernel stamps nothing on).
    pub at_ns: Option<u64>,
}

/// The wake delay of a datagram the kernel received at `at_ns` and a pass
/// read at `wall_ns` (both wall-clock ns), ns: 0 when the wall clock
/// stepped back between them; `None` without both.
pub fn wake_delay_ns(wall_ns: Option<u64>, at_ns: Option<u64>) -> Option<u64> {
    Some(wall_ns?.saturating_sub(at_ns?))
}

/// A packet's lateness on the wire, samples: at PTP time `now_ns` less its
/// wake delay `wake_ns`, the samples past its first frame `pos` at `rate`
/// (0 for one not yet due then).
pub fn wire_lateness(now_ns: u64, wake_ns: u64, pos: u64, rate: u32) -> u64 {
    clock::to_samples(now_ns.saturating_sub(wake_ns), rate).saturating_sub(pos)
}

/// The box's wall clock now, ns since 1970 (`None` before 1970).
pub(super) fn wall_now_ns() -> Option<u64> {
    u64::try_from(clock::realtime_ns()).ok()
}

/// Asks for a receive buffer of `bytes` on `s`; returns what the kernel
/// gave (Linux doubles what it grants, up to its `rmem_max`; 0 when it
/// cannot say). A refusal leaves the kernel's default.
pub(super) fn set_recv_buffer(s: &UdpSocket, bytes: usize) -> usize {
    let sock = socket2::SockRef::from(s);
    let _refused = sock.set_recv_buffer_size(bytes);
    sock.recv_buffer_size().unwrap_or(0)
}

/// Whether [`join`] binds a group's socket to the port only (Windows binds
/// no multicast address), so it gets every group sent to that port.
pub const GROUP_BOUND_TO_PORT: bool = cfg!(not(target_os = "linux"));

/// A socket that receives the multicast `group` on our address `ip`:
/// bound to the group's port (on Linux to the group itself, so it gets no
/// other group's datagrams to that port; Windows binds no multicast
/// address), shared with other sockets on it (two of our devices on one PC
/// receive one group), non-blocking, the receive buffer asked for, the
/// receive times stamped where the OS can, and a member of the group on
/// `ip`'s interface. Dropping it leaves the group.
pub fn join(ip: Ipv4Addr, group: SocketAddrV4) -> io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    if !group.ip().is_multicast() {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "not a multicast group",
        ));
    }
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    let at = if GROUP_BOUND_TO_PORT {
        Ipv4Addr::UNSPECIFIED
    } else {
        *group.ip()
    };
    s.bind(&SocketAddrV4::new(at, group.port()).into())?;
    s.join_multicast_v4(group.ip(), &ip)?;
    s.set_nonblocking(true)?;
    let s = UdpSocket::from(s);
    crate::winsock::no_connreset(&s)?;
    set_recv_buffer(&s, super::RECV_BUFFER_BYTES);
    stamp_receives(&s);
    Ok(s)
}

#[cfg(test)]
mod tests;
