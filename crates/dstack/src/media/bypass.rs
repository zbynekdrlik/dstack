//! Audio TX past the qdisc on Linux (dstack #1, gen3 on the N100 box,
//! 1.11.2026). A NIC with one TX queue (r8169) shares its qdisc with every
//! other sender on the box. On PREEMPT_RT the sender that holds the qdisc's
//! running state can be preempted while our packet waits behind it: with a
//! 300 kB TCP burst or an ssh login on the box, the IEM card read peaks of
//! 250 to 1800 µs and late packets at 250 µs, while our own counters showed
//! every send returning within 116 µs of its stamp. The wait came after
//! `send_to`, so no counter of ours could see it.
//!
//! An `AF_PACKET` socket with `PACKET_QDISC_BYPASS` hands the frame to the
//! driver directly. The kernel writes the Ethernet header from the
//! destination MAC; we write IPv4 and UDP ([`write_headers`]) into
//! [`HEADROOM`] bytes in front of the media packet, so nothing is copied.
//!
//! What goes direct: a unicast flow to a neighbour whose MAC the kernel has
//! resolved ([`arp_mac`]; the control tick reads `/proc/net/arp`). Multicast
//! stays on the UDP socket, so local members still hear it. A destination
//! with no complete ARP entry also stays on the UDP socket, which lets the
//! kernel resolve it. Opening the socket needs `CAP_NET_RAW`; without it the
//! device warns at start and every flow uses the UDP socket.
//!
//! On Windows (dstack #1, the FOH PC's send stalls below Winsock) the host
//! application may hand dstack a [`FrameSender`]: its own kernel send path
//! (an NDIS filter, dasio's). dstack then writes the whole Ethernet II frame
//! ([`write_ethernet`] in front of [`write_headers`]) and passes the frames
//! due in a TX pass to the sender in one call; the host only moves bytes.
//! The same rule picks the flows: a unicast destination whose MAC the
//! Windows neighbour table holds ([`neighbour_row`], read once a second off
//! the hot path) goes to the sender, multicast and unresolved destinations
//! stay on the UDP socket, and a frame the sender did not take goes out on
//! the UDP socket after all (`media::tx::host`).

use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};

/// The IPv4 and UDP headers written in front of a media packet.
pub const HEADROOM: usize = IP_HEADER + UDP_HEADER;
/// The Ethernet II header in front of a host sender's frame
/// ([`write_ethernet`]).
pub const ETHERNET_HEADER: usize = 14;
/// Every header in front of a media packet in a host sender's frame.
pub const FRAME_HEADROOM: usize = ETHERNET_HEADER + HEADROOM;
/// Ethernet II's type of an IPv4 frame.
const ETHERTYPE_IPV4: u16 = 0x0800;
/// The Windows neighbour states with a MAC (`NL_NEIGHBOR_STATE`): probe,
/// delay, stale, reachable, permanent. Unreachable (0) and incomplete (1)
/// have none.
const NEIGHBOUR_RESOLVED: std::ops::RangeInclusive<i32> = 2..=6;
const IP_HEADER: usize = 20;
const UDP_HEADER: usize = 8;
/// The TTL of a direct packet (Linux's default).
pub const TTL: u8 = 64;
/// IPv4 "don't fragment": a media packet always fits one frame.
const DONT_FRAGMENT: u16 = 0x4000;
const UDP: u8 = 17;
/// `/proc/net/arp`'s flag for a complete entry (`ATF_COM`).
const ARP_COMPLETE: u32 = 0x2;
/// The bit that marks a MAC present in a [`mac_word`].
const MAC_PRESENT: u64 = 1 << 48;
const _: () = assert!(MAC_PRESENT == u64::from_be_bytes([0, 1, 0, 0, 0, 0, 0, 0]));

