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
    // One problem for the block, also past the largest margin (W stays half
    // the latency).
    for bad in [100, 5_000] {
        assert_eq!(
            config().block(bad).device().unwrap_err(),
            ["block: 16, 32, 64, 128 or 256 frames"]
        );
        assert_eq!(work_margin(10_000_000, 48_000, bad), 240);
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

/// dstack #90: W is half the latency, and never under the pump's block
/// (4 ms at 48 kHz is 96 samples, under block 128).
#[test]
fn the_pump_margin_is_half_the_latency_and_at_least_the_block() {
    assert_eq!(work_margin(10_000_000, 48_000, 128), 240);
    assert_eq!(work_margin(4_000_000, 48_000, 128), 128);
    assert_eq!(work_margin(4_000_000, 48_000, 64), 96);
    assert_eq!(work_margin(4_000_000, 96_000, 128), 192);
    let low = config().latency_ns(4_000_000).device().unwrap();
    assert_eq!((low.block, low.work_margin), (128, 128));
    let small = config().latency_ns(4_000_000).block(32).device().unwrap();
    assert_eq!((small.block, small.work_margin), (32, 96));
    for b in dstack::stream::BLOCKS {
        for (latency, rate) in [
            (4_000_000, 44_100),
            (4_000_000, 48_000),
            (10_000_000, 96_000),
        ] {
            let cfg = config()
                .latency_ns(latency)
                .sample_rate(rate)
                .block(b)
                .device()
                .unwrap();
            assert!(cfg.work_margin >= cfg.block, "{b} {latency} {rate}");
        }
    }
}

#[test]
fn the_defaults_are_10_ms_normal_timing_and_half_the_latency_as_pump_margin() {
    let cfg = config().device().unwrap();
    assert_eq!(cfg.latency_ns, 10_000_000);
    assert_eq!(cfg.timing, TimingProfile::Normal);
    assert_eq!(cfg.sample_rate, 48_000);
    // 5 ms at 48 kHz.
    assert_eq!(cfg.work_margin, 240);
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

#[test]
fn the_app_s_rate_latency_and_product_are_taken_and_the_margin_follows() {
    let cfg = config()
        .sample_rate(96_000)
        .latency_ns(20_000_000)
        .product("SongPlayer", "sp")
        .device()
        .unwrap();
    assert_eq!(cfg.sample_rate, 96_000);
    assert_eq!(cfg.latency_ns, 20_000_000);
    assert_eq!(cfg.work_margin, 960);
    assert_eq!(
        (cfg.manufacturer.as_str(), cfg.model.as_str()),
        ("SongPlayer", "sp")
    );
}

#[test]
fn a_latency_under_4_ms_and_a_bad_name_are_refused_with_every_problem() {
    let problems = Config::new("-bad", "/tmp/x.toml")
        .tx(["Out"])
        .latency_ns(3_000_000)
        .device()
        .unwrap_err();
    assert!(
        problems
            .iter()
            .any(|p| p.starts_with("latency_ns: at least 4000000")),
        "{problems:?}"
    );
    assert!(
        problems.iter().any(|p| p.starts_with("name")),
        "{problems:?}"
    );
}

#[test]
fn a_config_the_device_refuses_is_a_setup_error_before_anything_runs() {
    struct Silent;
    impl Process for Silent {
        fn process(&mut self, _block: &mut Block<'_>) {}
    }
    let bad = config().latency_ns(1_000_000);
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
            assert!(why.contains("latency_ns: at least 4000000"), "{why}");
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
    // The pump's margin is half the latency at the device's rate.
    assert_eq!(cfg.work_margin, 480);
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
    for app in [8_000, 384_000] {
        assert!(
            config()
                .sample_rate(app)
                .network_rate(96_000)
                .device()
                .is_ok()
        );
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
