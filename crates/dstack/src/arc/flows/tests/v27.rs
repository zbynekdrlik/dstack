//! ARC 2.7's TX flow list (0x2200) and the fallback to it (dstack #271),
//! from the card's synthetic page ([`CARD_27_PAGE`]) and variants of it
//! edited byte by byte. The expected flows are read off the page's
//! documented layout, not from the parser.

use super::*;

use crate::arc::flows::v27::{LIST_27, list_tx_flows_27};
use crate::wire::STATUS_MORE;

/// The page's records' packet offsets.
const REC: [usize; 3] = [0x2c, 0x78, 0xb0];
/// Record 1's socket descriptor and record 1's names descriptor.
const SOCKET_1: usize = 0x48;
const NAMES_1: usize = 0x50;

fn v4(a: [u8; 4], port: u16) -> SocketAddrV4 {
    SocketAddrV4::new(Ipv4Addr::from(a), port)
}

/// The page's three flows (see [`CARD_27_PAGE`]).
fn card_flows() -> Vec<TxFlow> {
    vec![
        TxFlow {
            id: 1,
            local_id: 1,
            flags: 0x11,
            channels: vec![25, 26, 0, 0],
            fpp: 16,
            bits: 24,
            destinations: vec![v4([192, 0, 2, 10], 30125)],
        },
        TxFlow {
            id: 2,
            local_id: 2,
            flags: 0x02,
            channels: vec![1, 2],
            fpp: 16,
            bits: 24,
            destinations: vec![v4([239, 69, 0, 2], 4321)],
        },
        TxFlow {
            id: 3,
            local_id: 3,
            flags: 0x11,
            channels: vec![3, 0, 0, 0],
            fpp: 4,
            bits: 24,
            destinations: vec![v4([192, 0, 2, 11], 14343)],
        },
    ]
}

