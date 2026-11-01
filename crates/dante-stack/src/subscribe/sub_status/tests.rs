use super::*;

#[test]
fn the_codes_our_server_lists_are_inferno_s() {
    for (status, code) in [
        (Unresolved, 1),
        (InProgress, 8),
        (TooManyTxFlows, 0x14),
        (TxFail, 0x15),
        (Receiving, 0x0101_0009),
    ] {
        assert_eq!(status.code(), code, "{status:?}");
        assert_eq!(SubStatus::from_code(code), Some(status));
    }
}

#[test]
fn the_codes_seen_on_the_site_network_are_named() {
    // dstack #44: 42 channels from hardware transmitters, audio flowing.
    let code = 0x0301_0009;
    assert_eq!(SubStatus::from_code(code), Some(Receiving));
    assert_eq!(receiver_detail(code), 0x0301);
    assert_eq!(Receiving.state(), SubState::Connected);
    assert_eq!(Receiving.label(), "Subscribed (unicast)");
    assert_eq!(Receiving.dante_name(), "DYNAMIC");
    // A card's channels subscribed to its own TX.
    assert_eq!(SubStatus::from_code(4), Some(SubscribedSelf));
    assert_eq!(receiver_detail(4), 0);
    assert_eq!(SubscribedSelf.state(), SubState::Connected);
    assert_eq!(SubscribedSelf.label(), "Subscribed (self)");
}

#[test]
fn every_status_has_one_value_name_and_label() {
    let all = SubStatus::ALL;
    assert_eq!(all.len(), 48);
    for (i, s) in all.iter().enumerate() {
        assert_eq!(SubStatus::from_code(u32::from(s.value())), Some(*s));
        assert_eq!(SubStatus::from_code(s.code()), Some(*s));
        assert!(!s.label().is_empty() && !s.dante_name().is_empty(), "{s:?}");
        for t in &all[i + 1..] {
            assert!(s.value() < t.value(), "{s:?} {t:?}: ordered, distinct");
            assert_ne!(s.dante_name(), t.dante_name());
            assert_ne!(s.label(), t.label(), "{s:?} {t:?}");
        }
        // Only Receiving carries a receiver detail.
        let detail = receiver_detail(s.code());
        assert_eq!(detail != 0, *s == Receiving, "{s:?}");
    }
    assert_eq!(all[0], Unresolved);
    assert_eq!(all[47], SystemFail);
}

#[test]
fn a_word_reads_its_low_half_whatever_the_detail() {
    assert_eq!(SubStatus::from_code(0), None, "no subscription");
    assert_eq!(SubStatus::from_code(6), None, "not in the table");
    assert_eq!(SubStatus::from_code(0xffff), None);
    assert_eq!(
        SubStatus::from_code(0x0001_0000),
        None,
        "a detail, no status"
    );
    assert_eq!(SubStatus::from_code(0xabcd_0015), Some(TxFail));
    assert_eq!(SubStatus::from_code(0x0000_00ff), Some(SystemFail));
    assert_eq!(SubStatus::from_code(0x0100_000a), Some(ReceivingMulticast));
    assert_eq!(receiver_detail(0xabcd_0015), 0xabcd);
    assert_eq!(receiver_detail(0x0000_ffff), 0);
}

#[test]
fn the_states_follow_dante_controller() {
    for (status, state) in [
        (Unresolved, SubState::Pending),
        (Resolved, SubState::Pending),
        (Idle, SubState::Idle),
        (InProgress, SubState::Pending),
        (ReceivingMulticast, SubState::Connected),
        (Manual, SubState::Connected),
        (ResolveFail, SubState::Error),
        (TxFail, SubState::Error),
        (TxSchedulerFailure, SubState::Error),
        (TxNotReady, SubState::Pending),
        (TxAccessPending, SubState::Pending),
        (SystemFail, SubState::Error),
    ] {
        assert_eq!(status.state(), state, "{status:?}");
    }
    // Spot checks against netaudio 0.4.2's table.
    for (status, value, name, label) in [
        (SourceMissing, 5, "RESOLVED_NONE", "Source not present"),
        (TooManyRxFlows, 18, "NO_RX", "No more flows (Rx)"),
        (ClockDomain, 27, "CLOCK_DOMAIN", "Clock domain mismatch"),
        (
            TooManyUnicastTxFlows,
            37,
            "TX_FANOUT_LIMIT_REACHED",
            "No more unicast flows (Tx)",
        ),
        (TemplateFull, 68, "TEMPLATE_FULL", "Template full"),
        (
            RxTransportUnsupported,
            114,
            "RX_TRANSPORT_UNSUPPORTED",
            "Transport unsupported (Rx)",
        ),
    ] {
        assert_eq!(
            (status.value(), status.dante_name(), status.label()),
            (value, name, label)
        );
    }
}
