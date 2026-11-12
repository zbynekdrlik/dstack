// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The kernel's receive stamp on Linux (dstack #234): `SO_TIMESTAMPNS` on
//! the socket, `recvmsg` with the `SCM_TIMESTAMPNS` control message.

use std::io;
use std::mem::{size_of, zeroed};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::os::fd::AsRawFd;

use super::Received;

/// Asks the kernel to stamp every datagram `s` receives with its receive
/// time (`SO_TIMESTAMPNS`, the wall clock); `true` when it will.
pub fn stamp_receives(s: &impl AsRawFd) -> bool {
    let one: libc::c_int = 1;
    // SAFETY: a socket's own descriptor, and an int option of its size.
    let set = unsafe {
        libc::setsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_TIMESTAMPNS,
            (&one as *const libc::c_int).cast(),
            size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    set == 0
}

/// Whether a control message of `level` and `kind` is the receive stamp.
pub fn is_stamp(level: libc::c_int, kind: libc::c_int) -> bool {
    level == libc::SOL_SOCKET && kind == libc::SCM_TIMESTAMPNS
}

/// The IPv4 sender in `addr` when the kernel wrote `len` bytes of it.
pub fn sender_of(addr: &libc::sockaddr_in, len: libc::socklen_t) -> Option<SocketAddrV4> {
    let whole = len as usize >= size_of::<libc::sockaddr_in>();
    (whole && i32::from(addr.sin_family) == libc::AF_INET).then(|| {
        SocketAddrV4::new(
            Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr)),
            u16::from_be(addr.sin_port),
        )
    })
}

/// Wall-clock ns since 1970 of a `timespec` (`sec`, `nsec`); `None` for a
/// time before 1970 or a field out of range.
pub fn timespec_ns(sec: i64, nsec: i64) -> Option<u64> {
    let nsec = u64::try_from(nsec).ok().filter(|&n| n < 1_000_000_000)?;
    u64::try_from(sec)
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(nsec)
}

/// The control messages' room: one stamp (a `timespec`) with its header,
/// and more, aligned as a `cmsghdr`.
#[repr(C, align(8))]
struct Control([u8; 64]);

/// Reads one datagram of `s` into `buf` with its kernel receive time (the
/// hot path: one `recvmsg`, no allocation). A datagram longer than `buf`
/// is cut, as `recv_from` cuts it.
pub fn recv_stamped(s: &impl AsRawFd, buf: &mut [u8]) -> io::Result<Received> {
    // SAFETY: all-zero is a valid sockaddr_in and msghdr.
    let mut addr: libc::sockaddr_in = unsafe { zeroed() };
    let mut control = Control([0; 64]);
    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr().cast(),
        iov_len: buf.len(),
    };
    let mut msg: libc::msghdr = unsafe { zeroed() };
    msg.msg_name = (&mut addr as *mut libc::sockaddr_in).cast();
    msg.msg_namelen = size_of::<libc::sockaddr_in>() as libc::socklen_t;
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.0.as_mut_ptr().cast();
    msg.msg_controllen = control.0.len() as _;
    // SAFETY: every pointer in `msg` is to a live buffer of the length it
    // states, for the call's duration.
    let n = unsafe { libc::recvmsg(s.as_raw_fd(), &mut msg, 0) };
    let len = usize::try_from(n).map_err(|_| io::Error::last_os_error())?;
    let mut at_ns = None;
    // SAFETY: the kernel wrote `msg_controllen` bytes of whole control
    // messages into `control`; the CMSG walk stays inside them, and the
    // stamp's data is read unaligned.
    unsafe {
        let mut c = libc::CMSG_FIRSTHDR(&msg);
        while !c.is_null() {
            if is_stamp((*c).cmsg_level, (*c).cmsg_type) {
                let ts: libc::timespec = std::ptr::read_unaligned(libc::CMSG_DATA(c).cast());
                at_ns = timespec_ns(ts.tv_sec, ts.tv_nsec);
            }
            c = libc::CMSG_NXTHDR(&msg, c);
        }
    }
    Ok(Received {
        len,
        from: sender_of(&addr, msg.msg_namelen),
        at_ns,
    })
}
