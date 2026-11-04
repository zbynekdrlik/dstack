use super::*;

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
        assert_eq!(config().block(b).device().unwrap().block, b);
    }
    // One problem for the block, also past the largest margin (W and the
    // split follow BLOCK then, dstack #68).
    for bad in [100, 5_000] {
        assert_eq!(
            config().block(bad).device().unwrap_err(),
            ["block: 16, 32, 64, 128 or 256 frames"]
        );
        assert_eq!(least_work_margin(48_000, bad), 128);
        assert_eq!(work_margin(BUDGET_NS, 48_000, bad), 192);
        assert_eq!(
            split(BUDGET_NS, 48_000, 48_000, bad, Directions::Both),
            split(BUDGET_NS, 48_000, 48_000, BLOCK, Directions::Both)
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

/// dstack #68, #1 (was half the latency, #90; the block and 2 ms in
/// v0.6.22): W's least is the pump's block and at least 2 ms, the normal
/// profile's least; W itself is 2/5 of the budget above that.
#[test]
fn the_pump_margin_is_the_block_and_at_least_2_ms() {
    // 2 ms at 48 kHz is 96 samples, under block 128.
    assert_eq!(least_work_margin(48_000, 128), 128);
    assert_eq!(least_work_margin(48_000, 64), 96);
    assert_eq!(least_work_margin(96_000, 128), 192);
    assert_eq!(least_work_margin(96_000, 256), 256);
    assert_eq!(least_work_margin(44_100, 16), 88);
    let small = config().block(32).device().unwrap();
    assert_eq!((small.block, small.work_margin), (32, 192));
    for b in dstack::stream::BLOCKS {
        for (budget, rate) in [
            (10_000_000, 44_100),
            (10_000_000, 48_000),
            (20_000_000, 96_000),
        ] {
            let cfg = config()
                .budget_ns(budget)
                .sample_rate(rate)
                .block(b)
                .device()
                .unwrap();
            assert!(cfg.work_margin >= cfg.block, "{b} {budget} {rate}");
            assert_eq!(
                cfg.work_margin,
                work_margin(budget, rate, b),
                "{b} {budget} {rate}"
            );
        }
    }
}

/// dstack #68, #1: the defaults' 10 ms is the whole path. At 48 kHz W
/// 192 (4 ms, 2/5 of the budget) leaves a Dante latency of 288 samples
/// (6 ms; 7.33 ms with W 128 in v0.6.22, 10 ms with W 5 ms on top before).
#[test]
fn the_defaults_are_a_10_ms_budget_normal_timing_and_a_share_as_pump_margin() {
    let cfg = config().device().unwrap();
    assert_eq!(cfg.latency_ns, 6_000_000);
    assert_eq!(dstack::clock::latency_samples(cfg.latency_ns, 48_000), 288);
    assert_eq!(cfg.timing, TimingProfile::Normal);
    assert_eq!(cfg.sample_rate, 48_000);
    assert_eq!(cfg.work_margin, 192);
    assert_eq!(BUDGET_NS, 10_000_000);
    assert_eq!(cfg.encoding, 24);
    assert_eq!(cfg.name, "app-link");
    assert_eq!(cfg.rx, ["In 1", "In 2"]);
    assert_eq!(cfg.tx, ["Out 1", "Out 2"]);
    assert_eq!(cfg.manufacturer, "dlink");
    assert_eq!(cfg.model, "dlink");
    // No CPU pinning: the app's threads stay where the OS puts them.
    assert_eq!(cfg.pump_cpu, None);
    assert_eq!(cfg.io_cpu, None);
}

/// dstack #68, #1: a 20 ms budget at 96 kHz less W 768 (8 ms, 2/5 of it:
/// W grows with the budget) is a 12 ms Dante latency (18 ms with W 2 ms in
/// v0.6.22; the latency itself, W half of it, before).
#[test]
fn the_app_s_rate_budget_and_product_are_taken_and_the_split_follows() {
    let cfg = config()
        .sample_rate(96_000)
        .budget_ns(20_000_000)
        .product("SongPlayer", "sp")
        .device()
        .unwrap();
    assert_eq!(cfg.sample_rate, 96_000);
    assert_eq!(cfg.latency_ns, 12_000_000);
    assert_eq!(cfg.work_margin, 768);
    assert_eq!(
        (cfg.manufacturer.as_str(), cfg.model.as_str()),
        ("SongPlayer", "sp")
    );
}

/// dstack #68: a budget that leaves under 4 ms of Dante latency (was a
/// latency under 4 ms) is listed first, with the device's own problems.
#[test]
fn a_budget_too_small_and_a_bad_name_are_refused_with_every_problem() {
    let problems = Config::new("-bad", "/tmp/x.toml")
        .tx(["Out"])
        .budget_ns(6_000_000)
        .device()
        .unwrap_err();
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(
        problems[0].starts_with("budget_ns: 6000000 leaves a Dante latency of 3333333 ns"),
        "{problems:?}"
    );
    assert!(problems[1].starts_with("name"), "{problems:?}");
    // Alone, it is the one problem.
    assert_eq!(config().budget_ns(6_000_000).device().unwrap_err().len(), 1);
}

#[test]
fn a_config_the_device_refuses_is_a_setup_error_before_anything_runs() {
    struct Silent;
    impl Process for Silent {
        fn process(&mut self, _block: &mut Block<'_>) {}
    }
    let bad = config().budget_ns(1_000_000);
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
            assert!(why.contains("budget_ns: 1000000 leaves"), "{why}");
        }
        other => panic!("{other:?}"),
    }
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
    // The pump's margin is what keeps the Dante latency at its 4 ms floor
    // (and a sample) after the converter's TX delay, under its 4 ms share
    // (dstack #68, #1): 303 samples (3.16 ms), P 385 samples.
    assert_eq!(cfg.work_margin, 303);
    assert_eq!(cfg.latency_ns, 4_010_416);
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
    // The range's ends convert. 8 kHz's filter delays more than a 10 ms
    // budget leaves room for (about 19 ms of work; dstack #68), so it takes
    // a larger budget.
    assert!(
        config()
            .sample_rate(384_000)
            .network_rate(96_000)
            .device()
            .is_ok()
    );
    let low = config().sample_rate(8_000).network_rate(96_000);
    assert!(
        low.device().unwrap_err()[0].starts_with("budget_ns: 10000000 leaves"),
        "{:?}",
        low.device()
    );
    let wide = low.budget_ns(30_000_000).device();
    assert!(wide.is_ok(), "{wide:?}");
}

/// dstack #68, the table in the crate docs: the Dante latency is the budget
/// less the larger direction's work, in whole samples, and each path stays
/// within the budget (and within a sample's worth of it on the larger
/// side).
#[test]
fn the_budget_splits_into_the_dante_latency_and_the_work() {
    let ms = |ns: u64| ns / 10_000;
    // (device, app, block) -> (P, W, TX path, RX path), in 10 us steps.
    for ((dev, app, block), want) in [
        ((48_000, 48_000, 128), (600, 192, 1_000, 866)),
        ((96_000, 96_000, 128), (600, 384, 1_000, 733)),
        ((96_000, 48_000, 128), (401, 303, 999, 667)),
        ((96_000, 44_100, 128), (401, 279, 999, 665)),
        ((48_000, 44_100, 128), (402, 139, 999, 934)),
        ((96_000, 48_000, 256), (401, 303, 999, 801)),
        ((48_000, 48_000, 256), (466, 256, 999, 999)),
        ((48_000, 96_000, 128), (458, 192, 999, 991)),
    ] {
        let s = split(BUDGET_NS, dev, app, block, Directions::Both).unwrap();
        assert_eq!(
            (
                ms(s.latency_ns),
                s.margin,
                ms(s.tx_path_ns),
                ms(s.rx_path_ns)
            ),
            want,
            "{dev} {app} {block}"
        );
        assert!(s.tx_path_ns <= BUDGET_NS && s.rx_path_ns <= BUDGET_NS);
        let sample = 1_000_000_000 / u64::from(dev) + 1;
        assert!(BUDGET_NS - s.tx_path_ns.max(s.rx_path_ns) <= sample);
        // Whole samples at the device's rate, read back as the RX reads it.
        let p = dstack::clock::latency_samples(s.latency_ns, dev);
        assert_eq!(p * 1_000_000_000 / u64::from(dev), s.latency_ns);
    }
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

/// dstack #68: the paths' parts. Without conversion TX is W + P and RX
/// P + B; a converter adds its own delay each way ([`rated::delay_ns`]).
#[test]
fn each_path_is_its_work_plus_the_dante_latency() {
    let ns = |samples: u64, rate: u64| samples * 1_000_000_000 / rate;
    let s = split(BUDGET_NS, 96_000, 96_000, 128, Directions::Both).unwrap();
    assert_eq!(s.tx_path_ns, ns(384, 96_000) + s.latency_ns);
    assert_eq!(s.rx_path_ns, ns(128, 96_000) + s.latency_ns);
    let (rx, tx) = rated::delay_ns(96_000, 48_000, 128).unwrap();
    let c = split(BUDGET_NS, 96_000, 48_000, 128, Directions::Both).unwrap();
    assert_eq!(c.tx_path_ns, ns(303, 96_000) + tx + c.latency_ns);
    assert_eq!(c.rx_path_ns, ns(128, 96_000) + rx + c.latency_ns);
    // A 96 kHz app on a 48 kHz device at 9.5 ms: W's share (182 samples,
    // 3.79 ms) and the converter's TX delay are less than the block and
    // its RX delay, so the RX side sets the Dante latency.
    let up = split(9_500_000, 48_000, 96_000, 128, Directions::Both).unwrap();
    assert_eq!(up.margin, 182);
    assert!(up.rx_path_ns > up.tx_path_ns, "{up:?}");
    assert_eq!(9_500_000 - up.rx_path_ns, 2);
}

/// dstack #68: the Dante latency's floor is the normal profile's 4 ms: a
/// budget that leaves exactly that is taken, a nanosecond less is refused.
#[test]
fn a_budget_that_leaves_under_4_ms_of_dante_latency_is_a_problem() {
    assert_eq!(MIN_LATENCY_NS, 4_000_000);
    // 48 kHz, block 128: the work is 128 samples, 2666666 ns.
    let s = split(6_666_666, 48_000, 48_000, 128, Directions::Both).unwrap();
    assert_eq!(s.latency_ns, 4_000_000);
    assert_eq!(
        split(6_666_665, 48_000, 48_000, 128, Directions::Both).unwrap_err(),
        "budget_ns: 6666665 leaves a Dante latency of 3979166 ns after 2666666 ns of work \
         (block, margin, rate conversion); at least 4000000"
    );
    // Under the work itself: nothing left.
    assert!(
        split(1_000_000, 48_000, 48_000, 128, Directions::Both)
            .unwrap_err()
            .contains("Dante latency of 0 ns")
    );
    // A large block with a conversion can reach the floor.
    assert!(split(BUDGET_NS, 48_000, 44_100, 256, Directions::Both).is_err());
}

/// dstack #68: a device rate of 0 and a rate pair the converter refuses are
/// the split's problems, never a panic.
#[test]
fn a_rate_the_split_cannot_use_is_a_problem() {
    assert_eq!(
        split(BUDGET_NS, 0, 48_000, 128, Directions::Both).unwrap_err(),
        "sample_rate: the device's rate is 0 Hz"
    );
    assert!(
        split(BUDGET_NS, 96_000, 0, 128, Directions::Both)
            .unwrap_err()
            .starts_with("rate conversion: ")
    );
}

/// dstack #68: `Config::split` is the split of the config's own budget,
/// rates and block, and `Config::device` writes its latency and margin.
#[test]
fn the_config_s_split_is_what_its_device_runs() {
    let c = config()
        .sample_rate(48_000)
        .network_rate(96_000)
        .block(64)
        .budget_ns(12_000_000);
    let s = c.split().unwrap();
    assert_eq!(
        s,
        split(12_000_000, 96_000, 48_000, 64, Directions::Both).unwrap()
    );
    let cfg = c.device().unwrap();
    assert_eq!((cfg.latency_ns, cfg.work_margin), (s.latency_ns, s.margin));
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

/// dstack #1 (the last item of the 4.11.2026 list): the Dante latency is
/// sized by the directions the device has. An RX-only 48 kHz app on a
/// 96 kHz device gets the budget less its RX work, 7.33 ms (it got 5.17 ms
/// from the converter's TX delay of a TX path it never runs); a TX-only one
/// gets the budget less its TX work; a device with both keeps the larger.
#[test]
fn the_dante_latency_follows_the_directions_the_device_has() {
    let ms = |ns: u64| ns / 10_000;
    let base = |c: Config| c.sample_rate(48_000).network_rate(96_000);
    let rx_only = base(Config::new("rx-only", "/tmp/never-written.toml").rx(["In 1"]));
    let s = rx_only.split().unwrap();
    assert_eq!((ms(s.latency_ns), ms(s.rx_path_ns)), (733, 999), "{s:?}");
    assert!(BUDGET_NS - s.rx_path_ns <= 1_000_000_000 / 96_000 + 1);
    let cfg = rx_only.device().unwrap();
    assert_eq!(cfg.latency_ns, s.latency_ns);
    // TX-only where RX is the larger work (a 96 kHz app on a 48 kHz
    // device, at 9.5 ms: W's share is under the block and the converter's
    // RX delay there): the TX path takes the whole budget. (The RX side
    // stopped being the larger at 10 ms once W took its share of the
    // budget, the second change on #1.)
    let budget = 9_500_000;
    let tx_only = Config::new("tx-only", "/tmp/never-written.toml")
        .tx(["Out 1"])
        .sample_rate(96_000)
        .network_rate(48_000)
        .budget_ns(budget);
    let t = tx_only.split().unwrap();
    assert!(t.tx_path_ns <= budget, "{t:?}");
    assert!(budget - t.tx_path_ns <= 1_000_000_000 / 48_000 + 1, "{t:?}");
    assert!(t.rx_path_ns > budget, "{t:?}");
    assert_eq!(tx_only.device().unwrap().latency_ns, t.latency_ns);
    // Both directions: the larger work (4.01 ms here: P's floor and a
    // sample, W gave way to keep it).
    let both = base(config()).split().unwrap();
    assert_eq!(ms(both.latency_ns), 401);
    assert!(both.tx_path_ns <= BUDGET_NS && both.rx_path_ns <= BUDGET_NS);
}

/// dstack #1, the one-direction table in the crate docs: P is the budget
/// less that direction's work alone, in whole samples, and that path stays
/// within a sample of the budget.
#[test]
fn a_one_direction_device_splits_by_that_direction_s_work() {
    use Directions::{Both, Rx, Tx};
    let ms = |ns: u64| ns / 10_000;
    // (device, app, directions) -> (P, TX path, RX path), in 10 us steps.
    for ((dev, app, dir), want) in [
        ((96_000, 96_000, Rx), (866, 1_266, 999)),
        ((96_000, 96_000, Tx), (600, 1_000, 733)),
        ((96_000, 48_000, Rx), (733, 1_416, 999)),
        ((96_000, 48_000, Tx), (401, 999, 667)),
        ((96_000, 44_100, Rx), (734, 1_442, 999)),
        ((48_000, 44_100, Rx), (466, 1_174, 998)),
        ((48_000, 96_000, Tx), (458, 999, 991)),
        ((48_000, 96_000, Rx), (466, 1_008, 999)),
        ((48_000, 48_000, Rx), (733, 1_133, 999)),
    ] {
        let s = split(BUDGET_NS, dev, app, 128, dir).unwrap();
        assert_eq!(
            (ms(s.latency_ns), ms(s.tx_path_ns), ms(s.rx_path_ns)),
            want,
            "{dev} {app} {dir:?}"
        );
        let path = if dir == Rx {
            s.rx_path_ns
        } else {
            s.tx_path_ns
        };
        assert!(path <= BUDGET_NS, "{dev} {app} {dir:?}");
        assert!(BUDGET_NS - path <= 1_000_000_000 / u64::from(dev) + 1);
        // Both takes the larger work: never above either direction's P.
        let both = split(BUDGET_NS, dev, app, 128, Both).unwrap();
        assert!(both.latency_ns <= s.latency_ns, "{dev} {app} {dir:?}");
    }
    // The floor holds for one direction too: 48 kHz, block 128, RX work
    // 128 samples (2666666 ns).
    let floor = split(6_666_666, 48_000, 48_000, 128, Rx).unwrap();
    assert_eq!(floor.latency_ns, MIN_LATENCY_NS);
    assert!(
        split(6_666_665, 48_000, 48_000, 128, Tx)
            .unwrap_err()
            .contains("at least 4000000")
    );
}

/// dstack #1: a config's directions come from its channels; one with
/// neither splits as both.
#[test]
fn the_config_s_directions_are_its_channels() {
    assert_eq!(Directions::of(true, true), Directions::Both);
    assert_eq!(Directions::of(true, false), Directions::Rx);
    assert_eq!(Directions::of(false, true), Directions::Tx);
    assert_eq!(Directions::of(false, false), Directions::Both);
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
        split(BUDGET_NS, 96_000, 48_000, 128, Directions::Rx).unwrap()
    );
    let cfg = c.device().unwrap();
    assert_eq!((cfg.latency_ns, cfg.work_margin), (s.latency_ns, s.margin));
}

/// dstack #1 (intercom's report against v0.6.22: W at 2 ms restarted the
/// pump on a loaded PC, and a 20 ms budget kept it at 2 ms): W is 2/5 of
/// the budget, at least the block and 2 ms, at most the stack's largest
/// margin. A device with TX gives W up to keep P's 4 ms floor; an RX-only
/// device keeps the whole share (W is not in its path).
#[test]
fn the_pump_margin_is_two_fifths_of_the_budget() {
    use Directions::{Both, Rx, Tx};
    assert_eq!(work_margin(10_000_000, 48_000, 128), 192);
    assert_eq!(work_margin(10_000_000, 96_000, 128), 384);
    assert_eq!(work_margin(20_000_000, 96_000, 128), 768);
    assert_eq!(work_margin(20_000_000, 48_000, 128), 384);
    // The block, and 2 ms, are its least.
    assert_eq!(work_margin(10_000_000, 48_000, 256), 256);
    assert_eq!(work_margin(5_000_000, 48_000, 64), 96);
    // The stack's largest margin is its most.
    assert_eq!(work_margin(100_000_000, 96_000, 128), 3_840);
    assert_eq!(work_margin(200_000_000, 96_000, 128), MAX_WORK_MARGIN);
    // With a converter the TX side gives W up for P's floor (P 4.01 ms:
    // 385 whole samples); RX only keeps the share.
    for dir in [Both, Tx] {
        let s = split(BUDGET_NS, 96_000, 48_000, 128, dir).unwrap();
        assert_eq!((s.margin, s.latency_ns), (303, 4_010_416), "{dir:?}");
    }
    let rx = split(BUDGET_NS, 96_000, 48_000, 128, Rx).unwrap();
    assert_eq!((rx.margin, rx.latency_ns), (384, 7_333_333));
    // A 20 ms budget: W grows to its 8 ms share, the converter's TX side
    // too.
    let wide = split(20_000_000, 96_000, 48_000, 128, Both).unwrap();
    assert_eq!(wide.margin, 768);
    // Under the floor even at W's least: W stays at its least (the block).
    let tight = split(6_666_666, 48_000, 48_000, 128, Both).unwrap();
    assert_eq!((tight.margin, tight.latency_ns), (128, 4_000_000));
    // A 44.1 kHz network with a 48 kHz app: P's floor is not whole samples
    // there, and the room's last sample keeps P at it or above.
    let odd = split(BUDGET_NS, 44_100, 48_000, 128, Both).unwrap();
    assert!(odd.latency_ns >= MIN_LATENCY_NS, "{odd:?}");
    assert!(odd.margin < work_margin(BUDGET_NS, 44_100, 128), "{odd:?}");
}

/// dstack #1 (intercom's report against v0.6.22): at the default 10 ms
/// budget a pump 3 ms late (over v0.6.22's 2 ms W) still runs its block, on
/// dlink's default device and on the devices intercom runs (a native 96 kHz
/// sender, a 48 kHz app on a 96 kHz device receiving, or with both
/// directions); a block more than W late starts the schedule again. The
/// pump's own schedule (`dstack::stream::Blocks`) at the split's timing,
/// with no clock and no threads, so it holds on any box.
#[test]
fn at_the_default_budget_a_pump_3_ms_late_still_runs_its_block() {
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
        let late = latency_samples(3_000_000, cfg.sample_rate);
        assert!(late > latency_samples(2_000_000, cfg.sample_rate));
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

/// dstack #1 (camera-box, 4.11.2026): an app places its device's pump, TX
/// and RX threads on CPUs of its own choosing; unset, they stay anywhere,
/// and a CPU past the stack's limit is refused with the other problems.
#[test]
fn an_app_places_its_threads_on_its_own_cpus() {
    let unset = config().device().expect("valid");
    assert_eq!(
        (unset.pump_cpu, unset.io_cpu, unset.rx_cpu),
        (None, None, None)
    );
    let set = config()
        .pump_cpu(14)
        .io_cpu(3)
        .rx_cpu(5)
        .device()
        .expect("valid");
    assert_eq!(
        (set.pump_cpu, set.io_cpu, set.rx_cpu, set.rx_thread_cpu()),
        (Some(14), Some(3), Some(5), Some(5))
    );
    let io_only = config().io_cpu(3).device().expect("valid");
    assert_eq!(io_only.rx_thread_cpu(), Some(3));
    let problems = config()
        .pump_cpu(dstack::device::MAX_CPU)
        .device()
        .expect_err("past the limit");
    assert!(
        problems.iter().any(|p| p.contains("pump_cpu")),
        "{problems:?}"
    );
}
