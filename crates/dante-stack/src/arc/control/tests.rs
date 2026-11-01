use super::*;

use std::net::Ipv4Addr;

use crate::arc::{ArcOutcome, Live, handle};
use crate::device::{DeviceConfig, DeviceState};
use crate::media::tx::TxTable;
use crate::subscribe::{MAX_FLOWS, Manager};
use crate::wire::Header;

const TEST_DEVICE: &str = include_str!("../../../../../config/test-dante-device.toml");

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---- the requests, byte for byte as netaudio v0.0.10 sends them ----

#[test]
fn an_rx_label_is_netaudio_s_request() {
    // netaudio: "27ff 00{len} {seq} 3001 0000 0201 00{ch} 0014 00000000
    // {name} 00", len = name + 21.
    let want = [
        "27ff", "0018", "0000", "3001", "0000", "0201", "0005", "0014", "00000000", "566f78", "00",
    ];
    assert_eq!(
        hex(&rename_request(0, Dir::Rx, 5, Some("Vox"))),
        want.concat()
    );
    let reset = [
        "27ff", "0015", "0000", "3001", "0000", "0201", "0005", "0014", "00000000", "00",
    ];
    assert_eq!(hex(&rename_request(0, Dir::Rx, 5, None)), reset.concat());
}

#[test]
fn a_tx_label_is_netaudio_s_request() {
    // netaudio: "... 2013 0000 0201 0000 00{ch} 0018 000000000000 {name}
    // 00", len = name + 25.
    let want = [
        "27ff",
        "001e",
        "0000",
        "2013",
        "0000",
        "0201",
        "0000",
        "0007",
        "0018",
        "000000000000",
        "436c69636b",
        "00",
    ];
    assert_eq!(
        hex(&rename_request(0, Dir::Tx, 7, Some("Click"))),
        want.concat()
    );
    let reset = [
        "27ff",
        "0019",
        "0000",
        "2013",
        "0000",
        "0201",
        "0000",
        "0007",
        "0018",
        "000000000000",
        "00",
    ];
    assert_eq!(hex(&rename_request(0, Dir::Tx, 7, None)), reset.concat());
}

#[test]
fn a_device_name_is_netaudio_s_request() {
    // netaudio: "27ff 00{len} {seq} 1001 0000 {name} 00", len = name + 11.
    let want = [
        "27ff",
        "0013",
        "0000",
        "1001",
        "0000",
        &hex(b"iem-card"),
        "00",
    ];
    assert_eq!(hex(&device_rename_request(0, "iem-card")), want.concat());
}

// ---- against our own server ----

/// Our own device answering in process: its config, state and flows.
pub(crate) struct Own {
    pub(crate) cfg: DeviceConfig,
    pub(crate) state: DeviceState,
    tx: TxTable,
    rx: Manager,
    seq: u16,
}

impl Own {
    pub(crate) fn new() -> Self {
        let cfg = DeviceConfig::from_toml(TEST_DEVICE).expect("the test device");
        let rx = Manager::new(&cfg, &cfg.name, Ipv4Addr::new(10, 0, 0, 10), [0; MAX_FLOWS]);
        Own {
            state: DeviceState::from_config(&cfg),
            tx: TxTable::new(7),
            rx,
            cfg,
            seq: 0,
        }
    }
}

impl Call for Own {
    fn call(&mut self, op1: u16, body: &[u8]) -> Result<(u16, Vec<u8>), ReadError> {
        self.seq = self.seq.wrapping_add(1);
        let req = request(START, self.seq, op1, 0, body);
        let live = Live {
            tx: &self.tx,
            rx: &self.rx,
        };
        let bytes = match handle(&mut self.state, &self.cfg, &live, &req) {
            ArcOutcome::Reply(b) | ArcOutcome::Changed(b, _) => b,
            // No reply: as a device that does not answer.
            _ => return Err(ReadError::Timeout { op: op1 }),
        };
        let (h, body) = Header::parse(&bytes).expect("a reply parses");
        assert_eq!((h.seq, h.op1), (self.seq, op1), "the reply to our request");
        Ok((h.op2, body.to_vec()))
    }
}

#[test]
fn our_device_s_name_and_labels_read_back() {
    let mut own = Own::new();
    assert_eq!(device_name(&mut own).unwrap(), own.cfg.name);
    let got = labels(&mut own).unwrap();
    assert_eq!(
        (got.tx_count, got.rx_count),
        (own.cfg.tx.len(), own.cfg.rx.len())
    );
    assert!(got.complete(), "{got:?}");
    for (i, name) in own.cfg.tx.iter().enumerate() {
        assert_eq!(got.get(Dir::Tx, i as u16 + 1), Some(name.as_str()));
    }
    for (i, name) in own.cfg.rx.iter().enumerate() {
        assert_eq!(got.get(Dir::Rx, i as u16 + 1), Some(name.as_str()));
    }
}

