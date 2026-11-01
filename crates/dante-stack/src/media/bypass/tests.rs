use super::*;

fn at(a: [u8; 4], port: u16) -> SocketAddrV4 {
    SocketAddrV4::new(Ipv4Addr::from(a), port)
}

#[test]
fn the_headers_match_a_packet_built_by_hand() {
    // Built independently (Python struct + RFC 1071), payload of odd length.
    let want: [u8; HEADROOM] = [
        0x45, 0xb8, 0x00, 0x1f, 0x12, 0x34, 0x40, 0x00, 0x40, 0x11, 0x13, 0xe0, 0x0a, 0x00, 0x00,
        0x01, 0x0a, 0x00, 0x00, 0x02, 0x0f, 0xa0, 0x13, 0x88, 0x00, 0x0b, 0xc4, 0xab,
    ];
    let mut buf = [0xeeu8; HEADROOM + 3];
    buf[HEADROOM..].copy_from_slice(&[1, 2, 3]);
    let n = write_headers(
        &mut buf,
        at([10, 0, 0, 1], 4000),
        at([10, 0, 0, 2], 5000),
        0xb8,
        0x1234,
        3,
    );
    assert_eq!(n, HEADROOM + 3);
    assert_eq!(buf[..HEADROOM], want);
    assert_eq!(buf[HEADROOM..], [1, 2, 3], "the payload untouched");
}

/// The receiver's check: the one's complement sum over the header (IP) or
/// the pseudo-header and segment (UDP) is all ones.
fn verifies(sum: u32) -> bool {
    checksum(sum) == 0
}

#[test]
fn both_checksums_verify_for_any_payload_length() {
    for len in [0usize, 1, 2, 17, 1024, 1443] {
        let mut buf = vec![0u8; HEADROOM + len];
        for (i, b) in buf[HEADROOM..].iter_mut().enumerate() {
            *b = (i * 37 + 11) as u8;
        }
        let (src, dst) = (at([192, 168, 7, 112], 40001), at([192, 168, 7, 110], 4321));
        let n = write_headers(&mut buf, src, dst, 0xb8, len as u16, len);
        assert!(verifies(sum16(&buf[..IP_HEADER], 0)), "IP, len {len}");
        let pseudo = sum16(&src.ip().octets(), 0)
            + sum16(&dst.ip().octets(), 0)
            + u32::from(UDP)
            + (UDP_HEADER + len) as u32;
        assert!(
            verifies(sum16(&buf[IP_HEADER..n], pseudo)),
            "UDP, len {len}"
        );
        assert_eq!(usize::from(u16::from_be_bytes([buf[2], buf[3]])), n);
    }
}

#[test]
fn a_udp_checksum_of_zero_goes_out_as_all_ones() {
    // Pick the payload's last two bytes so the computed checksum is 0.
    let (src, dst) = (at([10, 0, 0, 1], 1), at([10, 0, 0, 2], 2));
    let mut buf = [0u8; HEADROOM + 2];
    write_headers(&mut buf, src, dst, 0, 0, 2);
    let c = u16::from_be_bytes([buf[26], buf[27]]);
    // Adding the checksum to the payload makes the sum all ones, so the
    // next computation would give 0.
    buf[HEADROOM..].copy_from_slice(&c.to_be_bytes());
    write_headers(&mut buf, src, dst, 0, 0, 2);
    assert_eq!(buf[26..28], [0xff, 0xff]);
}

#[test]
fn sum16_pads_an_odd_last_byte() {
    assert_eq!(sum16(&[0x12], 0), 0x1200);
    assert_eq!(sum16(&[0x12, 0x34, 0x56], 1), 0x1234 + 0x5600 + 1);
    assert_eq!(checksum(0x1_fffe), 0, "carries fold in");
    assert_eq!(checksum(0x2_fffd), 0, "twice");
}

const ARP: &str = "\
IP address       HW type     Flags       HW address            Mask     Device
10.77.7.110      0x1         0x2         00:1d:c1:aa:bb:cc     *        enp2s0
10.77.7.104      0x1         0x0         00:00:00:00:00:00     *        enp2s0
10.77.7.105      0x1         0x2         00:1d:c1:00:00:05     *        wlan0
10.77.7.106      0x1         0x6         00:1d:c1:00:00:06     *        enp2s0
10.77.7.107      0x1         0x2         00:00:00:00:00:00     *        enp2s0
10.77.7.108      0x1         0x2         00:1d:c1:00:00        *        enp2s0
";

#[test]
fn only_a_complete_entry_on_the_nic_gives_a_mac() {
    let ip = |d| Ipv4Addr::new(10, 77, 7, d);
    assert_eq!(
        arp_mac(ARP, ip(110), "enp2s0"),
        Some([0, 0x1d, 0xc1, 0xaa, 0xbb, 0xcc])
    );
    assert_eq!(arp_mac(ARP, ip(104), "enp2s0"), None, "incomplete");
    assert_eq!(arp_mac(ARP, ip(105), "enp2s0"), None, "another interface");
    assert_eq!(
        arp_mac(ARP, ip(106), "enp2s0"),
        Some([0, 0x1d, 0xc1, 0, 0, 6]),
        "complete + permanent"
    );
    assert_eq!(arp_mac(ARP, ip(107), "enp2s0"), None, "an all-zero MAC");
    assert_eq!(arp_mac(ARP, ip(108), "enp2s0"), None, "a short MAC");
    assert_eq!(arp_mac(ARP, ip(99), "enp2s0"), None, "not there");
    assert_eq!(arp_mac("", ip(110), "enp2s0"), None);
    // The header line is no entry, even if it parsed.
    let header_only = "10.77.7.110 0x1 0x2 00:1d:c1:aa:bb:cc * enp2s0\n";
    assert_eq!(arp_mac(header_only, ip(110), "enp2s0"), None);
}

#[test]
fn a_mac_survives_its_word_and_none_is_zero() {
    for mac in [[0u8; 6], [0xff; 6], [0, 0x1d, 0xc1, 0xaa, 0xbb, 0xcc]] {
        assert_eq!(word_mac(mac_word(Some(mac))), Some(mac));
    }
    assert_eq!(mac_word(None), 0);
    assert_eq!(word_mac(0), None);
    assert_eq!(
        word_mac(0xffff_ffff_ffff),
        None,
        "a MAC without the present bit"
    );
}

#[test]
fn only_resolved_unicast_destinations_go_direct() {
    let dests = [
        Some(at([10, 77, 7, 110], 4321)),
        Some(at([10, 77, 7, 104], 4321)),
        Some(at([239, 69, 0, 1], 4321)),
        None,
    ];
    let words: Vec<u64> = mac_words(ARP, "enp2s0", dests.into_iter()).collect();
    assert_eq!(
        words,
        [mac_word(Some([0, 0x1d, 0xc1, 0xaa, 0xbb, 0xcc])), 0, 0, 0]
    );
    // A multicast group never goes direct, even with an entry for it.
    let table = "h\n239.69.0.1 0x1 0x2 01:00:5e:45:00:01 * enp2s0\n";
    let words: Vec<u64> =
        mac_words(table, "enp2s0", [Some(at([239, 69, 0, 1], 1))].into_iter()).collect();
    assert_eq!(words, [0]);
}

/// Linux only: the address lookup serves the direct socket, which only
/// Linux opens.
#[cfg(target_os = "linux")]
#[test]
fn the_loopback_address_has_an_interface() {
    assert!(nic_of(Ipv4Addr::LOCALHOST).is_some());
    assert_eq!(nic_of(Ipv4Addr::new(192, 0, 2, 77)), None);
}
