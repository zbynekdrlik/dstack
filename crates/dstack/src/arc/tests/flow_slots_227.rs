// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! Our TX flow capacity as we state it (dstack #227): 0x1000 states the
//! config's `flow_slots` each way, under 2.7's start codes and 2.8.9's
//! (Dante Controller picks the highest flow it states for a multicast
//! create, which a device with fewer slots refused), and our 0x2600 list
//! (#148's records) is paged by the request's first flow, each page one
//! datagram. A reader up to v0.6.97 asks once, from flow 1, and takes only
//! a page with status OK: page one holds every flow that fits a datagram,
//! so such a reader keeps reading a transmitter whose flows fit.
//!
//! The pages' sizes are this file's own arithmetic on #148's record
//! (`record_bytes`), never the writer's.

use super::*;

use crate::arc::client::ReadError;
use crate::arc::control::Call;
use crate::arc::flows::{LIST, START_2809, list_body_from, list_tx_flows, parse_flows};
use crate::wire::STATUS_MORE;

/// The counts' TX and RX flows: the 2.7 counts' bytes 12-15, and the
/// 2.8.9 media record's fourth pair (after the 34 bytes of 2.7 counts,
/// the record's offset, 0, `14 14` and the media type: bytes 54-57).
fn stated_flows(dev: &mut Dev, start: u16) -> ([u8; 4], Option<[u8; 4]>) {
    let body = decode(dev.send(&request(start, 1, op::COUNTS, 0, &[]))).body;
    let at = |a: usize| -> [u8; 4] { body[a..a + 4].try_into().unwrap() };
    let v289 = (start == START_2809).then(|| at(54));
    (at(12), v289)
}

#[test]
fn the_counts_state_the_config_s_flow_slots_under_every_start_code() {
    for slots in [1u8, 4, 32] {
        let mut dev = Dev::new(DeviceConfig {
            flow_slots: usize::from(slots),
            ..config()
        });
        let want = [0, slots, 0, slots];
        for start in [NETAUDIO, CONTROLLER, START_2809] {
            let (counts, v289) = stated_flows(&mut dev, start);
            assert_eq!(counts, want, "{slots} slots, start {start:#06x}");
            if start == START_2809 {
                assert_eq!(v289, Some(want), "the 2.8.9 media record, {slots} slots");
            }
        }
    }
    // A config past the TX table's 32 (refused by its own checks) states
    // the 32 the table holds.
    let mut dev = Dev::new(DeviceConfig {
        flow_slots: 40,
        ..config()
    });
    assert_eq!(stated_flows(&mut dev, CONTROLLER).0, [0, 32, 0, 32]);
}

/// The bytes one of our unicast flows of `channels` channels takes on a
/// 0x2600 page: its table offset (2), its record (segments of 12, 2,
/// 3, 2 + `channels` and 2 words) and its destination's 8-byte
/// descriptor.
fn record_bytes(channels: usize) -> usize {
    2 + 2 * (12 + 2 + 3 + 2 + channels + 2) + 8
}

/// A page's bytes before its records: the header and `0000 0000 0000`,
/// `[slots, count]`.
const PAGE_HEAD: usize = HEADER_LEN + 8;

/// Our device with unicast TX flows in slots 1, 2, ..., one per entry of
/// `channels`, each of that many of our TX channels.
fn with_flows(cfg: DeviceConfig, channels: &[usize]) -> Dev {
    let mut dev = Dev::new(cfg);
    for (k, &n) in channels.iter().enumerate() {
        let rx_at = SocketAddrV4::new(PEER_IP, 40_000 + k as u16);
        let chans: Vec<Option<u16>> = (1..=n as u16).map(Some).collect();
        let (slot, _) = dev
            .tx
            .add(&tx_request("rx", "flow", &chans, rx_at), 0)
            .unwrap();
        assert_eq!(slot, k, "the flows fill the slots in order");
    }
    dev
}

/// Our answer to the 0x2600 request `body` (start 0x2809): the status and
/// the reply body, the datagram checked to fit one.
fn list_page(dev: &mut Dev, body: &[u8]) -> (u16, Vec<u8>) {
    let packet = match dev.send(&request(START_2809, 9, LIST, 0, body)) {
        ArcOutcome::Reply(packet) => packet,
        other => panic!("the list is answered: {other:?}"),
    };
    assert!(
        packet.len() <= MAX_DATAGRAM,
        "{} bytes: past one datagram",
        packet.len()
    );
    let (h, body) = Header::parse(&packet).unwrap();
    (h.op2, body.to_vec())
}

/// The media-local ids of a page's flows, every record read.
fn ids(body: &[u8]) -> Vec<u16> {
    let flows = parse_flows(body).expect("our page reads");
    assert_eq!(flows.unread, 0);
    flows.flows.iter().map(|f| f.local_id).collect()
}

/// Our device as a reader's ARC peer.
struct Peer<'a>(&'a mut Dev, Vec<u16>);

impl Call for Peer<'_> {
    fn call(&mut self, op1: u16, body: &[u8]) -> Result<(u16, Vec<u8>), ReadError> {
        self.call_as(NETAUDIO, op1, body)
    }

    fn call_as(&mut self, start: u16, op1: u16, body: &[u8]) -> Result<(u16, Vec<u8>), ReadError> {
        self.1.push(u16_at(body, 10));
        let answer = decode(self.0.send(&request(start, 3, op1, 0, body)));
        Ok((answer.header.op2, answer.body))
    }
}