#[test]
fn a_label_set_and_reset_reads_back() {
    let mut own = Own::new();
    rename_channel(&mut own, Dir::Rx, 2, Some("Vox L")).unwrap();
    rename_channel(&mut own, Dir::Tx, 3, Some("Click")).unwrap();
    assert_eq!(own.state.rx[1], "Vox L");
    assert_eq!(own.state.tx[2], "Click");
    let got = labels(&mut own).unwrap();
    assert_eq!(got.get(Dir::Rx, 2), Some("Vox L"));
    assert_eq!(got.get(Dir::Tx, 3), Some("Click"));
    // Reset: the default comes back.
    rename_channel(&mut own, Dir::Rx, 2, None).unwrap();
    rename_channel(&mut own, Dir::Tx, 3, None).unwrap();
    let got = labels(&mut own).unwrap();
    assert_eq!(got.get(Dir::Rx, 2), Some(own.cfg.rx[1].as_str()));
    assert_eq!(got.get(Dir::Tx, 3), Some(own.cfg.tx[2].as_str()));
}

#[test]
fn a_refused_label_is_a_refusal() {
    let mut own = Own::new();
    let past = own.cfg.rx.len() as u16 + 1;
    assert!(matches!(
        rename_channel(&mut own, Dir::Rx, past, Some("X")),
        Err(ReadError::Refused {
            op: op::RX_RENAME,
            ..
        })
    ));
    assert!(matches!(
        rename_channel(&mut own, Dir::Tx, 1, Some("no.dots")),
        Err(ReadError::Refused {
            op: op::TX_RENAME,
            ..
        })
    ));
}

#[test]
fn our_device_takes_a_device_rename() {
    let mut own = Own::new();
    rename_device(&mut own, "other-name").unwrap();
    assert_eq!(device_name(&mut own).unwrap(), "other-name");
    assert_eq!(own.state.name, "other-name");
}

/// A device whose name reply is empty, or that refuses.
struct Fixed(u16, Vec<u8>);

impl Call for Fixed {
    fn call(&mut self, _: u16, _: &[u8]) -> Result<(u16, Vec<u8>), ReadError> {
        Ok((self.0, self.1.clone()))
    }
}

#[test]
fn an_empty_or_refused_name_does_not_read() {
    assert_eq!(
        device_name(&mut Fixed(STATUS_OK, vec![0])),
        Err(ReadError::Malformed { op: GET_NAME })
    );
    assert_eq!(
        device_name(&mut Fixed(0x0022, Vec::new())),
        Err(ReadError::Refused {
            op: GET_NAME,
            status: 0x0022
        })
    );
}

#[test]
fn rx_channels_read_back_labels_and_subscriptions() {
    use crate::device::Subscription;
    use crate::subscribe::SubStatus;

    let mut own = Own::new();
    let sub = Subscription {
        tx_channel: "Click".to_string(),
        tx_device: "other-device".to_string(),
    };
    assert!(own.state.set_subscription(&own.cfg, 1, Some(sub)));
    let got = rx_channels(&mut own).unwrap();
    assert_eq!(got.len(), own.cfg.rx.len());
    for (i, ch) in got.iter().enumerate() {
        assert_eq!(ch.channel, i as u16 + 1);
        assert_eq!(ch.label, own.cfg.rx[i]);
    }
    let first = &got[0];
    assert_eq!(
        (first.tx_channel.as_deref(), first.tx_device.as_deref()),
        (None, None)
    );
    assert_eq!((first.status, first.sub_status()), (0, None));
    let second = &got[1];
    assert_eq!(second.tx_channel.as_deref(), Some("Click"));
    assert_eq!(second.tx_device.as_deref(), Some("other-device"));
    // The manager does not run it: the server lists it unresolved.
    assert_eq!(second.status, SubStatus::Unresolved.code());
    assert_eq!(second.sub_status(), Some(SubStatus::Unresolved));
}

#[test]
fn rx_channels_of_a_device_that_refuses_its_counts_is_a_refusal() {
    assert_eq!(
        rx_channels(&mut Fixed(0x0022, Vec::new())),
        Err(ReadError::Refused {
            op: op::COUNTS,
            status: 0x0022
        })
    );
    assert_eq!(
        rx_channels(&mut Fixed(STATUS_OK, vec![0, 0, 0])),
        Err(ReadError::Malformed { op: op::COUNTS })
    );
}

