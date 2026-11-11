use super::*;

use std::net::SocketAddrV4;

use crate::arc::flows::{
    CREATE, DELETE, MULTICAST, START_2809, TxFlow, UNICAST, create_body, delete_body,
    list_reply_body, parse_flows,
};
use crate::arc::{Change, MAX_FLOWS, flows_2809, op, tx_flows};
use crate::device::{DeviceState, SavedMulticast};
use crate::flows::control::FlowRequest;
use crate::media::tx::{MULTICAST_PORT, multicast_group};
use crate::wire::{HEADER_LEN, STATUS_ERROR, request};

const TEST_DEVICE: &str = include_str!("../../../../../config/test-device.toml");
const OUR_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 10);
/// Dante Controller's options word, as captured.
const OPTIONS: u16 = 0x0001;
/// Where the create body's one record starts (after its 10-byte table).
const RECORD: usize = 10;

/// The test device: 16 TX channels, 24 bits, `tx_fpp_max` 32.
fn cfg() -> DeviceConfig {
    DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-device.toml")
}

/// The status and body of our answer to `op1` with `body` (start code
/// 0x2809) on `state` and `tx`, and whether it changed the state
/// ([`ArcOutcome::Changed`], [`Change::TX_FLOWS_ONLY`]).
fn ask_with(
    state: &mut DeviceState,
    tx: &mut TxTable,
    op1: u16,
    body: &[u8],
) -> (u16, Vec<u8>, bool) {
    let packet = request(START_2809, 0x4242, op1, 0, body);
    let (h, body) = Header::parse(&packet).expect("our request parses");
    let (bytes, changed) = match answer(&h, body, state, &cfg(), tx, OUR_IP) {
        ArcOutcome::Reply(bytes) => (bytes, false),
        ArcOutcome::Changed(bytes, change) => {
            let flows = Change {
                tx_flows: true,
                ..Change::default()
            };
            assert_eq!(change, flows, "only our TX flows");
            assert_eq!(change, Change::TX_FLOWS_ONLY);
            (bytes, true)
        }
        other => panic!("not a reply: {other:?}"),
    };
    let (r, body) = Header::parse(&bytes).expect("our reply parses");
    assert_eq!((r.op1, r.seq), (op1, 0x4242));
    (r.op2, body.to_vec(), changed)
}

/// [`ask_with`] on a state of its own: the status and body.
fn ask(tx: &mut TxTable, op1: u16, body: &[u8]) -> (u16, Vec<u8>) {
    let mut state = DeviceState::from_config(&cfg());
    let (status, body, _) = ask_with(&mut state, tx, op1, body);
    (status, body)
}

/// Dante Controller's create of `channels` in slot `local_id`.
fn create(tx: &mut TxTable, local_id: u16, channels: &[u16]) -> (u16, Vec<u8>) {
    ask(
        tx,
        CREATE,
        &create_body(local_id, channels, OPTIONS).unwrap(),
    )
}

/// A refusal that leaves `tx` and the state as they were.
fn refused(tx: &mut TxTable, op1: u16, body: &[u8]) {
    let mut state = DeviceState::from_config(&cfg());
    state.set_multicast(0, &[1]);
    let saved = state.clone();
    let before: Vec<_> = tx.flows().map(|(s, f)| (s, f.clone())).collect();
    assert_eq!(
        ask_with(&mut state, tx, op1, body),
        (STATUS_ERROR, Vec::new(), false)
    );
    let after: Vec<_> = tx.flows().map(|(s, f)| (s, f.clone())).collect();
    assert_eq!(after, before, "nothing changed");
    assert_eq!(state, saved, "the state kept");
}

/// The flows our 0x2600 list states.
fn listed(tx: &TxTable) -> Vec<TxFlow> {
    let flows = parse_flows(&flows_2809(&cfg(), tx)).expect("our list reads");
    assert_eq!((flows.slots, flows.unread), (MAX_FLOWS, 0));
    flows.flows
}

