// SPDX-License-Identifier: MIT OR Apache-2.0
//! Our 0x2200 and 0x3200 in a real ARC 2.7 card's layout (dstack #274):
//! each socket descriptor tagged `08 02` (8 bytes, family 2 = IPv4, as
//! the card and netaudio's parsers read it), and the 0x2200 names
//! descriptor's word at +8 the flow's frames per packet (the card states
//! 4, 16 or 32 per flow). Read with this test's own offsets, compared with
//! the card's synthetic page ([`CARD_27_PAGE`]).

use super::*;

use crate::arc::flows::tests::{CARD_27_PAGE, hex};

/// The card page's record 1: its socket descriptor (packet offset 0x48) and
/// its names descriptor (0x50), each as the card wrote them.
fn card_record_1() -> ([u8; 2], [u8; 4]) {
    let card = hex(CARD_27_PAGE);
    (
        [card[0x48], card[0x49]],
        [card[0x50], card[0x51], card[0x52], card[0x53]],
    )
}

/// The u16 at body offset `at`.
fn word(body: &[u8], at: usize) -> usize {
    usize::from(u16::from_be_bytes([body[at], body[at + 1]]))
}

#[test]
fn our_tx_flow_list_tags_sockets_and_states_fpp_as_the_card() {
    let (card_tag, card_names) = card_record_1();
    assert_eq!(card_tag, [0x08, 0x02], "the card's socket tag");
    let mut dev = Dev::new(config());
    // Two flows at the card's other frame counts (4 and 32; 16 is the old
    // fixed word and proves nothing).
    let asked = [(4u16, 40_004u16), (32, 40_032)];
    for (fpp, port) in asked {
        let mut req = tx_request(
            "p",
            "f",
            &[Some(1), Some(2)],
            SocketAddrV4::new(PEER_IP, port),
        );
        req.fpp = fpp;
        dev.tx.add(&req, 0).unwrap();
    }
    let answer = dev.ask(1, op::TX_FLOWS, &DC_FIRST_PAGE);
    let (_, items, status) = flow_page(&answer);
    assert_eq!((items.len(), status), (2, STATUS_OK));
    let body = &answer.body;
    for (item, (fpp, port)) in items.iter().zip(asked) {
        let main = usize::from(*item) - HEADER_LEN;
        let sockets = word(body, main + 12);
        let slots = word(body, main + 14);
        assert_eq!((sockets, slots), (1, 2));
        let socket = word(body, main + 16) - HEADER_LEN;
        let mut want = card_tag.to_vec();
        want.extend(port.to_be_bytes());
        want.extend(PEER_IP.octets());
        assert_eq!(
            body[socket..socket + 8],
            want,
            "socket of the fpp {fpp} flow"
        );
        let names = word(body, main + 16 + 2 * (sockets + slots)) - HEADER_LEN;
        assert_eq!(body[names..names + 4], card_names, "names descriptor head");
        assert_eq!(word(body, names + 8), usize::from(fpp), "frames per packet");
        assert_eq!(body[names + 12..names + 20], [0; 8]);
    }
}

#[test]
fn our_rx_flow_list_tags_its_socket_as_the_card() {
    let (card_tag, _) = card_record_1();
    let mut dev = Dev::new(config());
    let net = Net(vec![("peer-device".into(), "TX 3".into(), adv(PEER_IP, 3))]);
    let subs = [(0, sub("TX 3", "peer-device"))];
    receiving(&mut dev.rx, &subs, &net);
    let answer = dev.ask(1, op::RX_FLOWS, &DC_FIRST_PAGE);
    let (_, items, status) = flow_page(&answer);
    assert_eq!((items.len(), status), (1, STATUS_OK));
    let body = &answer.body;
    let main = usize::from(items[0]) - HEADER_LEN;
    assert_eq!(word(body, main + 12), 1, "one socket");
    let socket = word(body, main + 18) - HEADER_LEN;
    assert_eq!(body[socket..socket + 2], card_tag);
    assert_eq!(body[socket + 4..socket + 8], OUR_IP.octets());
}
