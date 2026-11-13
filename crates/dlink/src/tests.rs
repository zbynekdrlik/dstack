use super::*;

use dstack::clock::latency_samples;

mod start;
mod terms;

fn config() -> Config {
    Config::new("app-link", "/tmp/never-written.toml")
        .rx(["In 1", "In 2"])
        .tx(["Out 1", "Out 2"])
}

/// dstack #90: the pump runs block 128 unless the app sets another of
/// `stream::BLOCKS`; any other is refused with the other problems.
#[test]
fn the_block_is_128_unless_the_app_sets_another_of_the_stack_s_blocks() {
    assert_eq!(config().device().unwrap().block, BLOCK);
    assert_eq!(BLOCK, 128);
    for b in dstack::stream::BLOCKS {
        let at_96 = config().sample_rate(96_000).block(b);
        assert_eq!(at_96.device().unwrap().block, b);
    }
    // A 48 kHz device with TX at block 256 and the defaults: W is 6 ms,
    // over the block, and P keeps its v0.6.105 value.
    let big = config().block(256).device().unwrap();
    assert_eq!((big.block, big.work_margin), (256, 288));
    assert_eq!(big.latency_ns, 9_000_000);
    // One problem for the block (W and the split follow BLOCK then).
    for bad in [100, 5_000] {
        assert_eq!(
            config().block(bad).device().unwrap_err(),
            ["block: 16, 32, 64, 128 or 256 frames"]
        );
        assert_eq!(least_work_margin(48_000, bad), 128);
        assert_eq!(default_work_margin(48_000, bad, Directions::Both), 288);
        assert_eq!(
            split(Terms::default(), 48_000, 48_000, bad, Directions::Both),
            split(Terms::default(), 48_000, 48_000, BLOCK, Directions::Both)
        );
    }
    // Listed with the others, as a config file's would be.
    let problems = config().block(0).flow_slots(0).device().unwrap_err();
    assert_eq!(
        problems,
        [
            "block: 16, 32, 64, 128 or 256 frames",
            "flow_slots: 1 to 32"
        ]
    );
}

/// dstack #1, #244: W's least is the pump's block and at least 2 ms, the
/// normal profile's least; unset it is 6 ms with TX and 15 ms RX only.
#[test]
fn the_pump_margin_is_the_block_and_at_least_2_ms() {
    // 2 ms at 48 kHz is 96 samples, under block 128.
    assert_eq!(least_work_margin(48_000, 128), 128);
    assert_eq!(least_work_margin(48_000, 64), 96);
    assert_eq!(least_work_margin(96_000, 128), 192);
    assert_eq!(least_work_margin(96_000, 256), 256);
    assert_eq!(least_work_margin(44_100, 16), 88);
    // 6 ms, 288 samples at 48 kHz, whatever the block.
    let small = config().block(32).device().unwrap();
    assert_eq!((small.block, small.work_margin), (32, 288));
    for b in dstack::stream::BLOCKS {
        for rate in [44_100, 48_000, 96_000] {
            let cfg = config().sample_rate(rate).block(b).device().unwrap();
            assert!(cfg.work_margin >= cfg.block, "{b} {rate}");
            assert_eq!(
                cfg.work_margin,
                default_work_margin(rate, b, Directions::Both),
                "{b} {rate}"
            );
        }
    }
}

/// dstack #244: the defaults keep v0.6.105's device. At 48 kHz with both
/// directions W 288 (6 ms) and P 432 samples (9 ms): (a) 1 ms + (b) 8 ms.
#[test]
fn the_defaults_are_a_1_ms_network_latency_normal_timing_and_a_6_ms_margin() {
    assert_eq!(NETWORK_LATENCY_NS, 1_000_000);
    assert_eq!(REFERENCE_PATH_NS, 15_000_000);
    assert_eq!(WORK_MARGIN_NS, 6_000_000);
    assert_eq!(RX_WORK_MARGIN_NS, 15_000_000);
    let cfg = config().device().unwrap();
    assert_eq!(cfg.latency_ns, 9_000_000);
    assert_eq!(dstack::clock::latency_samples(cfg.latency_ns, 48_000), 432);
    assert_eq!(cfg.timing, TimingProfile::Normal);
    assert_eq!(cfg.sample_rate, 48_000);
    assert_eq!(cfg.work_margin, 288);
    let s = config().split().unwrap();
    assert_eq!(
        (s.network_latency_ns, s.receive_buffer_ns, s.latency_ns),
        (1_000_000, 8_000_000, 9_000_000)
    );
    assert_eq!(config().terms(), Terms::default());
    assert_eq!(cfg.encoding, 24);
    assert_eq!(cfg.name, "app-link");
    assert_eq!(cfg.rx, ["In 1", "In 2"]);
    assert_eq!(cfg.tx, ["Out 1", "Out 2"]);
    assert_eq!(cfg.manufacturer, "dlink");
    assert_eq!(cfg.model, "dlink");
    // No CPU pinning: the app's threads stay where the OS puts them.
    assert_eq!(cfg.pump_cpu, None);
    assert_eq!(cfg.io_cpu, None);
    assert_eq!(cfg.rx_cpu, None);
}