#[test]
fn a_create_makes_a_multicast_flow_answered_with_its_record_and_no_destination() {
    let mut tx = TxTable::new(1);
    let (status, body) = create(&mut tx, 2, &[3, 1, 16]);
    assert_eq!(status, STATUS_OK);
    let reply = parse_flows(&body).expect("the reply reads");
    assert_eq!(reply.unread, 0);
    let made = TxFlow {
        id: 2,
        local_id: 2,
        flags: MULTICAST,
        channels: vec![3, 1, 16],
        fpp: 32,
        bits: 24,
        destinations: Vec::new(),
    };
    assert_eq!(reply.flows, std::slice::from_ref(&made));
    // The list states it in slot 2, with its group.
    let group = multicast_group(OUR_IP, cfg().process_id, 1);
    assert_eq!(group.port(), MULTICAST_PORT);
    assert_eq!(
        listed(&tx),
        [TxFlow {
            destinations: vec![group],
            ..made
        }]
    );
    let f = tx.flow(1).expect("the table's slot 1");
    assert!(f.multicast);
    assert_eq!((f.dest, f.fpp, f.bps), (group, 32, 3));
    assert_eq!(f.channels, [Some(3), Some(1), Some(16)]);
}

#[test]
fn a_delete_ends_the_multicast_flow_with_ok_and_no_body() {
    let mut tx = TxTable::new(1);
    assert_eq!(create(&mut tx, 1, &[1, 2]).0, STATUS_OK);
    assert_eq!(create(&mut tx, 3, &[4]).0, STATUS_OK);
    assert_eq!(
        ask(&mut tx, DELETE, &delete_body(1)),
        (STATUS_OK, Vec::new())
    );
    let left = listed(&tx);
    assert_eq!(left.iter().map(|f| f.local_id).collect::<Vec<_>>(), [3]);
    assert_eq!(tx.flow(0), None);
    // Deleted once: a second delete finds nothing.
    refused(&mut tx, DELETE, &delete_body(1));
}

/// dstack #207: a create keeps the flow in the device state (its slot and
/// channels; the group is derived at start), a delete drops it, each a
/// change for the state file; a refusal changes neither.
#[test]
fn a_create_and_a_delete_change_the_saved_multicast_flows() {
    let mut tx = TxTable::new(1);
    let mut state = DeviceState::from_config(&cfg());
    let flow = |slot, channels: &[u16]| SavedMulticast {
        slot,
        channels: channels.to_vec(),
    };
    let body = |local_id, channels: &[u16]| create_body(local_id, channels, OPTIONS).unwrap();
    let (status, _, changed) = ask_with(&mut state, &mut tx, CREATE, &body(3, &[2, 5]));
    assert_eq!((status, changed), (STATUS_OK, true));
    let (status, _, changed) = ask_with(&mut state, &mut tx, CREATE, &body(1, &[1]));
    assert_eq!((status, changed), (STATUS_OK, true));
    assert_eq!(state.multicast, [flow(0, &[1]), flow(2, &[2, 5])]);
    // A slot in use: refused, the saved flow as it was.
    assert_eq!(
        ask_with(&mut state, &mut tx, CREATE, &body(3, &[4])),
        (STATUS_ERROR, Vec::new(), false)
    );
    assert_eq!(state.multicast, [flow(0, &[1]), flow(2, &[2, 5])]);
    assert_eq!(
        ask_with(&mut state, &mut tx, DELETE, &delete_body(3)),
        (STATUS_OK, Vec::new(), true)
    );
    assert_eq!(state.multicast, [flow(0, &[1])]);
    // Deleted once: the second delete is refused and changes nothing.
    assert_eq!(
        ask_with(&mut state, &mut tx, DELETE, &delete_body(3)),
        (STATUS_ERROR, Vec::new(), false)
    );
    assert_eq!(state.multicast, [flow(0, &[1])]);
}

#[test]
fn a_create_in_a_slot_in_use_or_past_ours_is_refused() {
    let mut tx = TxTable::with_slots(1, 4);
    assert_eq!(create(&mut tx, 4, &[1]).0, STATUS_OK, "the last slot");
    refused(&mut tx, CREATE, &create_body(4, &[2], OPTIONS).unwrap());
    refused(&mut tx, CREATE, &create_body(5, &[2], OPTIONS).unwrap());
    // A receiver's flow holds its slot.
    let rx = SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 11), 50_000);
    let req = FlowRequest {
        rx_host: "peer-device".to_string(),
        rx_flow_name: "flow-1".to_string(),
        sample_rate: 96_000,
        bits: 24,
        channels: vec![Some(1)],
        fpp: 16,
        rx,
    };
    let (slot, _) = tx.add(&req, 0).unwrap();
    let local_id = u16::try_from(slot + 1).unwrap();
    refused(
        &mut tx,
        CREATE,
        &create_body(local_id, &[2], OPTIONS).unwrap(),
    );
    // A delete of it is refused too: it is the receiver's.
    refused(&mut tx, DELETE, &delete_body(local_id));
}