/// The page with `edit` applied to its datagram, as hex.
fn edited(edit: impl FnOnce(&mut Vec<u8>)) -> String {
    let mut d = hex(CARD_27_PAGE);
    edit(&mut d);
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn set_u16(d: &mut [u8], at: usize, v: u16) {
    d[at..at + 2].copy_from_slice(&v.to_be_bytes());
}

/// Status word at 8–9, the table's count at the body's byte 1 (11).
fn status_count(d: &mut [u8], status: u16, count: u8) {
    set_u16(d, 8, status);
    d[11] = count;
}

#[test]
fn the_card_s_page_reads_as_its_three_flows() {
    let mut card = Scripted::new(&[CARD_27_PAGE]);
    let flows = list_tx_flows_27(&mut card).unwrap();
    assert_eq!(flows.flows, card_flows());
    assert_eq!((flows.slots, flows.unread), (0, 0));
    let multicast: Vec<bool> = flows.flows.iter().map(TxFlow::multicast).collect();
    assert_eq!(multicast, vec![false, true, false]);
    // One request: the usual page request from flow 1, under 0x27ff.
    assert_eq!(card.asked, vec![(0x27ff, 0x2200, vec![0, 1, 0, 1, 0, 0])]);
    assert_eq!(LIST_27, 0x2200);
}

#[test]
fn a_page_saying_more_is_followed_from_the_flow_after_its_highest_id() {
    // Page 1 counts flows 1 and 2 and says more; page 2 holds flow 3.
    let one = edited(|d| status_count(d, STATUS_MORE, 2));
    let two = edited(|d| {
        status_count(d, STATUS_OK, 1);
        set_u16(d, 12, REC[2] as u16);
    });
    let mut card = Scripted::new(&[&one, &two]);
    let flows = list_tx_flows_27(&mut card).unwrap();
    assert_eq!(flows.flows, card_flows());
    let firsts: Vec<&[u8]> = card.asked.iter().map(|a| &a.2[2..4]).collect();
    assert_eq!(firsts, vec![&[0, 1][..], &[0, 3][..]]);
    // Flow 1 holds slot 5 (a gap before it): page 2 asks from 6, and a
    // page 2 that lists flow 2 again (below 6) takes only flow 3.
    let one = edited(|d| {
        status_count(d, STATUS_MORE, 1);
        set_u16(d, REC[0], 5);
    });
    let two = edited(|d| {
        status_count(d, STATUS_OK, 2);
        set_u16(d, 12, REC[1] as u16);
        set_u16(d, 14, REC[2] as u16);
        set_u16(d, REC[2], 6);
    });
    let mut card = Scripted::new(&[&one, &two]);
    let flows = list_tx_flows_27(&mut card).unwrap();
    let ids: Vec<u16> = flows.flows.iter().map(|f| f.id).collect();
    assert_eq!(ids, vec![5, 6]);
    assert_eq!(&card.asked[1].2[2..4], &[0, 6]);
}

#[test]
fn a_page_saying_more_with_no_flow_or_without_end_is_malformed() {
    let none = edited(|d| status_count(d, STATUS_MORE, 0));
    let mut card = Scripted::new(&[&none]);
    assert_eq!(
        list_tx_flows_27(&mut card),
        Err(ReadError::Malformed { op: 0x2200 })
    );
    assert_eq!(card.asked.len(), 1);
    // A page saying more that names no flow past the one asked from.
    let more = edited(|d| status_count(d, STATUS_MORE, 1));
    let mut card = Scripted::new(&[&more]);
    assert_eq!(
        list_tx_flows_27(&mut card),
        Err(ReadError::Malformed { op: 0x2200 })
    );
    let firsts: Vec<&[u8]> = card.asked.iter().map(|a| &a.2[2..4]).collect();
    assert_eq!(firsts, vec![&[0, 1][..], &[0, 2][..]]);
    // Pages that always say more, each one flow further, end after
    // MAX_PAGES requests.
    let pages: Vec<String> = (1..=40u16)
        .map(|id| {
            edited(|d| {
                status_count(d, STATUS_MORE, 1);
                set_u16(d, REC[0], id);
            })
        })
        .collect();
    let pages: Vec<&str> = pages.iter().map(String::as_str).collect();
    let mut card = Scripted::new(&pages);
    assert_eq!(
        list_tx_flows_27(&mut card),
        Err(ReadError::Malformed { op: 0x2200 })
    );
    assert_eq!(card.asked.len(), crate::arc::client::MAX_PAGES);
}

#[test]
fn a_refusal_or_a_table_that_does_not_read_is_an_error() {
    let refused = edited(|d| {
        d.truncate(HEADER_LEN);
        set_u16(d, 8, 0x0022);
    });
    let mut card = Scripted::new(&[&refused]);
    assert_eq!(
        list_tx_flows_27(&mut card),
        Err(ReadError::Refused {
            op: 0x2200,
            status: 0x0022
        })
    );
    // More counted than slots.
    let over = edited(|d| d[10] = 2);
    let mut card = Scripted::new(&[&over]);
    assert_eq!(
        list_tx_flows_27(&mut card),
        Err(ReadError::Malformed { op: 0x2200 })
    );
}

#[test]
fn a_record_that_does_not_read_is_counted_and_the_rest_listed() {
    let expect = |skip: usize| {
        let mut f = card_flows();
        f.remove(skip);
        f
    };
    // Record 1's socket descriptor is not one.
    let tag = edited(|d| d[SOCKET_1] = 0x09);
    let read = list_tx_flows_27(&mut Scripted::new(&[&tag])).unwrap();
    assert_eq!((read.flows, read.unread), (expect(0), 1));
    // Record 2's offset is inside the header.
    let inside = edited(|d| set_u16(d, 14, 4));
    let read = list_tx_flows_27(&mut Scripted::new(&[&inside])).unwrap();
    assert_eq!((read.flows, read.unread), (expect(1), 1));
    // Record 3's offset is past the reply.
    let past = edited(|d| set_u16(d, 16, 0x0400));
    let read = list_tx_flows_27(&mut Scripted::new(&[&past])).unwrap();
    assert_eq!((read.flows, read.unread), (expect(2), 1));
    // Record 3's names descriptor is past the reply.
    let names = edited(|d| set_u16(d, REC[2] + 28, 0x0400));
    let read = list_tx_flows_27(&mut Scripted::new(&[&names])).unwrap();
    assert_eq!((read.flows, read.unread), (expect(2), 1));
    // Record 1's socket offset is inside the header.
    let socket = edited(|d| set_u16(d, REC[0] + 16, 2));
    let read = list_tx_flows_27(&mut Scripted::new(&[&socket])).unwrap();
    assert_eq!((read.flows, read.unread), (expect(0), 1));
}

#[test]
fn inferno_s_socket_tag_reads_and_a_record_without_names_has_no_fpp() {
    let page = edited(|d| {
        d[SOCKET_1] = 0x80;
        set_u16(d, REC[0] + 26, 0);
    });
    let read = list_tx_flows_27(&mut Scripted::new(&[&page])).unwrap();
    let mut expect = card_flows();
    expect[0].fpp = 0;
    assert_eq!(read.flows, expect);
    // The names descriptor's frames per packet is its word at 8–9.
    let page = edited(|d| set_u16(d, NAMES_1 + 8, 32));
    let read = list_tx_flows_27(&mut Scripted::new(&[&page])).unwrap();
    assert_eq!(read.flows[0].fpp, 32);
}

#[test]
fn a_2600_refused_as_unknown_is_read_with_0x2200_and_that_form_returned() {
    let mut card = Scripted::new(&[CARD_2600_REFUSED, CARD_27_PAGE]);
    let (flows, form) = read_tx_flows(&mut card, ListForm::V289).unwrap();
    assert_eq!((flows.flows, form), (card_flows(), ListForm::V27));
    let asked: Vec<(u16, u16)> = card.asked.iter().map(|a| (a.0, a.1)).collect();
    assert_eq!(asked, vec![(0x2809, 0x2600), (0x27ff, 0x2200)]);
}

#[test]
fn the_remembered_form_is_asked_first_and_alone_when_it_answers() {
    let mut card = Scripted::new(&[CARD_27_PAGE]);
    let (_, form) = read_tx_flows(&mut card, ListForm::V27).unwrap();
    assert_eq!((form, card.ops()), (ListForm::V27, vec![0x2200]));
    let mut ultimo = Scripted::new(&[ULTIMO_LIST]);
    let (flows, form) = read_tx_flows(&mut ultimo, ListForm::V289).unwrap();
    assert_eq!((form, ultimo.ops()), (ListForm::V289, vec![0x2600]));
    assert_eq!(flows.flows.len(), 2);
}

#[test]
fn a_0x2200_refused_as_unknown_falls_back_to_0x2600() {
    let unknown_2200 = "27ff000a710222000030";
    let mut ultimo = Scripted::new(&[unknown_2200, ULTIMO_LIST]);
    let (flows, form) = read_tx_flows(&mut ultimo, ListForm::V27).unwrap();
    assert_eq!((form, ultimo.ops()), (ListForm::V289, vec![0x2200, 0x2600]));
    assert_eq!(flows.flows.len(), 2);
}

#[test]
fn another_refusal_does_not_fall_back_and_both_refused_is_the_other_s_error() {
    // 0x0022 is not "unknown": no second form.
    let mut card = Scripted::new(&["2809000a710126000022", CARD_27_PAGE]);
    assert_eq!(
        read_tx_flows(&mut card, ListForm::V289),
        Err(ReadError::Refused {
            op: 0x2600,
            status: 0x0022
        })
    );
    assert_eq!(card.ops(), vec![0x2600]);
    // Both unknown: the second form's refusal.
    let mut none = Scripted::new(&[CARD_2600_REFUSED, "27ff000a710222000030"]);
    assert_eq!(
        read_tx_flows(&mut none, ListForm::V289),
        Err(ReadError::Refused {
            op: 0x2200,
            status: STATUS_UNKNOWN_OP
        })
    );
    assert_eq!(none.ops(), vec![0x2600, 0x2200]);
}

#[test]
fn the_forms_name_their_lists() {
    assert_eq!(ListForm::default(), ListForm::V289);
    assert_eq!((ListForm::V289.op(), ListForm::V27.op()), (0x2600, 0x2200));
    assert_eq!(ListForm::V289.other(), ListForm::V27);
    assert_eq!(ListForm::V27.other(), ListForm::V289);
    assert_eq!(STATUS_UNKNOWN_OP, 0x0030);
}
