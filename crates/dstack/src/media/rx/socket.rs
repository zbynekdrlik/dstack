// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The RX flow sockets' OS side: the receive buffer, the multicast join,
//! and the receive time of each datagram (dstack #234, #381).
//!
//! The RX thread judges a packet's lateness at its pass, when it reads the
//! datagram. A packet late on the wire and a packet that came on time but
//! waited for a late RX-thread wake look the same there. So each flow
//! socket asks the OS to stamp every datagram's receive time, read with
//! the datagram, and the pass reads the stamp's clock once at its start
//! ([`stamp_now_ns`]); the difference is the packet's wake delay
//! ([`wake_delay_ns`]), and its lateness on the wire is its lateness at the
//! pass's PTP time less that delay ([`wire_lateness`]). Only differences of
//! one clock's reads are used, so that clock's offset from PTP does not
//! matter.
//!
//! - Linux: the kernel stamps every datagram (`SO_TIMESTAMPNS`, the wall
//!   clock) and the thread reads it with `recvmsg` (the `SCM_TIMESTAMPNS`
//!   control message). A step of the wall clock between a receive and the
//!   pass reads as a false wake delay (forward) or as 0 (backward, where
//!   the wire lateness is then the lateness at the pass).
//! - Windows (dstack #381, Windows 10 2004 and later): the socket asks for
//!   receive stamps (`SIO_TIMESTAMPING`, `TIMESTAMPING_FLAG_RX`) and reads
//!   with `WSARecvMsg` (the `SO_TIMESTAMP` control message, a QPC count).
//!   Windows stamps only in the NIC's miniport driver: a software stamp on
//!   QPC when the driver supports it and its software timestamping is on
//!   (`*SoftwareTimestamp`, off by default), or a hardware stamp in the
//!   NIC's own clock. So a box takes the stamps only when the interface's
//!   ACTIVE capabilities say software receive stamps and no hardware ones
//!   ([`stamping_of`]), and a stamp counts only within `STAMP_WINDOW_NS`
//!   before its read on QPC (`qpc_stamp_ns`); the stamp clock there is the
//!   app's raw QPC ([`crate::clock::app_monotonic_ns`]). Loopback has no
//!   miniport and is never stamped.
//!
//! Where a box gets no stamp ([`Stamping`], the status's `rx_stamps`),
//! [`super::RxShared::stamps`] is false: no wake delay and no wire
//! lateness are measured, the pass reads no stamp clock, and the arrival
//! ages fall back to the read's bracket (dstack #350). A datagram that
//! comes without a stamp on a stamping box is bracketed alone.

use std::io::{self, ErrorKind};
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};

use crate::clock;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(not(any(target_os = "linux", windows)))]
mod other;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub(super) use linux::{handle_of, recv_handle, recv_stamped, stamp_receives, stamping};
#[cfg(not(any(target_os = "linux", windows)))]
pub(super) use other::{handle_of, recv_handle, recv_stamped, stamp_receives, stamping};
#[cfg(windows)]
pub(super) use windows::{handle_of, recv_handle, recv_stamped, stamp_receives, stamping};

// The clock the OS stamps a receive in (dstack #381): an alias, so there
// is no body to mutate here; each OS's choice is its own.
#[cfg(windows)]
use crate::clock::app_monotonic_ns as stamp_clock_ns;
#[cfg(not(windows))]
use crate::clock::realtime_ns as stamp_clock_ns;

/// One datagram read from a flow socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Received {
    /// Its bytes in the buffer.
    pub len: usize,
    /// Its sender; `None` for any address that is no IPv4 one.
    pub from: Option<SocketAddrV4>,
    /// When the OS received it, ns on the stamp clock ([`stamp_now_ns`]:
    /// Linux's wall clock, Windows' QPC); `None` without a stamp (a box or
    /// a datagram the OS stamps nothing on).
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

/// Whether a packet was already late when the kernel received it: its
/// [`wire_lateness`] past `limit`, the rule `late` uses (the flow's playout
/// plus the pump's block, samples).
pub fn wire_late(now_ns: u64, wake_ns: u64, pos: u64, rate: u32, limit: u64) -> bool {
    wire_lateness(now_ns, wake_ns, pos, rate) > limit
}

/// The stamp clock now, ns: the clock the OS stamps a receive in (Linux:
/// the wall clock, since 1970; Windows: the app's raw QPC, dstack #381);
/// `None` for a negative reading.
pub(super) fn stamp_now_ns() -> Option<u64> {
    u64::try_from(stamp_clock_ns()).ok()
}

/// Where a box's flow sockets get their datagrams' receive times, or why
/// they get none (dstack #381; the status's `rx_stamps`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stamping {
    /// The kernel stamps every receive (Linux, `SO_TIMESTAMPNS`).
    Kernel,
    /// The NIC's driver stamps every receive in software, on QPC (Windows,
    /// `SIO_TIMESTAMPING` with the driver's software timestamping on).
    Nic,
    /// The OS refused to stamp a socket (Windows before 10 2004).
    Refused,
    /// The interface's driver stamps no receive (Windows: its software
    /// timestamping is off or unsupported, or no adapter holds the
    /// address).
    NicOff,
    /// The NIC stamps receives in its own hardware clock, which dstack does
    /// not read (Windows: the adapter's hardware timestamping is on).
    NicClock,
}