#[test]
fn a_create_of_a_channel_we_do_not_have_is_refused() {
    let mut tx = TxTable::new(1);
    assert_eq!(cfg().tx.len(), 16);
    refused(&mut tx, CREATE, &create_body(1, &[1, 17], OPTIONS).unwrap());
    assert_eq!(create(&mut tx, 1, &[16]).0, STATUS_OK);
}

#[test]
fn a_create_that_is_not_one_multicast_record_of_a_real_slot_is_refused() {
    let mut tx = TxTable::new(1);
    let good = create_body(1, &[1, 2], OPTIONS).unwrap();
    // Media-local id 0: no slot.
    let mut slot_0 = good.clone();
    slot_0[RECORD + 8..RECORD + 10].copy_from_slice(&[0, 0]);
    refused(&mut tx, CREATE, &slot_0);
    // A unicast record.
    let mut unicast = good.clone();
    unicast[RECORD + 12..RECORD + 16].copy_from_slice(&UNICAST.to_be_bytes());
    refused(&mut tx, CREATE, &unicast);
    // A record that does not read (its offset past the body).
    let mut lost = good.clone();
    lost[8..10].copy_from_slice(&[0xff, 0xff]);
    refused(&mut tx, CREATE, &lost);
    // No table at all.
    refused(&mut tx, CREATE, &good[..6]);
    refused(&mut tx, CREATE, &[]);
    // Two records.
    let flow = |local_id| TxFlow {
        id: 0,
        local_id,
        flags: MULTICAST,
        channels: vec![1],
        fpp: 0,
        bits: 24,
        destinations: Vec::new(),
    };
    refused(&mut tx, CREATE, &list_reply_body(&[flow(1), flow(2)]));
    refused(&mut tx, CREATE, &list_reply_body(&[]));
    // One record that reads beside one that does not.
    let mut half = list_reply_body(&[flow(1), flow(2)]);
    half[10..12].copy_from_slice(&[0xff, 0xff]);
    let read = parse_flows(&half).unwrap();
    assert_eq!((read.flows.len(), read.unread), (1, 1));
    refused(&mut tx, CREATE, &half);
    // One such record is a create.
    assert_eq!(
        ask(&mut tx, CREATE, &list_reply_body(&[flow(2)])).0,
        STATUS_OK
    );
    assert_eq!(create(&mut tx, 1, &[1, 2]).0, STATUS_OK);
}

#[test]
fn a_delete_without_its_flow_is_refused() {
    let mut tx = TxTable::new(1);
    assert_eq!(create(&mut tx, 1, &[1]).0, STATUS_OK);
    refused(&mut tx, DELETE, &delete_body(0));
    refused(&mut tx, DELETE, &delete_body(1)[..13]);
    refused(&mut tx, DELETE, &[]);
    refused(&mut tx, DELETE, &delete_body(2));
    refused(&mut tx, DELETE, &delete_body(u16::MAX));
    assert_eq!(ask(&mut tx, DELETE, &delete_body(1)).0, STATUS_OK);
}

#[test]
fn the_tx_flow_page_states_a_multicast_flow_s_type_and_group() {
    let mut tx = TxTable::new(1);
    assert_eq!(create(&mut tx, 1, &[1, 2]).0, STATUS_OK);
    let packet = request(0x2729, 7, op::TX_FLOWS, 0, &[0, 1, 0, 1, 0, 0]);
    let (h, body) = Header::parse(&packet).unwrap();
    let ArcOutcome::Reply(bytes) = tx_flows(&h, body, &cfg(), &tx) else {
        panic!("the page is answered");
    };
    let body = &bytes[HEADER_LEN..];
    assert_eq!(body[1], 1, "one flow");
    let main = usize::from(read_u16(body, 2).unwrap()) - HEADER_LEN;
    assert_eq!(read_u16(body, main), Some(1), "flow 1");
    assert_eq!(read_u16(body, main + 2), Some(0x02), "multicast");
    let socket = usize::from(read_u16(body, main + 16).unwrap()) - HEADER_LEN;
    let group = multicast_group(OUR_IP, cfg().process_id, 0);
    let mut want = vec![0x80, 0x02];
    want.extend(group.port().to_be_bytes());
    want.extend(group.ip().octets());
    assert_eq!(body[socket..socket + 8], want);
}
