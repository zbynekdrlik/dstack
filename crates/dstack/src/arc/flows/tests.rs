use super::*;

use crate::wire::STATUS_ERROR;

/// Hex to bytes.
pub(crate) fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// A UltimoX2's (platform software 4.2.5.1) 0x2600 reply, 8.11.2026
/// (dstack #148): flow 1 multicast to its group, channels 1 and 2; flow 2
/// unicast to a receiver, channels 2 and 1. The group, the receiver's
/// address and its name are replaced (TEST-NET, a synthetic name of the
/// same length).
pub(crate) const ULTIMO_LIST: &str = "280900e851022600000100000000000002020020007c31000001770000000018142700010000000300010000000000020000000000160018000f424000000000000000000000000008130000000000000001000000000000040b01010070000005070002000100020000020000200000080210e1efff000132000000142700020000000300020000000000110000000000780018000000000000000000000000000000000813000000d400e40001000000000000040b010100cc0000050700020002000100000200002000000802e014c000021e746573742d72656365697665722d3100325f3100";

/// Our create sent to that UltimoX2 (media-local id 1, channels 1 and 2)
/// and its reply: the flow it made.
const ULTIMO_CREATE: &str = "2809006a51012601000000000001000001010014162b0000000000030001000000000002000000000000000000000000000000000000000000000000000000000a15000000000000000100000000000000000000040b0000000000000507000200010002000002000000";
const ULTIMO_CREATED: &str = "2809006e51012601000100000000000001010020310000000000000000000018142700010000000300010000000000020000000000140018000f424000000000000000000000000008130000000000000001000000000000040b0000000000000507000200010002000002000020";

/// netaudio's request to the same UltimoX2 at 18:36Z (options 0, the name
/// "mics"): the reply, OK and no record. It made no flow.
const ULTIMO_NOTHING: &str = "28090014000a2601000100000000000001000000";

/// Dante Controller's exchanges netaudio recorded
/// (`tests/fixtures/multicast_creation_2809.json`): an AES3 adapter
/// (options 0x0071) with channels 1+2 and with channel 2, a Bluetooth
/// adapter (0x0001) with channels 1+2, each in media-local slot 2.
const DC: [(&str, &str); 3] = [
    (
        "2809006a15e02601000000000071000001010014162b0000000000030002000000000002000000000000000000000000000000000000000000000000000000000a15000000000000000100000000000000000000040b0000000000000507000200010002000002000000",
        "2809006e15e0260100010000000000000101002032006e040000000000000018142700020000000300020000000000020000000000140018000f424000000000000000000000000008130000000000000001000000000000040b0000000000000507000200010002000002000010",
    ),
    (
        "2809006816002601000000000071000001010014162a0000000000030002000000000002000000000000000000000000000000000000000000000000000000000a14000000000000000100000000000000000000040a000000000000040600010002000002000000",
        "2809006c16002601000100000000000001010020320000000000000000000018142600020000000300020000000000020000000000140018000f424000000000000000000000000008120000000000000001000000000000040a000000000000040600010002000002000010",
    ),
    (
        "2809006a171d2601000000000001000001010014162b0000000000030002000000000002000000000000000000000000000000000000000000000000000000000a15000000000000000100000000000000000000040b0000000000000507000200010002000002000000",
        "2809006e171d2601000100000000000001010020320000000000000000000018142700020000000300020000000000020000000000140018000f424000000000000000000000000008130000000000000001000000000000040b0000000000000507000200010002000002000010",
    ),
];

/// A device that answers every call with one recorded datagram and keeps
/// what it was asked.
struct Recorded {
    reply: Vec<u8>,
    asked: Vec<(u16, u16, Vec<u8>)>,
}

impl Recorded {
    fn new(datagram: &str) -> Self {
        Recorded {
            reply: hex(datagram),
            asked: Vec::new(),
        }
    }
}

impl Call for Recorded {
    fn call(&mut self, op1: u16, body: &[u8]) -> Result<(u16, Vec<u8>), ReadError> {
        self.call_as(0x27ff, op1, body)
    }

    fn call_as(&mut self, start: u16, op1: u16, body: &[u8]) -> Result<(u16, Vec<u8>), ReadError> {
        self.asked.push((start, op1, body.to_vec()));
        let status = u16::from_be_bytes([self.reply[8], self.reply[9]]);
        Ok((status, self.reply[HEADER_LEN..].to_vec()))
    }
}

