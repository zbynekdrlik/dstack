// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;

#[test]
fn the_range_is_32_slots_of_64_ports_from_29800() {
    assert_eq!(
        (FLOW_PORT_FIRST, FLOW_PORT_LAST, SLOT_STRIDE, TX_OFFSET),
        (29_800, 31_847, 64, 32)
    );
    assert_eq!(slot_bases(0), (29_800, 29_832));
    assert_eq!(slot_bases(1), (29_864, 29_896));
    assert_eq!(slot_bases(31), (31_784, 31_816));
    // Slot 31's TX block ends at the range's last port.
    assert_eq!(slot_bases(31).1 + TX_OFFSET - 1, FLOW_PORT_LAST);
}

#[test]
fn a_slot_is_an_arc_port_from_4440_to_4471() {
    assert_eq!(slot_of(4439), None);
    assert_eq!(slot_of(4440), Some(0));
    assert_eq!(slot_of(4471), Some(31));
    assert_eq!(slot_of(4472), None);
    assert_eq!(slot_of(0), None);
}

#[test]
fn unset_bases_are_the_slot_s_and_warn_nothing() {
    let b = flow_bases(None, None, 4441).unwrap();
    assert_eq!(
        b,
        FlowBases {
            rx: 29_864,
            tx: 29_896,
            slot: Some(1),
            warnings: vec![],
        }
    );
}

#[test]
fn a_configured_base_wins_and_the_other_is_the_slot_s() {
    // The device's own block, set by hand: no warning.
    let b = flow_bases(Some(29_800), None, 4440).unwrap();
    assert_eq!((b.rx, b.tx, b.slot), (29_800, 29_832, Some(0)));
    assert!(b.warnings.is_empty(), "{:?}", b.warnings);
    let b = flow_bases(None, Some(29_896), 4441).unwrap();
    assert_eq!((b.rx, b.tx), (29_864, 29_896));
    assert!(b.warnings.is_empty(), "{:?}", b.warnings);
}

#[test]
fn a_configured_base_on_another_slot_s_block_is_warned() {
    // A base set before #264 (30000) on a slot-0 device: slot 3's ports.
    let b = flow_bases(Some(30_000), None, 4440).unwrap();
    assert_eq!((b.rx, b.tx), (30_000, 29_832));
    assert_eq!(
        b.warnings,
        [
            "rx_port_base: its ports 30000-30031 are another slot's block, not this device's \
          own (29800-29831); a device of ours in that slot on this address cannot start: \
          remove the key, the default is the device's own block (docs/two-networks.md)"
        ]
    );
    let b = flow_bases(None, Some(29_833), 4440).unwrap();
    assert_eq!(b.warnings.len(), 1);
    assert!(b.warnings[0].starts_with("tx_port_base: its ports 29833-29864 are another"));
    // No slot: no own block to compare with.
    let b = flow_bases(Some(30_000), Some(30_032), 5000).unwrap();
    assert!(b.warnings.is_empty(), "{:?}", b.warnings);
}

#[test]
#[should_panic(expected = "slot 32 past the 32 slots")]
fn a_slot_past_the_last_has_no_block() {
    let _ = slot_bases(PORT_SLOTS);
}

#[test]
fn a_configured_block_leaving_the_range_at_either_end_is_warned() {
    // Inside at both ends (slot 0's RX, slot 31's TX): no warning.
    let first = flow_bases(Some(FLOW_PORT_FIRST), None, 4440).unwrap();
    assert!(first.warnings.is_empty(), "{:?}", first.warnings);
    let last = flow_bases(None, Some(FLOW_PORT_LAST - 31), 4471).unwrap();
    assert!(last.warnings.is_empty(), "{:?}", last.warnings);
    // One port below the first.
    let below = flow_bases(Some(FLOW_PORT_FIRST - 1), None, 4441).unwrap();
    assert_eq!(below.warnings.len(), 1);
    assert!(
        below.warnings[0].starts_with(
            "rx_port_base: its ports 29799-29830 leave the dstack flow range 29800-31847"
        ),
        "{}",
        below.warnings[0]
    );
    // One port past the last.
    let past = flow_bases(None, Some(FLOW_PORT_LAST - 30), 4440).unwrap();
    assert_eq!(past.warnings.len(), 1);
    assert!(
        past.warnings[0].starts_with("tx_port_base: its ports 31817-31848 leave"),
        "{}",
        past.warnings[0]
    );
    // Both far outside: one warning each, RX first.
    let both = flow_bases(Some(47_000), Some(47_100), 4440).unwrap();
    assert_eq!(both.warnings.len(), 2);
    assert!(both.warnings[0].starts_with("rx_port_base"));
    assert!(both.warnings[1].starts_with("tx_port_base"));
}

#[test]
fn no_slot_and_a_base_unset_is_refused_naming_the_key() {
    let e = flow_bases(None, None, 4472).unwrap_err();
    assert_eq!(
        e,
        "rx_port_base: unset, and the ARC port 4472 is no device slot's (4440-4471), so the \
         flow ports have no default; set rx_port_base and tx_port_base inside the dstack flow \
         range 29800-31847 (docs/two-networks.md)"
    );
    let e = flow_bases(Some(30_000), None, 5000).unwrap_err();
    assert!(e.starts_with("tx_port_base: unset"), "{e}");
    // Both set: no slot needed.
    let b = flow_bases(Some(30_000), Some(30_032), 5000).unwrap();
    assert_eq!((b.rx, b.tx, b.slot), (30_000, 30_032, None));
}

#[test]
fn rx_and_tx_blocks_that_overlap_are_refused_and_touching_ones_are_not() {
    // TX starting on RX's last port.
    let e = flow_bases(Some(30_000), Some(30_031), 4440).unwrap_err();
    assert_eq!(
        e,
        "the RX flow ports 30000-30031 overlap the TX flow ports 30031-30062: set rx_port_base \
         and tx_port_base 32 ports apart"
    );
    // RX starting on TX's last port.
    assert!(flow_bases(Some(30_031), Some(30_000), 4440).is_err());
    // A configured RX on the slot's default TX block.
    assert!(flow_bases(Some(29_832), None, 4440).is_err());
    // Back to back, either way round.
    assert!(flow_bases(Some(30_000), Some(30_032), 4440).is_ok());
    assert!(flow_bases(Some(30_032), Some(30_000), 4440).is_ok());
}
