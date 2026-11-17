// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The receive path on Windows (dstack #381): each flow socket asks for
//! receive stamps (`SIO_TIMESTAMPING`, `TIMESTAMPING_FLAG_RX`) and every
//! read is one `WSARecvMsg` with its `SO_TIMESTAMP` control message (a QPC
//! count). Thin glue: the control message walk ([`super::qpc_stamp_of`]),
//! the stamp's check and scale ([`super::qpc_stamp_ns`]) and the box's
//! verdict ([`super::stamping_of`]) are pure functions tested on Linux.
//! Windows-only, so outside the mutation gate (`.cargo/mutants.toml`); CI
//! builds it and runs every RX test through it on Windows.

use std::io;
use std::mem::{ManuallyDrop, size_of};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::os::windows::io::{AsRawSocket, FromRawSocket};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::NO_ERROR;
use windows_sys::Win32::NetworkManagement::IpHelper::{
    ConvertInterfaceIndexToLuid, INTERFACE_TIMESTAMP_CAPABILITIES,
};
use windows_sys::Win32::NetworkManagement::Ndis::NET_LUID_LH;
use windows_sys::Win32::Networking::WinSock::{
    AF_INET, CMSGHDR, LPFN_WSARECVMSG, SIO_GET_EXTENSION_FUNCTION_POINTER, SIO_TIMESTAMPING,
    SO_TIMESTAMP, SOCKADDR, SOCKADDR_IN, SOCKET, SOCKET_ERROR, SOL_SOCKET, TIMESTAMPING_CONFIG,
    TIMESTAMPING_FLAG_RX, WSABUF, WSAGetLastError, WSAID_WSARECVMSG, WSAIoctl, WSAMSG,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

use super::{
    NicStamps, Received, Stamping, WIN_CMSG_HEADER, WIN_SO_TIMESTAMP, WIN_SOL_SOCKET, qpc_stamp_ns,
    qpc_stamp_of, stamping_of,
};

// The pure walk's layout is the platform's.
const _: () = assert!(size_of::<CMSGHDR>() == WIN_CMSG_HEADER);
const _: () = assert!(SOL_SOCKET == WIN_SOL_SOCKET);
const _: () = assert!(SO_TIMESTAMP as i32 == WIN_SO_TIMESTAMP);

/// A flow socket's OS handle as one word for the writer's handle table
/// (dstack #326): the handle plus 1, so 0 is none.
pub fn handle_of(s: &impl AsRawSocket) -> u64 {
    s.as_raw_socket().wrapping_add(1)
}

/// `WSARecvMsg`, found once through the first socket that asks for stamps
/// (an extension function: `SIO_GET_EXTENSION_FUNCTION_POINTER`); `None`
/// when the provider has none, and every read is then `recv_from`.
static RECV_MSG: OnceLock<LPFN_WSARECVMSG> = OnceLock::new();

/// Finds `WSARecvMsg` through the socket `s` (once a process).
fn find_recv_msg(s: SOCKET) -> LPFN_WSARECVMSG {
    *RECV_MSG.get_or_init(|| {
        let mut f: LPFN_WSARECVMSG = None;
        let mut returned = 0u32;
        // SAFETY: a valid socket; the input is the GUID the call reads, the
        // output a function pointer of its size; no overlapped I/O.
        let r = unsafe {
            WSAIoctl(
                s,
                SIO_GET_EXTENSION_FUNCTION_POINTER,
                (&WSAID_WSARECVMSG as *const windows_sys::core::GUID).cast(),
                size_of::<windows_sys::core::GUID>() as u32,
                (&mut f as *mut LPFN_WSARECVMSG).cast(),
                size_of::<LPFN_WSARECVMSG>() as u32,
                &mut returned,
                std::ptr::null_mut(),
                None,
            )
        };
        if r == 0 { f } else { None }
    })
}

/// Asks the OS to stamp every datagram `s` receives (`SIO_TIMESTAMPING`
/// with `TIMESTAMPING_FLAG_RX`); `true` when it took the request and
/// `WSARecvMsg` is there to read the stamps. Windows before 10 2004 refuses
/// the request. Whether a datagram then carries a stamp is the NIC
/// driver's ([`stamping`]).
pub fn stamp_receives(s: &impl AsRawSocket) -> bool {
    let sock = s.as_raw_socket() as SOCKET;
    let config = TIMESTAMPING_CONFIG {
        Flags: TIMESTAMPING_FLAG_RX,
        TxTimestampsBuffered: 0,
    };
    let mut returned = 0u32;
    // SAFETY: a valid socket, an input the call only reads, no output, no
    // overlapped I/O.
    let r = unsafe {
        WSAIoctl(
            sock,
            SIO_TIMESTAMPING,
            (&config as *const TIMESTAMPING_CONFIG).cast(),
            size_of::<TIMESTAMPING_CONFIG>() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
            None,
        )
    };
    r == 0 && find_recv_msg(sock).is_some()
}

/// `GetInterfaceActiveTimestampCapabilities` (Windows 10 2004 and later),
/// found at run time so the binaries still start on an older Windows.
type ActiveCaps =
    unsafe extern "system" fn(*const NET_LUID_LH, *mut INTERFACE_TIMESTAMP_CAPABILITIES) -> u32;

/// The receive-stamp capabilities active on the interface that holds `ip`;
/// `None` when no adapter holds it or Windows cannot say.
fn nic_stamps(ip: Ipv4Addr) -> Option<NicStamps> {
    let (_, _, _, index, _) = crate::host::windows::win_nic::of_with_index(ip).ok()?;
    let mut luid = NET_LUID_LH { Value: 0 };
    // SAFETY: a writable LUID.
    if unsafe { ConvertInterfaceIndexToLuid(index, &mut luid) } != NO_ERROR {
        return None;
    }
    let dll: Vec<u16> = "iphlpapi.dll\0".encode_utf16().collect();
    // SAFETY: a NUL-terminated wide name; the module stays loaded (a
    // system DLL this binary already imports from).
    let module = unsafe { LoadLibraryW(dll.as_ptr()) };
    if module.is_null() {
        return None;
    }
    // SAFETY: a loaded module and a NUL-terminated ANSI name.
    let found = unsafe {
        GetProcAddress(
            module,
            c"GetInterfaceActiveTimestampCapabilities".as_ptr().cast(),
        )
    }?;
    // SAFETY: the export's documented signature.
    let active: ActiveCaps = unsafe { std::mem::transmute(found) };
    let mut caps = INTERFACE_TIMESTAMP_CAPABILITIES::default();
    // SAFETY: a valid LUID and a writable capabilities struct.
    if unsafe { active(&luid, &mut caps) } != NO_ERROR {
        return None;
    }
    Some(NicStamps {
        software_rx: caps.SoftwareCapabilities.AllReceive,
        hardware_rx: caps.HardwareCapabilities.AllReceive,
    })
}

/// The box's [`Stamping`] for flow sockets on our address `ip`, `accepted`
/// when every one took [`stamp_receives`]: the NIC driver's active
/// capabilities decide ([`stamping_of`]).
pub fn stamping(ip: Ipv4Addr, accepted: bool) -> Stamping {
    stamping_of(false, accepted, accepted.then(|| nic_stamps(ip)).flatten())
}

/// QPC now, ticks, and its frequency (fixed at boot).
fn qpc_now() -> (i64, i64) {
    static FREQ: OnceLock<i64> = OnceLock::new();
    let frequency = *FREQ.get_or_init(|| {
        let mut f = 0i64;
        // SAFETY: a valid, writable i64.
        unsafe { QueryPerformanceFrequency(&mut f) };
        f.max(1)
    });
    let mut t = 0i64;
    // SAFETY: a valid, writable i64.
    unsafe { QueryPerformanceCounter(&mut t) };
    (t, frequency)
}

/// The control messages' room: one stamp (a header and a `UINT64`) and
/// more, aligned as a `CMSGHDR`.
#[repr(C, align(8))]
struct Control([u8; 64]);

/// Reads one datagram of the socket `sock` with `WSARecvMsg` and its
/// receive stamp (the hot path: one call, no allocation; a QPC read only
/// for a stamped datagram); `recv_from` when `WSARecvMsg` was not found.
fn recv_msg(sock: SOCKET, buf: &mut [u8]) -> io::Result<Received> {
    let Some(recv) = RECV_MSG.get().copied().flatten() else {
        return recv_plain(sock, buf);
    };
    let mut addr = SOCKADDR_IN::default();
    let mut control = Control([0; 64]);
    let mut data = WSABUF {
        len: u32::try_from(buf.len()).unwrap_or(u32::MAX),
        buf: buf.as_mut_ptr(),
    };
    let mut msg = WSAMSG {
        name: (&mut addr as *mut SOCKADDR_IN).cast::<SOCKADDR>(),
        namelen: size_of::<SOCKADDR_IN>() as i32,
        lpBuffers: &mut data,
        dwBufferCount: 1,
        Control: WSABUF {
            len: control.0.len() as u32,
            buf: control.0.as_mut_ptr(),
        },
        dwFlags: 0,
    };
    let mut n = 0u32;
    // SAFETY: every pointer in `msg` is to a live buffer of the length it
    // states, for the call's duration; no overlapped I/O.
    let r = unsafe { recv(sock, &mut msg, &mut n, std::ptr::null_mut(), None) };
    if r == SOCKET_ERROR {
        // SAFETY: reads this thread's last socket error.
        return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }));
    }
    let written = (msg.Control.len as usize).min(control.0.len());
    let at_ns = qpc_stamp_of(&control.0[..written]).and_then(|stamp| {
        let (now, frequency) = qpc_now();
        qpc_stamp_ns(stamp, now, frequency)
    });
    let whole = usize::try_from(msg.namelen).is_ok_and(|l| l >= size_of::<SOCKADDR_IN>());
    let from = (whole && addr.sin_family == AF_INET).then(|| {
        // SAFETY: the IPv4 union member of an AF_INET address.
        let raw = unsafe { addr.sin_addr.S_un.S_addr };
        SocketAddrV4::new(
            Ipv4Addr::from(u32::from_be(raw)),
            u16::from_be(addr.sin_port),
        )
    });
    Ok(Received {
        len: n as usize,
        from,
        at_ns,
    })
}