#[test]
fn an_rx_page_reads_the_status_s_two_halves_and_names_by_offset() {
    // [slots 1, count 1], one descriptor, then the names. Offsets count from
    // the datagram's first byte (the header's 10 bytes before the body).
    let names = 2 + RX_ITEM;
    let off = |k: usize| (HEADER_LEN + names + k) as u16;
    let mut body = vec![1, 1];
    for f in [3u16, 6, 0, off(0), off(6), off(13), 0x0101, 0x0009, 0, 0] {
        body.extend_from_slice(&f.to_be_bytes());
    }
    body.extend_from_slice(b"Click\0other\0\0Vox L\0");
    let page = rx_page(STATUS_MORE, &body).expect("a page");
    assert!(page.more);
    assert_eq!((page.slots, page.count), (1, 1));
    assert_eq!(
        page.items,
        vec![RxChannel {
            channel: 3,
            label: "Vox L".to_string(),
            tx_channel: Some("Click".to_string()),
            tx_device: Some("other".to_string()),
            status: 0x0101_0009,
        }]
    );
    assert_eq!(
        page.items[0].sub_status(),
        Some(crate::subscribe::SubStatus::Receiving)
    );
}

#[test]
fn an_rx_channel_names_a_hardware_transmitter_s_receiving_word() {
    // dstack #44: the word real transmitters give a receiving channel.
    let ch = RxChannel {
        channel: 1,
        label: "In 1".to_string(),
        tx_channel: Some("Out 1".to_string()),
        tx_device: Some("other".to_string()),
        status: 0x0301_0009,
    };
    assert_eq!(
        ch.sub_status(),
        Some(crate::subscribe::SubStatus::Receiving)
    );
    assert_eq!(ch.receiver_detail(), 0x0301);
    let own = RxChannel {
        status: 0x0101_0009,
        ..ch.clone()
    };
    assert_eq!(own.receiver_detail(), 0x0101);
}

#[test]
fn an_rx_page_counts_a_channel_0_and_does_not_list_it() {
    // [slots 2, count 2]: channel 0, then channel 1, both with a name.
    let names = 2 + 2 * RX_ITEM;
    let off = |k: usize| (HEADER_LEN + names + k) as u16;
    let mut body = vec![2, 2];
    for id in [0u16, 1] {
        for f in [id, 6, 0, 0, 0, off(0), 0, 0, 0, 0] {
            body.extend_from_slice(&f.to_be_bytes());
        }
    }
    body.extend_from_slice(b"Vox\0");
    let page = rx_page(STATUS_OK, &body).expect("a page");
    assert!(!page.more);
    assert_eq!((page.slots, page.count), (2, 2));
    let listed: Vec<u16> = page.items.iter().map(|c| c.channel).collect();
    assert_eq!(listed, vec![1]);
}

#[test]
fn a_subscription_is_netaudio_s_request() {
    // netaudio: "27ff 00{len} {seq} 3010 0000 0201 00{rx} 0034 00{device
    // offset} " + 34 zero bytes + "{tx channel} 00 {tx device} 00".
    let want = [
        "27ff",
        "0043",
        "0000",
        "3010",
        "0000",
        "0201",
        "0003",
        "0034",
        "003a",
        &"00".repeat(34),
        &hex(b"Click"),
        "00",
        &hex(b"stage-pc"),
        "00",
    ];
    assert_eq!(
        hex(&subscribe_request(0, 3, "Click", "stage-pc")),
        want.concat()
    );
}

#[test]
fn an_unsubscription_is_netaudio_s_request() {
    // netaudio: "27ff 0010 {seq} 3014 0000 0001 0000 00{rx}".
    let want = [
        "27ff", "0010", "0000", "3014", "0000", "0001", "0000", "0004",
    ];
    assert_eq!(hex(&unsubscribe_request(0, 4)), want.concat());
}

#[test]
fn our_device_takes_a_subscription_and_its_end() {
    let mut own = Own::new();
    subscribe(&mut own, 2, "Click", "stage-pc").unwrap();
    let got = rx_channels(&mut own).unwrap();
    assert_eq!(got[1].tx_channel.as_deref(), Some("Click"));
    assert_eq!(got[1].tx_device.as_deref(), Some("stage-pc"));
    assert_eq!(
        own.state.subscription(1).map(|s| s.tx_device.as_str()),
        Some("stage-pc")
    );
    unsubscribe(&mut own, 2).unwrap();
    let got = rx_channels(&mut own).unwrap();
    assert_eq!((got[1].tx_channel.as_deref(), got[1].status), (None, 0));
    assert!(own.state.subscription(1).is_none());
}

#[test]
fn a_refused_subscription_or_end_is_a_refusal() {
    assert_eq!(
        subscribe(&mut Fixed(0x0022, Vec::new()), 1, "a", "b"),
        Err(ReadError::Refused {
            op: op::RX_SUBSCRIBE,
            status: 0x0022
        })
    );
    assert_eq!(
        unsubscribe(&mut Fixed(0x0022, Vec::new()), 1),
        Err(ReadError::Refused {
            op: op::RX_UNSUBSCRIBE,
            status: 0x0022
        })
    );
    assert_eq!(
        subscribe(&mut Fixed(STATUS_OK, Vec::new()), 1, "a", "b"),
        Ok(())
    );
    assert_eq!(unsubscribe(&mut Fixed(STATUS_OK, Vec::new()), 1), Ok(()));
}