/// Writes the IPv4 header (DSCP `tos`, don't fragment, `id`) and the UDP
/// header from `src` to `dst` into `buf[..HEADROOM]`, for the payload of
/// `len` bytes already in `buf[HEADROOM..]`. Returns the frame's length.
/// `buf` must hold `HEADROOM + len` bytes.
pub fn write_headers(
    buf: &mut [u8],
    src: SocketAddrV4,
    dst: SocketAddrV4,
    tos: u8,
    id: u16,
    len: usize,
) -> usize {
    let total = HEADROOM + len;
    // A media packet is at most a datagram (`wire::MAX_DATAGRAM`), so both
    // lengths fit 16 bits.
    let ip_len = total as u16;
    let udp_len = (UDP_HEADER + len) as u16;
    let (s, d) = (src.ip().octets(), dst.ip().octets());
    let ip = &mut buf[..IP_HEADER];
    ip[0] = 0x45;
    ip[1] = tos;
    ip[2..4].copy_from_slice(&ip_len.to_be_bytes());
    ip[4..6].copy_from_slice(&id.to_be_bytes());
    ip[6..8].copy_from_slice(&DONT_FRAGMENT.to_be_bytes());
    ip[8] = TTL;
    ip[9] = UDP;
    ip[10..12].fill(0);
    ip[12..16].copy_from_slice(&s);
    ip[16..20].copy_from_slice(&d);
    let sum = checksum(sum16(ip, 0));
    ip[10..12].copy_from_slice(&sum.to_be_bytes());
    let udp = &mut buf[IP_HEADER..total];
    udp[0..2].copy_from_slice(&src.port().to_be_bytes());
    udp[2..4].copy_from_slice(&dst.port().to_be_bytes());
    udp[4..6].copy_from_slice(&udp_len.to_be_bytes());
    udp[6..8].fill(0);
    let pseudo = u32::from(u16::from_be_bytes([s[0], s[1]]))
        + u32::from(u16::from_be_bytes([s[2], s[3]]))
        + u32::from(u16::from_be_bytes([d[0], d[1]]))
        + u32::from(u16::from_be_bytes([d[2], d[3]]))
        + u32::from(UDP)
        + u32::from(udp_len);
    // A computed 0 is sent as all ones (RFC 768): 0 means "no checksum".
    let sum = match checksum(sum16(udp, pseudo)) {
        0 => 0xffff,
        s => s,
    };
    udp[6..8].copy_from_slice(&sum.to_be_bytes());
    total
}

/// Writes the Ethernet II header of an IPv4 frame from `src` (our NIC's
/// MAC) to `dst` into `buf[..ETHERNET_HEADER]`. `buf` must hold
/// [`ETHERNET_HEADER`] bytes.
pub fn write_ethernet(buf: &mut [u8], dst: [u8; 6], src: [u8; 6]) {
    buf[..6].copy_from_slice(&dst);
    buf[6..12].copy_from_slice(&src);
    buf[12..ETHERNET_HEADER].copy_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
}

/// A host's kernel send path (dstack #1): the application hands it to the
/// device before its media threads start (`Media::set_frame_sender`), and
/// the TX thread passes it every frame due in a pass, in one call. Used on
/// Windows only; without one, every flow goes through its UDP socket there.
///
/// Each frame is a complete Ethernet II frame of one media packet: our NIC's
/// MAC as the source, IPv4 (DSCP EF, don't fragment, TTL 64) and UDP, all
/// written by dstack. The sender queues them on the NIC in order.
pub trait FrameSender: Send + Sync {
    /// Sends `frames`, marking in `sent` (as long as `frames`, all `false`
    /// on the call) each frame it took. A frame not marked goes out through
    /// the UDP socket after all. `Err`: the whole batch was refused (none
    /// taken, whatever `sent` holds). Called on the TX thread, once a pass:
    /// it must not block, and should not allocate.
    fn send(&self, frames: &[&[u8]], sent: &mut [bool]) -> io::Result<()>;
}

/// One resolved entry of the Windows neighbour table (`MIB_IPNET_ROW2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Neighbour {
    pub ip: Ipv4Addr,
    pub mac: [u8; 6],
    /// The interface it was resolved on (`InterfaceIndex`).
    pub ifindex: u32,
}

/// The neighbour of a table row: `ip`, its physical address `phys` (the
/// row's bytes up to its length), its `NL_NEIGHBOR_STATE` and the
/// interface. `None` for a row without a MAC: a state that has none
/// (unreachable, incomplete), an address that is not 6 bytes, or all zero.
pub fn neighbour_row(ip: Ipv4Addr, phys: &[u8], state: i32, ifindex: u32) -> Option<Neighbour> {
    let mac: [u8; 6] = phys.try_into().ok()?;
    (NEIGHBOUR_RESOLVED.contains(&state) && mac != [0; 6]).then_some(Neighbour { ip, mac, ifindex })
}

/// The MAC of `ip` on the interface `ifindex` among `rows`.
pub fn neighbour_mac(rows: &[Neighbour], ip: Ipv4Addr, ifindex: u32) -> Option<[u8; 6]> {
    rows.iter()
        .find(|n| n.ip == ip && n.ifindex == ifindex)
        .map(|n| n.mac)
}

