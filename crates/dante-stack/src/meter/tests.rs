use super::*;

/// Dante Controller's meter start, captured (netaudio's fixture
/// tests/fixtures/metering/controller_start.bin, the controller already at
/// 192.0.2.10 with MAC 00:00:00:00:00:01): transaction 0x25c7, name
/// "ad4d", frames to port 8751. Byte 0x23, the name's pad byte, is 0x30
/// there (garbage); 0 here.
const DC_START: [u8; 66] = [
    0x12, 0x00, 0x00, 0x42, 0x25, 0xc7, 0x30, 0x10, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x01, 0x00, 0x00, 0x00, 0x04, 0x00, 0x10, 0x00, 0x02, 0x00, 0x12, 0x00, 0x0a, 0x61, 0x64,
    0x34, 0x64, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x16, 0x00, 0x01, 0x22, 0x2f, 0x00, 0x01,
    0x00, 0x00, 0xc0, 0x00, 0x02, 0x0a, 0x22, 0x2f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x22, 0x2f,
    0x00, 0x00,
];

const DC_MAC: [u8; 6] = [0, 0, 0, 0, 0, 1];

fn dc_to() -> SocketAddrV4 {
    SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 10), 8751)
}

#[test]
fn the_start_is_dante_controller_s_captured_request() {
    assert_eq!(
        start_request(0x25c7, "ad4d", DC_MAC, dc_to()).unwrap(),
        DC_START
    );
}

#[test]
fn the_stop_is_the_start_with_its_destination_zeroed() {
    // The capture's stop: transaction 0x25c8, the last 16 bytes zero.
    let mut want = DC_START;
    want[4..6].copy_from_slice(&[0x25, 0xc8]);
    want[50..].fill(0);
    assert_eq!(stop_request(0x25c8, "ad4d", DC_MAC, dc_to()).unwrap(), want);
}

#[test]
fn a_name_of_even_length_gets_two_zeros_and_the_offsets_follow_it() {
    let r = start_request(1, "abcde", DC_MAC, dc_to()).unwrap();
    // "abcde" NUL is 6 bytes: offsets 0x10 and 0x12, the tail 0x16.
    assert_eq!(&r[22..24], &[0x00, 0x10]);
    assert_eq!(&r[26..28], &[0x00, 0x12]);
    assert_eq!(&r[30..36], b"abcde\0");
    let r = start_request(1, "abcdef", DC_MAC, dc_to()).unwrap();
    assert_eq!(&r[30..38], b"abcdef\0\0");
    assert_eq!(&r[22..24], &[0x00, 0x12]);
    assert_eq!(&r[42..44], &[0x00, 0x18]);
    assert_eq!(r.len(), 68);
    assert_eq!(&r[2..4], &[0x00, 68]);
    assert_eq!(start_request(1, &"x".repeat(70_000), DC_MAC, dc_to()), None);
}

/// A frame in netaudio's frame layout: two TX levels, three RX levels.
fn frame() -> Vec<u8> {
    let mut f = vec![0xff, 0xff, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00];
    f.extend_from_slice(&[1, 2, 3, 4, 5, 6, 0, 0]);
    f.extend_from_slice(b"Audinate");
    f.extend_from_slice(&[FRAME_VERSION, 2, 3, 0, 1, 41, 254, 255, 0]);
    let len = f.len() as u16;
    f[2..4].copy_from_slice(&len.to_be_bytes());
    f
}

#[test]
fn a_frame_reads_the_device_and_its_levels() {
    assert_eq!(
        parse_frame(&frame()),
        Some(Frame {
            device_id: [1, 2, 3, 4, 5, 6, 0, 0],
            tx: vec![0, 1],
            rx: vec![41, 254, 255],
        })
    );
    // Not a frame: another start code, vendor or version, or counts
    // past it.
    for (at, v) in [(1, 0xfe), (16, b'X'), (24, 3), (26, 9)] {
        let mut f = frame();
        f[at] = v;
        assert_eq!(parse_frame(&f), None, "byte {at}");
    }
    assert_eq!(parse_frame(&frame()[..20]), None);
    assert!(parse_frame(&frame()[..32]).is_some());
    assert_eq!(parse_frame(&frame()[..31]), None);
}

#[test]
fn a_level_byte_reads_as_netaudio_s_scale() {
    assert_eq!(level(0), Level::Clipping);
    assert_eq!(level(1), Level::Dbfs(0.0));
    assert_eq!(level(41), Level::Dbfs(-20.0));
    assert_eq!(level(253), Level::Dbfs(-126.0));
    assert_eq!(level(254), Level::Muted);
    assert_eq!(level(255), Level::Marker);
    assert_eq!(
        [0, 3, 254, 255].map(dbfs),
        [Some(0.0), Some(-1.0), None, None]
    );
}

#[test]
fn a_meter_subscribes_keeps_the_frames_and_stops() {
    let dev = UdpSocket::bind("127.0.0.1:0").unwrap();
    let device = match dev.local_addr().unwrap() {
        SocketAddr::V4(a) => a,
        _ => unreachable!(),
    };
    let (meter, frames) = Meter::start(Ipv4Addr::LOCALHOST, device, "ctl", DC_MAC).unwrap();
    dev.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut buf = [0u8; 256];
    let (n, from) = dev.recv_from(&mut buf).unwrap();
    let start = buf[..n].to_vec();
    let SocketAddr::V4(to) = from else {
        unreachable!()
    };
    // The start names the meter's own socket as the destination.
    assert_eq!(
        start,
        start_request(u16::from_be_bytes([start[4], start[5]]), "ctl", DC_MAC, to).unwrap()
    );
    // Another sender's frame is skipped; the device's is kept and sent on.
    let other = UdpSocket::bind("127.0.0.2:0").unwrap();
    let mut theirs = frame();
    theirs[29] = 9;
    other.send_to(&theirs, to).unwrap();
    dev.send_to(&frame(), to).unwrap();
    let got = frames.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(got.rx, vec![41, 254, 255]);
    assert_eq!(meter.latest(), Some(got));
    assert!(frames.try_recv().is_err(), "only the device's frame");
    meter.stop().unwrap();
    let (n, _) = dev.recv_from(&mut buf).unwrap();
    assert_eq!(&buf[n - 16..n], &[0; 16], "the stop clears the destination");
    assert_eq!(&buf[6..8], &[0x30, 0x10]);
}

#[test]
fn a_meter_renews_its_subscription_until_it_stops() {
    // netaudio's daemon renews every 3 s (its KEEPALIVE) and a device
    // drops a subscription that is not renewed: v0.4.31 sent the start
    // once.
    let dev = UdpSocket::bind("127.0.0.1:0").unwrap();
    let device = match dev.local_addr().unwrap() {
        SocketAddr::V4(a) => a,
        _ => unreachable!(),
    };
    let (meter, _frames) = Meter::start(Ipv4Addr::LOCALHOST, device, "ctl", DC_MAC).unwrap();
    dev.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut buf = [0u8; 256];
    let (n, _) = dev.recv_from(&mut buf).unwrap();
    let first = buf[..n].to_vec();
    let started = std::time::Instant::now();
    let (n, _) = dev.recv_from(&mut buf).expect("a renewal within 5 s");
    let again = buf[..n].to_vec();
    let after = started.elapsed();
    assert!(
        after >= Duration::from_millis(2500),
        "renewed after {after:?}"
    );
    assert_ne!(again[4..6], first[4..6], "a new transaction");
    assert_eq!(again[6..], first[6..], "the same start otherwise");
    meter.stop().unwrap();
}