/// dstack #244: the app's rate, terms and product are taken; P is (a) + (b).
#[test]
fn the_app_s_rate_terms_and_product_are_taken() {
    let c = config()
        .sample_rate(96_000)
        .network_latency_ns(1_000_000)
        .receive_buffer_ns(11_000_000)
        .work_margin_ns(8_000_000)
        .product("SongPlayer", "sp");
    assert_eq!(
        c.terms(),
        Terms {
            network_latency_ns: Some(1_000_000),
            receive_buffer_ns: Some(11_000_000),
            work_margin_ns: Some(8_000_000),
        }
    );
    let cfg = c.device().unwrap();
    assert_eq!(cfg.sample_rate, 96_000);
    assert_eq!(cfg.latency_ns, 12_000_000);
    assert_eq!(cfg.work_margin, 768);
    assert_eq!(
        (cfg.manufacturer.as_str(), cfg.model.as_str()),
        ("SongPlayer", "sp")
    );
}

/// dstack #244: (a) + (b) under P's floor is listed first, with the
/// device's own problems.
#[test]
fn a_latency_under_its_floor_and_a_bad_name_are_refused_with_every_problem() {
    let problems = Config::new("-bad", "/tmp/x.toml")
        .tx(["Out"])
        .receive_buffer_ns(2_000_000)
        .device()
        .unwrap_err();
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert_eq!(
        problems[0],
        "network_latency_ns + receive_buffer_ns: 1000000 + 2000000 = 3000000 ns of Dante \
         latency; at least 4916666 (a TX packet's own age, dlink::tx_floor_ns)"
    );
    assert!(problems[1].starts_with("name"), "{problems:?}");
    // Alone, it is the one problem.
    assert_eq!(
        config()
            .receive_buffer_ns(2_000_000)
            .device()
            .unwrap_err()
            .len(),
        1
    );
}