/// Reads one datagram of `sock` with `recv_from`, with no receive time.
fn recv_plain(sock: SOCKET, buf: &mut [u8]) -> io::Result<Received> {
    // SAFETY: `sock` is a socket the RX thread keeps open for the call
    // (`super::super::handles`); `ManuallyDrop` never closes it.
    let s = ManuallyDrop::new(unsafe { UdpSocket::from_raw_socket(sock as u64) });
    let (len, from) = s.recv_from(buf)?;
    Ok(Received {
        len,
        from: match from {
            SocketAddr::V4(a) => Some(a),
            SocketAddr::V6(_) => None,
        },
        at_ns: None,
    })
}

/// Reads one datagram of a socket the RX thread's poll registered, with its
/// receive stamp: through mio's `try_io`, which re-arms the socket's
/// readiness on Windows when the read finds it empty.
pub fn recv_stamped(s: &mio::net::UdpSocket, buf: &mut [u8]) -> io::Result<Received> {
    s.try_io(|| recv_msg(s.as_raw_socket() as SOCKET, buf))
}

/// Reads one datagram of the socket whose [`handle_of`] is `handle`, with
/// its receive stamp; an invalid handle (0) is an `InvalidInput` error.
/// For a socket no poll registered (the pump's drain, dstack #326).
pub fn recv_handle(handle: u64, buf: &mut [u8]) -> io::Result<Received> {
    let raw = handle
        .checked_sub(1)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    recv_msg(raw as SOCKET, buf)
}
