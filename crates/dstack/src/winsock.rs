// SPDX-License-Identifier: GPL-3.0-or-later

//! Windows' UDP "connection reset" (dasio plan Task 2). After a
//! datagram to a closed port draws an ICMP port-unreachable, Windows fails
//! the socket's next `recv_from` with `WSAECONNRESET`; the device's servers
//! end on a receive error, so one closed peer port would end the device.
//! [`no_connreset`] turns that report off (`SIO_UDP_CONNRESET`) on every
//! UDP socket the device opens; elsewhere it does nothing.

use std::io;

/// Turns off the connection-reset report on the UDP socket `s` (Windows);
/// a no-op elsewhere.
#[cfg(windows)]
pub fn no_connreset(s: &impl std::os::windows::io::AsRawSocket) -> io::Result<()> {
    use windows_sys::Win32::Networking::WinSock::{SIO_UDP_CONNRESET, SOCKET, WSAIoctl};
    let off: u32 = 0;
    let mut returned: u32 = 0;
    // SAFETY: a valid socket, a 4-byte input the call only reads, no output
    // buffer, no overlapped I/O.
    let r = unsafe {
        WSAIoctl(
            s.as_raw_socket() as SOCKET,
            SIO_UDP_CONNRESET,
            (&off as *const u32).cast(),
            std::mem::size_of::<u32>() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
            None,
        )
    };
    if r == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// A no-op off Windows.
#[cfg(not(windows))]
pub fn no_connreset<T>(_s: &T) -> io::Result<()> {
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use std::net::UdpSocket;
    use std::time::Duration;

    use super::no_connreset;

    /// A socket that sent to a closed port on loopback, then a datagram
    /// from a live peer: what its next receive gives.
    fn receive_after_a_closed_port(off: bool) -> std::io::Result<Vec<u8>> {
        let a = UdpSocket::bind("127.0.0.1:0")?;
        if off {
            no_connreset(&a)?;
        }
        let closed = UdpSocket::bind("127.0.0.1:0")?.local_addr()?;
        a.send_to(b"to nobody", closed)?;
        std::thread::sleep(Duration::from_millis(100));
        let live = UdpSocket::bind("127.0.0.1:0")?;
        live.send_to(b"hello", a.local_addr()?)?;
        a.set_read_timeout(Some(Duration::from_secs(1)))?;
        let mut buf = [0u8; 64];
        let (n, _) = a.recv_from(&mut buf)?;
        Ok(buf[..n].to_vec())
    }

    #[test]
    fn windows_reports_a_closed_port_on_the_next_receive() {
        let e = receive_after_a_closed_port(false).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::ConnectionReset, "{e}");
    }

    #[test]
    fn with_the_report_off_the_next_datagram_arrives() {
        assert_eq!(receive_after_a_closed_port(true).unwrap(), b"hello");
    }
}