#[test]
fn the_ultimo_list_reads_its_multicast_and_its_unicast_flow() {
    let mut dev = Recorded::new(ULTIMO_LIST);
    let flows = list_tx_flows(&mut dev).unwrap();
    assert_eq!(flows.unread, 0);
    assert_eq!(
        flows.flows,
        vec![
            TxFlow {
                id: 1,
                local_id: 1,
                flags: MULTICAST,
                channels: vec![1, 2],
                fpp: 32,
                bits: 24,
                destinations: vec![SocketAddrV4::new(Ipv4Addr::new(239, 255, 0, 1), 4321)],
            },
            TxFlow {
                id: 2,
                local_id: 2,
                flags: 0x11,
                channels: vec![2, 1],
                fpp: 32,
                bits: 24,
                destinations: vec![SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 30), 57364)],
            },
        ]
    );
    assert!(flows.flows[0].multicast());
    assert!(!flows.flows[1].multicast());
    // The request: start code 0x2809, 0x2600, netaudio's 24 bytes.
    assert_eq!(dev.asked, vec![(START_2809, LIST, list_body().to_vec())]);
    assert_eq!(
        hex("000000000000000100010001000000000000000000000000"),
        list_body()
    );
}

#[test]
fn our_create_is_dante_controller_s_and_reads_the_ultimo_s_new_flow() {
    let mut dev = Recorded::new(ULTIMO_CREATED);
    let made = create_multicast(&mut dev, 1, &[1, 2], 0x0001).unwrap();
    assert_eq!(
        made,
        TxFlow {
            id: 1,
            local_id: 1,
            flags: MULTICAST,
            channels: vec![1, 2],
            fpp: 32,
            bits: 24,
            destinations: Vec::new(),
        }
    );
    let sent = hex(ULTIMO_CREATE);
    assert_eq!(
        dev.asked,
        vec![(START_2809, CREATE, sent[HEADER_LEN..].to_vec())]
    );
}

#[test]
fn the_create_body_is_byte_for_byte_dante_controller_s() {
    let asks: [(&[u16], u16); 3] = [(&[1, 2], 0x0071), (&[2], 0x0071), (&[1, 2], 0x0001)];
    for ((request, reply), (channels, options)) in DC.iter().zip(asks) {
        let sent = hex(request);
        assert_eq!(
            create_body(2, channels, options).unwrap(),
            sent[HEADER_LEN..]
        );
        let made = create_multicast(&mut Recorded::new(reply), 2, channels, options).unwrap();
        assert_eq!((made.id, made.local_id), (2, 2));
        assert_eq!(made.channels, channels);
        assert!(made.multicast());
        assert_eq!((made.fpp, made.bits), (16, 24));
    }
}

#[test]
fn an_ok_answer_without_a_record_made_no_flow() {
    assert_eq!(
        create_multicast(&mut Recorded::new(ULTIMO_NOTHING), 1, &[1, 2], 0x0001),
        Err(CreateError::NotCreated)
    );
}

#[test]
fn a_refusal_or_an_unreadable_reply_is_an_error() {
    let mut refused = hex(ULTIMO_CREATED);
    refused[8..10].copy_from_slice(&STATUS_ERROR.to_be_bytes());
    let mut dev = Recorded {
        reply: refused,
        asked: Vec::new(),
    };
    assert_eq!(
        create_multicast(&mut dev, 1, &[1, 2], 0x0001),
        Err(CreateError::Read(ReadError::Refused {
            op: CREATE,
            status: STATUS_ERROR
        }))
    );
    assert_eq!(
        list_tx_flows(&mut dev),
        Err(ReadError::Refused {
            op: LIST,
            status: STATUS_ERROR
        })
    );
    // A record table that passes the reply.
    let short = &hex(ULTIMO_LIST)[..HEADER_LEN + 9];
    let mut dev = Recorded {
        reply: short.to_vec(),
        asked: Vec::new(),
    };
    assert_eq!(
        list_tx_flows(&mut dev),
        Err(ReadError::Malformed { op: LIST })
    );
    // A created record that does not read.
    let mut cut = hex(ULTIMO_CREATED);
    cut.truncate(cut.len() - 4);
    let mut dev = Recorded {
        reply: cut,
        asked: Vec::new(),
    };
    assert_eq!(
        create_multicast(&mut dev, 1, &[1, 2], 0x0001),
        Err(CreateError::Read(ReadError::Malformed { op: CREATE }))
    );
}