#[test]
fn a_config_the_device_refuses_is_a_setup_error_before_anything_runs() {
    struct Silent;
    impl Process for Silent {
        fn process(&mut self, _block: &mut Block<'_>) {}
    }
    let bad = config().receive_buffer_ns(0);
    let e = Link::start_on(
        &bad,
        &Place::Ip(std::net::Ipv4Addr::new(192, 0, 2, 1)),
        Map {
            rx: vec![],
            tx: vec![],
        },
        Silent,
        Clock::System,
    )
    .err()
    .expect("refused");
    match e {
        HostError::Setup(why) => {
            assert!(why.starts_with("dlink config: "), "{why}");
            assert!(
                why.contains("network_latency_ns + receive_buffer_ns: 1000000 + 0 ="),
                "{why}"
            );
        }
        other => panic!("{other:?}"),
    }
}

/// dstack #68: a device rate of 0 and a rate pair the converter refuses are
/// the split's problems, never a panic.
#[test]
fn a_rate_the_split_cannot_use_is_a_problem() {
    assert_eq!(
        split(Terms::default(), 0, 48_000, 128, Directions::Both).unwrap_err(),
        "sample_rate: the device's rate is 0 Hz"
    );
    assert!(
        split(Terms::default(), 96_000, 0, 128, Directions::Both)
            .unwrap_err()
            .starts_with("rate conversion: ")
    );
}

/// dstack #244: `Config::split` is the split of the config's own terms,
/// rates and block, and `Config::device` writes its latency and margin.
#[test]
fn the_config_s_split_is_what_its_device_runs() {
    let c = config()
        .sample_rate(48_000)
        .network_rate(96_000)
        .block(64)
        .network_latency_ns(500_000)
        .receive_buffer_ns(5_000_000);
    let s = c.split().unwrap();
    assert_eq!(
        s,
        split(c.terms(), 96_000, 48_000, 64, Directions::Both).unwrap()
    );
    assert_eq!(s.latency_ns, 5_500_000);
    let cfg = c.device().unwrap();
    assert_eq!((cfg.latency_ns, cfg.work_margin), (s.latency_ns, s.margin));
}

/// dstack #1: a config's directions come from its channels; one with
/// neither counts as both.
#[test]
fn the_config_s_directions_are_its_channels() {
    assert_eq!(Directions::of(true, true), Directions::Both);
    assert_eq!(Directions::of(true, false), Directions::Rx);
    assert_eq!(Directions::of(false, true), Directions::Tx);
    assert_eq!(Directions::of(false, false), Directions::Both);
    assert!(Directions::Both.has_rx() && Directions::Both.has_tx());
    assert!(Directions::Rx.has_rx() && !Directions::Rx.has_tx());
    assert!(!Directions::Tx.has_rx() && Directions::Tx.has_tx());
    let named = |rx: &[&str], tx: &[&str]| {
        Config::new("dirs", "/tmp/never-written.toml")
            .rx(rx.iter().copied())
            .tx(tx.iter().copied())
    };
    assert_eq!(config().directions(), Directions::Both);
    assert_eq!(named(&["In"], &[]).directions(), Directions::Rx);
    assert_eq!(named(&[], &["Out"]).directions(), Directions::Tx);
    assert_eq!(named(&[], &[]).directions(), Directions::Both);
    // Config::split and Config::device run that split.
    let c = named(&["In"], &[]).network_rate(96_000);
    let s = c.split().unwrap();
    assert_eq!(
        s,
        split(Terms::default(), 96_000, 48_000, 128, Directions::Rx).unwrap()
    );
    let cfg = c.device().unwrap();
    assert_eq!((cfg.latency_ns, cfg.work_margin), (s.latency_ns, s.margin));
}

#[test]
fn a_second_device_s_ports_process_id_and_rx_port_base_are_taken() {
    let cfg = config()
        .ports(Ports {
            arc: 4441,
            cmc: 8801,
            flows: 4456,
        })
        .process_id(1)
        .rx_port_base(40_000)
        .device()
        .unwrap();
    assert_eq!(
        (cfg.arc_port, cfg.cmc_port, cfg.flows_port),
        (4441, 8801, 4456)
    );
    assert_eq!(cfg.process_id, 1);
    assert_eq!(cfg.rx_port_base, Some(40_000));
    // Unset, they are Dante's defaults and the OS's RX ports.
    let plain = config().device().unwrap();
    assert_eq!(
        (plain.arc_port, plain.cmc_port, plain.flows_port),
        (4440, 8800, 4455)
    );
    assert_eq!(plain.process_id, 0);
    assert_eq!(plain.rx_port_base, None);
    // A base whose 32 ports collide with our own control ports is refused.
    let clash = config().rx_port_base(4440).device().unwrap_err();
    assert!(
        clash.iter().any(|p| p.starts_with("rx_port_base")),
        "{clash:?}"
    );
}

#[test]
fn declared_subscriptions_reach_the_device_config_in_order() {
    let cfg = config()
        .subscribe(2, "Master", "foh-mix")
        .subscribe(1, "Click", "foh-mix")
        .device()
        .unwrap();
    let got: Vec<(usize, &str, &str)> = cfg
        .subscribe
        .iter()
        .map(|d| (d.rx, d.tx_channel.as_str(), d.tx_device.as_str()))
        .collect();
    assert_eq!(got, [(2, "Master", "foh-mix"), (1, "Click", "foh-mix")]);
    assert!(config().device().unwrap().subscribe.is_empty());
    // Checked as a config file's would be.
    let problems = config()
        .subscribe(3, "Master", "foh-mix")
        .device()
        .unwrap_err();
    assert_eq!(problems, ["subscribe: rx 3 is not an RX channel (1 to 2)"]);
}

#[test]
fn the_flow_slots_are_taken_and_checked() {
    assert_eq!(config().device().unwrap().flow_slots, 32);
    assert_eq!(config().flow_slots(2).device().unwrap().flow_slots, 2);
    assert_eq!(
        config().flow_slots(0).device().unwrap_err(),
        ["flow_slots: 1 to 32"]
    );
}

#[test]
fn a_network_rate_runs_the_device_at_it_and_the_app_at_its_own() {
    let c = config().sample_rate(48_000).network_rate(96_000);
    assert_eq!((c.device_rate(), c.converts()), (96_000, true));
    let cfg = c.device().unwrap();
    assert_eq!(cfg.sample_rate, 96_000);
    // The defaults: W 6 ms (576 samples) and P 803 samples (8.36 ms, as in
    // v0.6.105), over a TX packet's own age of 4.58 ms (dstack #1, #244).
    assert_eq!(cfg.work_margin, 576);
    assert_eq!(cfg.latency_ns, 8_364_583);
    assert!(cfg.latency_ns >= 4_583_333);
    // 44.1 kHz on a 96 kHz network converts too (any ratio).
    let cd = config().sample_rate(44_100).network_rate(96_000);
    assert_eq!((cd.device_rate(), cd.converts()), (96_000, true));
    assert_eq!(cd.device().unwrap().sample_rate, 96_000);
    // The same rate is no conversion; unset is the app's rate.
    let same = config().sample_rate(48_000).network_rate(48_000);
    assert_eq!((same.device_rate(), same.converts()), (48_000, false));
    assert_eq!(same.device().unwrap().sample_rate, 48_000);
    assert_eq!(config().device_rate(), SAMPLE_RATE);
    assert!(!config().converts());
}

#[test]
fn an_app_rate_outside_the_range_is_refused() {
    for app in [0, 7_999, 384_001] {
        let problems = config()
            .sample_rate(app)
            .network_rate(96_000)
            .device()
            .unwrap_err();
        assert_eq!(
            problems,
            vec![format!(
                "sample_rate {app} must be between 8000 and 384000 Hz"
            )]
        );
    }
    // The range's ends convert. 8 kHz's filters delay more than the old
    // 15 ms budget held (about 19 ms of work; refused up to v0.6.105): its
    // default P is now the TX floor, 440 samples at 96 kHz (dstack #244).
    assert!(
        config()
            .sample_rate(384_000)
            .network_rate(96_000)
            .device()
            .is_ok()
    );
    let low = config().sample_rate(8_000).network_rate(96_000);
    let s = low.split().unwrap();
    assert_eq!(s.latency_ns, tx_floor_ns(96_000));
    assert_eq!(s.latency_ns, 4_583_333);
    assert_eq!(low.device().unwrap().latency_ns, 4_583_333);
}

/// A host sender that is never called (the config only carries it).
struct Never;

impl FrameSender for Never {
    fn send(&self, _: &[&[u8]], _: &mut [bool]) -> std::io::Result<()> {
        unreachable!("a config never sends")
    }
}

/// dstack #1: the app's frame sender rides the config to the device; two
/// configs are equal with the same sender (or none), not with another.
#[test]
fn the_config_carries_the_app_s_frame_sender() {
    assert!(config().frame_sender.0.is_none());
    let sender: Arc<dyn FrameSender> = Arc::new(Never);
    let with = config().frame_sender(Arc::clone(&sender));
    assert!(Arc::ptr_eq(with.frame_sender.0.as_ref().unwrap(), &sender));
    assert_eq!(with, config().frame_sender(Arc::clone(&sender)));
    assert_ne!(with, config().frame_sender(Arc::new(Never)));
    assert_ne!(with, config());
    assert_ne!(config(), with);
    assert_eq!(config(), config());
    assert_eq!(format!("{:?}", with.frame_sender), "Some(..)");
    assert_eq!(format!("{:?}", config().frame_sender), "None");
    // The device config is the same either way.
    assert_eq!(with.device().unwrap(), config().device().unwrap());
}

/// dstack #68 (review): a network rate of 0 is one problem, the device's
/// own sample_rate one, not the split's too; with a bad name beside it,
/// both are listed.
#[test]
fn a_network_rate_of_0_is_the_device_s_one_problem() {
    let problems = config().network_rate(0).device().unwrap_err();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].starts_with("sample_rate"), "{problems:?}");
    assert!(
        !problems[0].contains("the device's rate is 0 Hz"),
        "{problems:?}"
    );
    let two = Config::new("-bad", "/tmp/x.toml")
        .network_rate(0)
        .device()
        .unwrap_err();
    assert_eq!(two.len(), 2, "{two:?}");
}

