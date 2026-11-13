// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

fn texts<T>(list: &[Choice<T>]) -> Vec<&str> {
    list.iter().map(|c| c.text.as_str()).collect()
}

#[test]
fn the_buffers_are_dstack_s_blocks_with_their_time() {
    let list = blocks(256, 96_000);
    assert_eq!(
        texts(&list),
        [
            "16 samples (0.17 ms @ 96 kHz)",
            "32 samples (0.33 ms @ 96 kHz)",
            "64 samples (0.67 ms @ 96 kHz)",
            "128 samples (1.33 ms @ 96 kHz)",
            "256 samples (2.67 ms @ 96 kHz)",
        ]
    );
    assert_eq!(index_of(&list, &128), 3);
    let odd = blocks(48, 44_100);
    assert_eq!(
        odd.iter().map(|c| c.value).collect::<Vec<_>>(),
        [16, 32, 48, 64, 128, 256]
    );
    assert_eq!(odd[2].text, "48 samples (1.09 ms @ 44.1 kHz)");
    assert!(odd.iter().all(|c| !c.greyed));
}

#[test]
fn a_value_not_in_a_list_selects_the_first() {
    assert_eq!(index_of(&blocks(256, 96_000), &1000), 0);
}

#[test]
fn rate_in_khz() {
    assert_eq!(khz(96_000), "96");
    assert_eq!(khz(44_100), "44.1");
    assert_eq!(khz(88_200), "88.2");
}

#[test]
fn rx_latencies_are_dante_s_inside_the_device_s_range() {
    assert_eq!(
        texts(&rx_latencies(2_000_000, None)),
        ["0.25 ms", "0.5 ms", "1 ms", "2 ms", "5 ms", "10 ms"]
    );
    let list = rx_latencies(2_000_000, Some((500_000, 5_000_000)));
    assert_eq!(texts(&list), ["0.5 ms", "1 ms", "2 ms", "5 ms"]);
    let odd = rx_latencies(3_000_000, Some((1_000_000, 2_000_000)));
    assert_eq!(texts(&odd), ["1 ms", "2 ms", "3 ms"]);
    assert_eq!(odd[2].value, 3_000_000);
}

#[test]
fn tx_latencies_start_with_the_rx_latency_s() {
    let list = tx_latencies(Some(1_000_000));
    assert_eq!(
        texts(&list),
        [
            "Same as the RX latency",
            "0.25 ms",
            "0.5 ms",
            "1 ms",
            "2 ms",
            "5 ms",
            "10 ms"
        ]
    );
    assert_eq!(list[0].value, None);
    assert_eq!(list[3].value, Some(1_000_000));
    let odd = tx_latencies(Some(1_500_000));
    assert_eq!(odd[4].text, "1.5 ms");
    assert_eq!(odd.len(), 8);
    assert_eq!(tx_latencies(None).len(), 7);
}

#[test]
fn channel_counts_are_2_to_64_and_the_current_one() {
    let list = channels(16);
    assert_eq!(list.len(), 63);
    assert_eq!(list[0].value, 2);
    assert_eq!(list[62].value, 64);
    assert_eq!(list[14].text, "16");
    let one = channels(1);
    assert_eq!(one.len(), 64);
    assert_eq!(one[0].value, 1);
    assert_eq!(channels(65).last().map(|c| c.value), Some(65));
}

#[test]
fn cpus_list_any_then_each_cpu_the_busy_greyed() {
    let list = cpus(4, &BTreeSet::from([1, 3]), Some(2));
    assert_eq!(texts(&list), ["Any", "CPU 0", "CPU 1", "CPU 2", "CPU 3"]);
    let grey: Vec<bool> = list.iter().map(|c| c.greyed).collect();
    assert_eq!(grey, [false, false, true, false, true]);
    assert_eq!(list[0].value, None);
    assert_eq!(list[3].value, Some(2));
    let past = cpus(2, &BTreeSet::new(), Some(5));
    assert_eq!(texts(&past), ["Any", "CPU 0", "CPU 1", "CPU 5"]);
    assert_eq!(cpus(2, &BTreeSet::new(), Some(1)).len(), 3);
}

fn adapter(alias: &str, ip: Option<[u8; 4]>, mbit: Option<u64>) -> Adapter {
    Adapter {
        alias: alias.into(),
        description: format!("{alias} card"),
        ipv4: ip.map(Ipv4Addr::from),
        mbit,
    }
}

#[test]
fn an_adapter_shows_its_alias_card_address_and_speed() {
    assert_eq!(
        adapter_text(&adapter("Ethernet", Some([10, 0, 0, 2]), Some(2500))),
        "Ethernet · Ethernet card · 10.0.0.2 · 2.5 Gbps"
    );
    assert_eq!(
        adapter_text(&adapter("E", Some([10, 0, 0, 2]), Some(1000))),
        "E · E card · 10.0.0.2 · 1 Gbps"
    );
    assert_eq!(
        adapter_text(&adapter("E", None, Some(100))),
        "E · E card · 100 Mbps"
    );
    let mut bare = adapter("E", None, None);
    bare.description.clear();
    assert_eq!(adapter_text(&bare), "E");
}

#[test]
fn the_alias_of_an_address_is_its_adapter_s() {
    let all = [
        adapter("A", Some([10, 0, 0, 1]), None),
        adapter("B", Some([10, 0, 0, 2]), None),
    ];
    assert_eq!(
        alias_of(&all, Ipv4Addr::new(10, 0, 0, 2)).as_deref(),
        Some("B")
    );
    assert_eq!(alias_of(&all, Ipv4Addr::new(10, 0, 0, 3)), None);
}

#[test]
fn the_interfaces_offer_the_installed_one_the_adapters_and_a_missing_alias() {
    let all = [
        adapter("A", Some([10, 0, 0, 1]), None),
        adapter("Off", None, None),
        adapter("B", Some([10, 0, 0, 2]), None),
    ];
    let none = interfaces(&all, None, Some("B"));
    assert_eq!(
        texts(&none),
        [
            "As installed (now B)",
            "A · A card · 10.0.0.1",
            "B · B card · 10.0.0.2"
        ]
    );
    assert_eq!(none[0].value, None);
    assert_eq!(none[2].value.as_deref(), Some("B"));
    assert_eq!(interfaces(&all, None, None)[0].text, "As installed");
    let set = interfaces(&all, Some("B"), None);
    assert_eq!(set.len(), 2);
    assert_eq!(index_of(&set, &Some("B".to_string())), 1);
    let gone = interfaces(&all, Some("Old"), None);
    assert_eq!(gone.len(), 3);
    assert_eq!(gone[2].text, "Old (not found)");
    assert_eq!(gone[2].value.as_deref(), Some("Old"));
}