#[test]
fn a_record_that_does_not_read_is_counted_and_the_rest_listed() {
    let mut d = hex(ULTIMO_LIST);
    // The first record's destination offset points past the reply.
    d[0x5c..0x5e].copy_from_slice(&0x0400u16.to_be_bytes());
    let flows = parse_flows(&d[HEADER_LEN..]).unwrap();
    assert_eq!(flows.unread, 1);
    assert_eq!(flows.flows.len(), 1);
    assert_eq!(flows.flows[0].id, 2);
    // A segment of no words.
    let mut d = hex(ULTIMO_LIST);
    d[0x48] = 0;
    let flows = parse_flows(&d[HEADER_LEN..]).unwrap();
    assert_eq!((flows.unread, flows.flows.len()), (1, 1));
    // A descriptor that is not `08 02`.
    let mut d = hex(ULTIMO_LIST);
    d[0x70] = 9;
    assert_eq!(parse_flows(&d[HEADER_LEN..]).unwrap().unread, 1);
    // A segment longer than its record.
    let mut d = hex(ULTIMO_LIST);
    d[0x6a] = 3;
    let flows = parse_flows(&d[HEADER_LEN..]).unwrap();
    assert_eq!((flows.unread, flows.flows[0].id), (1, 2));
    // The last segment two words longer than its record (the record made
    // one word longer, so the overrun is not caught by the chain alone).
    let mut d = hex(ULTIMO_LIST);
    d[0x21] = 0x28;
    d[0x6a] = 4;
    let flows = parse_flows(&d[HEADER_LEN..]).unwrap();
    assert_eq!((flows.unread, flows.flows[0].id), (1, 2));
    // A record offset past the reply: the table does not read.
    let mut d = hex(ULTIMO_LIST);
    d.truncate(0x13);
    assert_eq!(parse_flows(&d[HEADER_LEN..]), None);
    // More records than slots: the table does not read.
    let mut d = hex(ULTIMO_LIST);
    d[0x10] = 1;
    assert_eq!(parse_flows(&d[HEADER_LEN..]), None);
}

#[test]
fn only_a_request_dante_controller_sends_is_built() {
    let invalid = |r: Result<Vec<u8>, CreateError>| matches!(r, Err(CreateError::Invalid(_)));
    assert!(invalid(create_body(1, &[1, 2], 0)));
    assert!(invalid(create_body(0, &[1, 2], 1)));
    assert!(invalid(create_body(1, &[], 1)));
    assert!(invalid(create_body(1, &[0], 1)));
    assert!(invalid(create_body(1, &[1, 1], 1)));
    assert!(invalid(create_body(1, &[1, 2, 3, 4, 5, 6, 7, 8, 9], 1)));
    assert!(create_body(1, &[1, 2, 3, 4, 5, 6, 7, 8], 1).is_ok());
    // Nothing is sent for a request that is not built.
    let mut dev = Recorded::new(ULTIMO_CREATED);
    assert!(create_multicast(&mut dev, 1, &[1, 1], 1).is_err());
    assert!(dev.asked.is_empty());
}

#[test]
fn a_built_body_s_record_reads_back_as_its_flow() {
    // The record we send has the list's chain: read in a reply, it names
    // the slot and the channels it asked for.
    for channels in [&[3u16][..], &[1, 2, 3, 4, 5, 6, 7, 8]] {
        let body = create_body(5, channels, 1).unwrap();
        let flows = parse_flows(&body).unwrap();
        assert_eq!(flows.unread, 0);
        let f = &flows.flows[0];
        assert_eq!((f.local_id, f.id, f.fpp), (5, 0, 0));
        assert_eq!(f.channels, channels);
        assert!(f.multicast());
        assert!(f.destinations.is_empty());
    }
}

#[test]
fn the_list_goes_over_udp_with_the_2809_start_code() {
    use std::net::UdpSocket;
    use std::sync::atomic::{AtomicBool, AtomicU16};
    use std::time::Duration;

    use crate::arc::client::Udp;
    use crate::ctl::Requests;

    let dev = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    dev.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let Ok(std::net::SocketAddr::V4(at)) = dev.local_addr() else {
        panic!("an IPv4 socket");
    };
    let device = std::thread::spawn(move || {
        let mut buf = [0u8; 2048];
        let (n, from) = dev.recv_from(&mut buf).unwrap();
        let asked = buf[..n].to_vec();
        // The recorded reply, under the request's sequence number.
        let mut reply = hex(ULTIMO_LIST);
        reply[4..6].copy_from_slice(&asked[4..6]);
        dev.send_to(&reply, from).unwrap();
        asked
    });
    let seq = AtomicU16::new(7);
    let requests = Requests::default();
    let stop = AtomicBool::new(false);
    let mut ex = Udp::open(
        Ipv4Addr::LOCALHOST,
        at,
        &seq,
        Duration::from_secs(5),
        &requests,
        &stop,
    )
    .unwrap();
    let flows = list_tx_flows(&mut ex).unwrap();
    assert_eq!(flows.flows.len(), 2);
    let asked = device.join().unwrap();
    assert_eq!(
        asked,
        hex("28090022000726000000000000000000000100010001000000000000000000000000")
    );
}

#[test]
fn a_flow_lists_every_destination_in_order() {
    // The multicast record's destination table, given a second entry: the
    // unicast record's descriptor.
    let mut d = hex(ULTIMO_LIST);
    d[0x5a..0x5c].copy_from_slice(&[2, 2]);
    d[0x5e..0x60].copy_from_slice(&0x00ccu16.to_be_bytes());
    let flows = parse_flows(&d[HEADER_LEN..]).unwrap();
    assert_eq!(
        flows.flows[0].destinations,
        vec![
            SocketAddrV4::new(Ipv4Addr::new(239, 255, 0, 1), 4321),
            SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 30), 57364),
        ]
    );
}