/// dstack #1 (intercom's report against v0.6.22; the owner's 15 ms of
/// 4.11.2026): at the default W of 6 ms a pump 5 ms late (over the
/// 2.7 to 3.6 ms preemptions seen on loaded PCs with no CPU pinning)
/// still runs its block, on
/// dlink's default device and on the devices intercom runs (a native 96 kHz
/// sender, a 48 kHz app on a 96 kHz device receiving, or with both
/// directions); a block more than W late starts the schedule again. The
/// pump's own schedule (`dstack::stream::Blocks`) at the split's timing,
/// with no clock and no threads, so it holds on any box.
#[test]
fn at_the_default_margin_a_pump_5_ms_late_still_runs_its_block() {
    use dstack::stream::{Blocks, Due, Timing};
    let one = |name: &str| Config::new(name, "/tmp/never-written.toml");
    for c in [
        config(),
        one("tx-96").tx(["Out"]).sample_rate(96_000),
        one("rx-96-48")
            .rx(["In"])
            .sample_rate(48_000)
            .network_rate(96_000),
        config().sample_rate(48_000).network_rate(96_000),
    ] {
        let cfg = c.device().unwrap();
        let t = Timing::of(&cfg);
        let late = latency_samples(5_000_000, cfg.sample_rate);
        assert!(late > latency_samples(3_600_000, cfg.sample_rate));
        let due = Blocks::bootstrap(0, t).due();
        for (behind, runs) in [(late, true), (t.margin, true), (t.margin + 1, false)] {
            let mut blocks = Blocks::bootstrap(0, t);
            let got = blocks.next(due + behind);
            assert_eq!(
                matches!(got, Due::Run { .. }),
                runs,
                "{} at {behind} behind: {got:?} ({t:?})",
                cfg.name
            );
        }
    }
}