#[test]
fn the_tx_flow_list_pages_by_its_first_flow_each_page_one_datagram() {
    // 32 flows of 8 channels: 21 fit a datagram.
    let fit = (MAX_DATAGRAM - PAGE_HEAD) / record_bytes(8);
    assert_eq!(fit, 21);
    let mut dev = with_flows(config(), &[8; 32]);
    let (status, page1) = list_page(&mut dev, &list_body_from(1));
    assert_eq!(status, STATUS_MORE, "more follow");
    assert_eq!(ids(&page1), (1..=21).collect::<Vec<u16>>());
    assert_eq!(page1[6], 32, "the table states our 32 slots");
    let (status, page2) = list_page(&mut dev, &list_body_from(22));
    assert_eq!(status, STATUS_OK);
    assert_eq!(ids(&page2), (22..=32).collect::<Vec<u16>>());
    // From the middle of a page, and past every flow.
    let (status, from) = list_page(&mut dev, &list_body_from(30));
    assert_eq!((status, ids(&from)), (STATUS_OK, vec![30, 31, 32]));
    let (status, past) = list_page(&mut dev, &list_body_from(33));
    assert_eq!((status, ids(&past)), (STATUS_OK, vec![]));
    // A request without its first flow, or flow 0, reads from flow 1, as
    // every 0x2600 request was answered before pages.
    for body in [&[][..], &[0; 10][..], &list_body_from(0)[..]] {
        assert_eq!(list_page(&mut dev, body), (STATUS_MORE, page1.clone()));
    }
    // Our own reader follows the pages and takes each flow once.
    let mut peer = Peer(&mut dev, Vec::new());
    let read = list_tx_flows(&mut peer).unwrap();
    assert_eq!(peer.1, [1, 22]);
    let read_ids: Vec<u16> = read.flows.iter().map(|f| f.local_id).collect();
    assert_eq!(read_ids, (1..=32).collect::<Vec<u16>>());
    assert_eq!((read.slots, read.unread), (32, 0));
}

#[test]
fn page_one_holds_every_flow_that_fits_a_datagram_to_its_last_byte() {
    // 23 flows of 3 channels and 2 of 4 fill a datagram exactly.
    let mut channels = vec![3; 23];
    channels.extend([4, 4]);
    let bytes: usize = channels.iter().map(|&c| record_bytes(c)).sum();
    assert_eq!(PAGE_HEAD + bytes, MAX_DATAGRAM);
    let mut dev = with_flows(config(), &channels);
    // A reader up to v0.6.97 asks once from flow 1 and needs status OK:
    // every flow on that page.
    let (status, page) = list_page(&mut dev, &list_body_from(1));
    assert_eq!(status, STATUS_OK);
    assert_eq!(HEADER_LEN + page.len(), MAX_DATAGRAM);
    assert_eq!(ids(&page), (1..=25).collect::<Vec<u16>>());
    // One flow more (of one channel) passes the datagram: page one keeps
    // the 25, the 26th is on page two.
    channels.push(1);
    let mut dev = with_flows(config(), &channels);
    let (status, page) = list_page(&mut dev, &list_body_from(1));
    assert_eq!((status, ids(&page).len()), (STATUS_MORE, 25));
    let (status, page) = list_page(&mut dev, &list_body_from(26));
    assert_eq!((status, ids(&page)), (STATUS_OK, vec![26]));
}

/// A reader up to v0.6.97 (`list_tx_flows` at that tag, as written
/// there): one request with its `list_body` (24 bytes, `00 01 00 01 00
/// 01` at bytes 6-11), and any status but OK refused. The media-local ids
/// it reads, or the status it refused.
fn v0_6_97_reader(dev: &mut Dev) -> Result<Vec<u16>, u16> {
    let mut body = [0u8; 24];
    body[6..12].copy_from_slice(&[0, 1, 0, 1, 0, 1]);
    let answer = decode(dev.send(&request(START_2809, 4, LIST, 0, &body)));
    if answer.header.op2 != STATUS_OK {
        return Err(answer.header.op2);
    }
    Ok(ids(&answer.body))
}

#[test]
fn a_v0_6_97_reader_reads_every_flow_that_fits_page_one_and_is_refused_past_it() {
    let mut fits = with_flows(config(), &[8; 21]);
    assert_eq!(v0_6_97_reader(&mut fits), Ok((1..=21).collect()));
    let mut past = with_flows(config(), &[8; 22]);
    assert_eq!(v0_6_97_reader(&mut past), Err(STATUS_MORE));
}

#[test]
fn the_tx_flow_list_states_the_config_s_flow_slots() {
    let mut dev = with_flows(
        DeviceConfig {
            flow_slots: 4,
            ..config()
        },
        &[2, 2],
    );
    let (status, page) = list_page(&mut dev, &list_body_from(1));
    assert_eq!(status, STATUS_OK);
    let flows = parse_flows(&page).unwrap();
    assert_eq!((flows.slots, flows.flows.len()), (4, 2));
}