/// [`mac_words`] from the Windows neighbour `rows` on `ifindex`.
pub fn neighbour_mac_words<'a>(
    rows: &'a [Neighbour],
    ifindex: u32,
    dests: impl Iterator<Item = Option<SocketAddrV4>> + 'a,
) -> impl Iterator<Item = u64> + 'a {
    words_by(dests, move |ip| neighbour_mac(rows, ip, ifindex))
}

/// A MAC written `aa:bb:cc:dd:ee:ff` (hex, either case).
pub fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let mut out = [0u8; 6];
    let mut parts = text.split(':');
    for b in &mut out {
        let part = parts.next()?;
        // Two hex digits: `from_str_radix` alone takes a sign and one digit.
        if part.len() != 2 || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        *b = u8::from_str_radix(part, 16).ok()?;
    }
    parts.next().is_none().then_some(out)
}

/// The one's complement sum of `data` as 16-bit big-endian words (an odd
/// last byte padded with zero), added to `sum`, not yet folded.
fn sum16(data: &[u8], sum: u32) -> u32 {
    let (words, rest) = data.as_chunks::<2>();
    let mut sum = words
        .iter()
        .fold(sum, |s, &w| s + u32::from(u16::from_be_bytes(w)));
    if let [last] = rest {
        sum += u32::from(*last) << 8;
    }
    sum
}

/// The Internet checksum of a [`sum16`]: carries folded in, complemented.
fn checksum(mut sum: u32) -> u16 {
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// The MAC of `ip` on the interface `dev` in `table` (the text of
/// `/proc/net/arp`), when its entry is complete; `None` otherwise.
pub fn arp_mac(table: &str, ip: Ipv4Addr, dev: &str) -> Option<[u8; 6]> {
    table.lines().skip(1).find_map(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        let [addr, _, flags, mac, _, device] = f[..] else {
            return None;
        };
        let flags = u32::from_str_radix(flags.trim_start_matches("0x"), 16).ok()?;
        if addr.parse::<Ipv4Addr>().ok()? != ip || device != dev || flags & ARP_COMPLETE == 0 {
            return None;
        }
        let mut out = [0u8; 6];
        let mut parts = mac.split(':');
        for b in &mut out {
            *b = u8::from_str_radix(parts.next()?, 16).ok()?;
        }
        (parts.next().is_none() && out != [0; 6]).then_some(out)
    })
}

/// The direct MAC of each destination in `dests` (one per TX slot): a
/// unicast destination with a complete entry in `table` on `dev`, as a
/// [`mac_word`]; 0 for the others.
pub fn mac_words<'a>(
    table: &'a str,
    dev: &'a str,
    dests: impl Iterator<Item = Option<SocketAddrV4>> + 'a,
) -> impl Iterator<Item = u64> + 'a {
    words_by(dests, move |ip| arp_mac(table, ip, dev))
}

/// The [`mac_word`] of each destination: `lookup`'s MAC for a unicast
/// one, 0 for multicast and none.
fn words_by<'a>(
    dests: impl Iterator<Item = Option<SocketAddrV4>> + 'a,
    lookup: impl Fn(Ipv4Addr) -> Option<[u8; 6]> + 'a,
) -> impl Iterator<Item = u64> + 'a {
    dests.map(move |dest| {
        mac_word(
            dest.filter(|d| !d.ip().is_multicast())
                .and_then(|d| lookup(*d.ip())),
        )
    })
}

/// `mac` as one word for a seqlock cell: 0 for none.
pub fn mac_word(mac: Option<[u8; 6]>) -> u64 {
    mac.map_or(0, |m| {
        // The present bit (48) is byte 1; the MAC the low six bytes.
        let mut w = [0, 1, 0, 0, 0, 0, 0, 0];
        w[2..].copy_from_slice(&m);
        u64::from_be_bytes(w)
    })
}

/// The MAC a [`mac_word`] holds.
pub fn word_mac(w: u64) -> Option<[u8; 6]> {
    (w & MAC_PRESENT != 0).then(|| {
        let b = w.to_be_bytes();
        [b[2], b[3], b[4], b[5], b[6], b[7]]
    })
}