impl Stamping {
    /// Whether the datagrams carry receive times dstack takes.
    pub fn on(self) -> bool {
        matches!(self, Stamping::Kernel | Stamping::Nic)
    }

    /// Its status word (`rx_stamps`).
    pub fn word(self) -> &'static str {
        match self {
            Stamping::Kernel => "kernel",
            Stamping::Nic => "nic",
            Stamping::Refused => "refused",
            Stamping::NicOff => "nic_off",
            Stamping::NicClock => "nic_clock",
        }
    }
}

/// An interface's active receive-stamp capabilities (Windows'
/// `GetInterfaceActiveTimestampCapabilities`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NicStamps {
    /// The driver stamps every receive in software (QPC).
    pub software_rx: bool,
    /// The NIC stamps every receive in its hardware clock.
    pub hardware_rx: bool,
}

/// A box's [`Stamping`]: `accepted` when every flow socket took the stamp
/// request; then `kernel` when the kernel stamps (Linux), else the
/// interface's active capabilities `nic` decide (`None`: not read, or no
/// adapter holds the address). A hardware clock wins over a software one:
/// Windows then stamps in the NIC's.
pub fn stamping_of(kernel: bool, accepted: bool, nic: Option<NicStamps>) -> Stamping {
    match (accepted, kernel, nic) {
        (false, _, _) => Stamping::Refused,
        (true, true, _) => Stamping::Kernel,
        (true, false, Some(n)) if n.hardware_rx => Stamping::NicClock,
        (true, false, Some(n)) if n.software_rx => Stamping::Nic,
        (true, false, _) => Stamping::NicOff,
    }
}

/// How long before its read a Windows receive stamp may be, ns: an older
/// one is not taken as a QPC count of ours (the NIC's hardware clock, or a
/// datagram that waited over a second, which the bracket judges).
#[cfg(any(windows, test))]
pub const STAMP_WINDOW_NS: u64 = 1_000_000_000;

/// A Windows receive stamp `stamp` (QPC ticks) read when QPC was `now`, at
/// `frequency` ticks a second, as ns on the stamp clock
/// ([`crate::clock::app_monotonic_ns`]'s scale); `None` for a stamp after
/// the read or more than [`STAMP_WINDOW_NS`] before it.
#[cfg(any(windows, test))]
pub fn qpc_stamp_ns(stamp: u64, now: i64, frequency: i64) -> Option<u64> {
    let stamp = i64::try_from(stamp).ok()?;
    let ago = u64::try_from(clock::qpc_ns_of(now.checked_sub(stamp)?, frequency)).ok()?;
    if ago > STAMP_WINDOW_NS {
        return None;
    }
    u64::try_from(clock::qpc_ns_of(stamp, frequency)).ok()
}

/// Windows' `SOL_SOCKET`, a control message's level.
#[cfg(any(windows, test))]
pub const WIN_SOL_SOCKET: i32 = 0xffff;
/// Windows' `SO_TIMESTAMP` control message type (0x300A, `mstcpip.h`).
#[cfg(any(windows, test))]
pub const WIN_SO_TIMESTAMP: i32 = 0x300a;
/// A control message's header on 64-bit Windows (`WSACMSGHDR`: `cmsg_len`
/// a `SIZE_T`, then the level and the type), bytes; the data follows it.
#[cfg(any(windows, test))]
pub const WIN_CMSG_HEADER: usize = 16;
/// The control messages' alignment on 64-bit Windows (`SIZE_T`): the next
/// header starts at the length rounded up to it.
#[cfg(any(windows, test))]
pub const WIN_CMSG_ALIGN: usize = 8;

/// The receive stamp (a QPC count) in the control messages `control` that
/// `WSARecvMsg` wrote (its `Control.len` bytes): the first whole
/// `SOL_SOCKET`/`SO_TIMESTAMP` message's `UINT64`; `None` without one, or
/// at a header whose length cannot hold itself (the walk stops there).
#[cfg(any(windows, test))]
pub fn qpc_stamp_of(control: &[u8]) -> Option<u64> {
    let bytes = |at: usize, n: usize| control.get(at..at.checked_add(n)?);
    let mut at = 0;
    while let Some(h) = bytes(at, WIN_CMSG_HEADER) {
        let len = u64::from_le_bytes(h[..8].try_into().ok()?);
        let level = i32::from_le_bytes(h[8..12].try_into().ok()?);
        let kind = i32::from_le_bytes(h[12..].try_into().ok()?);
        let len = usize::try_from(len)
            .ok()
            .filter(|&l| l >= WIN_CMSG_HEADER)?;
        if level == WIN_SOL_SOCKET && kind == WIN_SO_TIMESTAMP && len >= WIN_CMSG_HEADER + 8 {
            let data = bytes(at + WIN_CMSG_HEADER, 8)?;
            return Some(u64::from_le_bytes(data.try_into().ok()?));
        }
        at = at.checked_add(len.checked_next_multiple_of(WIN_CMSG_ALIGN)?)?;
    }
    None
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