/// The interface that holds `ip`.
pub fn nic_of(ip: Ipv4Addr) -> Option<String> {
    if_addrs::get_if_addrs()
        .ok()?
        .into_iter()
        .find(|i| i.ip() == std::net::IpAddr::V4(ip))
        .map(|i| i.name)
}

/// A socket that sends frames past the qdisc of one interface.
#[derive(Debug)]
pub struct Direct {
    #[cfg(target_os = "linux")]
    fd: std::os::fd::OwnedFd,
    #[cfg(target_os = "linux")]
    ifindex: i32,
}

/// Linux's `PACKET_QDISC_BYPASS` (not in the libc crate).
#[cfg(target_os = "linux")]
const PACKET_QDISC_BYPASS: libc::c_int = 20;

impl Direct {
    /// The socket on interface `nic`: refused without `CAP_NET_RAW`
    /// (`PermissionDenied`), and off Linux (`Unsupported`).
    #[cfg(target_os = "linux")]
    pub fn open(nic: &str) -> io::Result<Direct> {
        use std::os::fd::FromRawFd;
        let name = std::ffi::CString::new(nic)?;
        // SAFETY: a NUL-terminated name the call only reads.
        let ifindex = unsafe { libc::if_nametoindex(name.as_ptr()) };
        if ifindex == 0 {
            return Err(io::Error::last_os_error());
        }
        // Protocol 0: the socket receives nothing; each send names its own.
        // SAFETY: plain arguments; the result is checked.
        let raw = unsafe {
            libc::socket(
                libc::AF_PACKET,
                libc::SOCK_DGRAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                0,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a socket just opened, owned by nobody else.
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        let one: libc::c_int = 1;
        // SAFETY: `one` outlives the call, which reads its size.
        let r = unsafe {
            libc::setsockopt(
                std::os::fd::AsRawFd::as_raw_fd(&fd),
                libc::SOL_PACKET,
                PACKET_QDISC_BYPASS,
                (&one as *const libc::c_int).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        if r != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Direct {
            fd,
            ifindex: ifindex as i32,
        })
    }

    /// Off Linux: never.
    #[cfg(not(target_os = "linux"))]
    pub fn open(_nic: &str) -> io::Result<Direct> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    /// Sends the IPv4 `frame` to `mac` (the hot path: one system call).
    #[cfg(target_os = "linux")]
    pub fn send(&self, frame: &[u8], mac: [u8; 6]) -> io::Result<()> {
        // SAFETY: an all-zero sockaddr_ll is valid; its fields are set below.
        let mut to: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
        to.sll_family = libc::AF_PACKET as u16;
        to.sll_protocol = (libc::ETH_P_IP as u16).to_be();
        to.sll_ifindex = self.ifindex;
        to.sll_halen = 6;
        to.sll_addr[..6].copy_from_slice(&mac);
        // SAFETY: `frame` and `to` outlive the call, which only reads them.
        let r = unsafe {
            libc::sendto(
                std::os::fd::AsRawFd::as_raw_fd(&self.fd),
                frame.as_ptr().cast(),
                frame.len(),
                0,
                (&to as *const libc::sockaddr_ll).cast(),
                std::mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t,
            )
        };
        if r < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Off Linux: never opened, so never called.
    #[cfg(not(target_os = "linux"))]
    pub fn send(&self, _frame: &[u8], _mac: [u8; 6]) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

/// Opens the direct socket on the interface holding `ip` and logs the
/// outcome; `None` when it cannot be opened (the UDP sockets carry every
/// flow). Off Linux it is silent.
pub fn open_for(ip: Ipv4Addr) -> Option<(Direct, String)> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let Some(nic) = nic_of(ip) else {
        tracing::warn!(%ip, "audio TX goes through the qdisc: no interface holds the address");
        return None;
    };
    match Direct::open(&nic) {
        Ok(d) => {
            tracing::info!(nic = %nic, "audio TX bypasses the qdisc (AF_PACKET, unicast flows to resolved neighbours)");
            Some((d, nic))
        }
        Err(e) => {
            tracing::warn!(nic = %nic, error = %e, kind = ?e.kind(),
                "audio TX goes through the qdisc: the direct socket was refused (it needs CAP_NET_RAW)");
            // Only then does the qdisc matter for audio.
            #[cfg(target_os = "linux")]
            crate::qdisc::report(&nic);
            None
        }
    }
}

/// The Windows glue: the neighbour table and our NIC.
#[cfg(windows)]
pub(crate) mod windows;

#[cfg(test)]
mod tests;
